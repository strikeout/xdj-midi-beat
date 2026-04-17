//! MIDI Timecode (MTC) generator — quarter-frame and full-frame messages.
//!
//! Derives HH:MM:SS:FF from wall-clock elapsed time since playback started,
//! then streams MTC quarter-frame messages (0xF1) at the correct rate for the
//! configured frame rate.  Sends a full-frame SysEx on start and on seek
//! (large timecode jumps).
//!
//! Reference: MIDI 1.0 Detailed Specification — MTC Quarter Frame (RP-004).
//!
//! Timing: 8 quarter-frame messages encode one complete timecode over 2 frame
//! periods.  Interval between QF messages = 1 / (fps × 4).

use std::sync::Arc;
use std::time::{Duration, Instant};

use midir::MidiOutputConnection;
use parking_lot::Mutex;

use crate::config::{MtcFrameRate, SharedConfig};
use crate::state::SharedState;
use crate::tui::state::MidiActivity;

// ── Timecode representation ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timecode {
    hours: u8,
    minutes: u8,
    seconds: u8,
    frames: u8,
}

impl Timecode {
    fn zero() -> Self {
        Self {
            hours: 0,
            minutes: 0,
            seconds: 0,
            frames: 0,
        }
    }

    /// Derive timecode from wall-clock elapsed seconds at the given frame rate.
    fn from_elapsed(elapsed_secs: f64, fps: u8) -> Self {
        if elapsed_secs < 0.0 {
            return Self::zero();
        }
        let total_frames = (elapsed_secs * fps as f64) as u64;

        let frames = (total_frames % fps as u64) as u8;
        let total_secs = total_frames / fps as u64;
        let seconds = (total_secs % 60) as u8;
        let minutes = ((total_secs / 60) % 60) as u8;
        let hours = ((total_secs / 3600) % 24) as u8;

        Self {
            hours,
            minutes,
            seconds,
            frames,
        }
    }

    /// Total frame count for comparison / seek detection.
    fn total_frames(&self, fps: u8) -> u64 {
        let fps = fps as u64;
        self.hours as u64 * 3600 * fps
            + self.minutes as u64 * 60 * fps
            + self.seconds as u64 * fps
            + self.frames as u64
    }

    /// Increment by one frame, wrapping at 24h.
    fn increment(&mut self, fps: u8) {
        self.frames += 1;
        if self.frames >= fps {
            self.frames = 0;
            self.seconds += 1;
        }
        if self.seconds >= 60 {
            self.seconds = 0;
            self.minutes += 1;
        }
        if self.minutes >= 60 {
            self.minutes = 0;
            self.hours += 1;
        }
        if self.hours >= 24 {
            self.hours = 0;
        }
    }
}

impl std::fmt::Display for Timecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}:{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

// ── MTC state ─────────────────────────────────────────────────────────────────

struct MtcState {
    /// The timecode being transmitted in the current 8-QF cycle.
    cycle_tc: Timecode,
    /// Quarter-frame piece index (0–7).
    qf_index: u8,
    /// Instant of the last QF send.
    last_qf_time: Instant,
    /// Whether MTC is currently running (master is playing).
    running: bool,
    /// Wall-clock instant when playback started (for elapsed time derivation).
    play_start: Instant,
    /// Last known enabled state (for detecting runtime toggles).
    last_enabled: bool,
    start_debounce_until: Option<Instant>,
}

impl MtcState {
    fn new() -> Self {
        Self {
            cycle_tc: Timecode::zero(),
            qf_index: 0,
            last_qf_time: Instant::now(),
            running: false,
            play_start: Instant::now(),
            last_enabled: false,
            start_debounce_until: None,
        }
    }

    /// Elapsed seconds since playback started.
    fn elapsed_secs(&self) -> f64 {
        self.play_start.elapsed().as_secs_f64()
    }
}

// ── Quarter-frame encoding ────────────────────────────────────────────────────

/// Build the data byte for a quarter-frame message.
///
/// Format: [PPP VVVV] where PPP = piece type (0–7), VVVV = 4-bit value.
fn quarter_frame_data(tc: &Timecode, piece: u8, rate: &MtcFrameRate) -> u8 {
    let value = match piece {
        0 => tc.frames & 0x0F,
        1 => (tc.frames >> 4) & 0x01,
        2 => tc.seconds & 0x0F,
        3 => (tc.seconds >> 4) & 0x03,
        4 => tc.minutes & 0x0F,
        5 => (tc.minutes >> 4) & 0x03,
        6 => tc.hours & 0x0F,
        7 => ((tc.hours >> 4) & 0x01) | (rate.rate_code() << 1),
        _ => 0,
    };
    (piece << 4) | (value & 0x0F)
}

/// Build a full-frame SysEx message: F0 7F 7F 01 01 rh mm ss ff F7.
fn full_frame_sysex(tc: &Timecode, rate: &MtcFrameRate) -> [u8; 10] {
    let rh = (rate.rate_code() << 5) | (tc.hours & 0x1F);
    [
        0xF0, 0x7F, 0x7F, 0x01, 0x01, rh, tc.minutes, tc.seconds, tc.frames, 0xF7,
    ]
}

// ── Main MTC task ─────────────────────────────────────────────────────────────

pub async fn run(
    conn: Arc<Mutex<Option<MidiOutputConnection>>>,
    state: SharedState,
    cfg: SharedConfig,
    activity: Arc<Mutex<MidiActivity>>,
) {
    let mut ms = MtcState::new();

    tracing::info!("MTC timecode task started");

    loop {
        let (enabled, frame_rate) = {
            let cfg_r = cfg.read();
            (cfg_r.midi.mtc.enabled, cfg_r.midi.mtc.frame_rate)
        };

        // Handle enable/disable transitions.
        if enabled != ms.last_enabled {
            ms.last_enabled = enabled;
            if !enabled && ms.running {
                ms.running = false;
                tracing::info!("MTC disabled at runtime");
            } else if enabled {
                tracing::info!("MTC enabled at runtime");
            }
        }

        if !enabled {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }

        let fps = frame_rate.fps();

        // Quarter-frame interval: 1 frame time / 4.
        // 8 QF messages span 2 frames → 4 QF messages per frame → interval = 1/(fps*4).
        let qf_interval_us = 1_000_000.0 / (fps as f64 * 4.0);
        let qf_interval = Duration::from_micros(qf_interval_us as u64);

        // Read master state.
        let (bpm, is_playing) = {
            let st = state.read();
            (st.master.bpm, st.master.is_playing)
        };

        // Start/stop transitions.
        if is_playing && bpm > 0.0 && !ms.running {
            ms.running = true;
            ms.qf_index = 0;
            ms.play_start = Instant::now();
            ms.last_qf_time = Instant::now();
            ms.start_debounce_until = Some(Instant::now() + Duration::from_millis(100));

            // Send full-frame at timecode 00:00:00:00 on start.
            let tc = Timecode::zero();
            ms.cycle_tc = tc;

            let sysex = full_frame_sysex(&tc, &frame_rate);
            let mut c = conn.lock();
            if let Some(ref mut c) = *c {
                let _ = c.send(&sysex);
            }
            tracing::debug!(timecode = %tc, "MTC started — full frame sent");
        } else if (!is_playing || bpm <= 0.0) && ms.running {
            let can_stop = ms.start_debounce_until
                .map(|until| Instant::now() > until)
                .unwrap_or(true);
            if can_stop || bpm <= 0.0 {
                ms.running = false;
                ms.start_debounce_until = None;
                tracing::debug!("MTC stopped — master not playing");
            }
        }

        if !ms.running {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }

        // ── Deadline-based sleep until next QF ──────────────────────────────
        let now = Instant::now();
        let elapsed = now.duration_since(ms.last_qf_time);
        if elapsed < qf_interval {
            let remaining = qf_interval - elapsed;
            tokio::time::sleep(remaining).await;
        }

        // ── At the start of each 8-QF cycle (piece 0), compute timecode ─────
        if ms.qf_index == 0 {
            let target_tc = Timecode::from_elapsed(ms.elapsed_secs(), fps);

            // Detect seek: if timecode jumped more than 4 frames, resync.
            // (Can happen if the task was delayed by OS scheduling.)
            let diff = (target_tc.total_frames(fps) as i64
                - ms.cycle_tc.total_frames(fps) as i64)
                .unsigned_abs();
            if diff > 4 {
                ms.cycle_tc = target_tc;
                // Send full-frame to resync receivers.
                let sysex = full_frame_sysex(&target_tc, &frame_rate);
                let mut c = conn.lock();
                if let Some(ref mut c) = *c {
                    let _ = c.send(&sysex);
                }
                tracing::debug!(timecode = %target_tc, "MTC drift detected — full frame resync");
            } else {
                // Normal: auto-increment by 2 frames (one QF cycle = 2 frames).
                ms.cycle_tc.increment(fps);
                ms.cycle_tc.increment(fps);
            }
        }

        // ── Send quarter-frame message ──────────────────────────────────────
        let data_byte = quarter_frame_data(&ms.cycle_tc, ms.qf_index, &frame_rate);
        {
            let mut c = conn.lock();
            if let Some(ref mut c) = *c {
                let _ = c.send(&[0xF1, data_byte]);
            }
        }
        activity.lock().clock_pulses += 1; // reuse counter for MTC activity

        ms.qf_index = (ms.qf_index + 1) % 8;
        ms.last_qf_time = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MtcFrameRate;

    #[test]
    fn timecode_from_elapsed_and_total_frames_roundtrip() {
        let tc = Timecode::from_elapsed(3661.5, 30);
        assert_eq!(tc.hours, 1);
        assert_eq!(tc.minutes, 1);
        assert_eq!(tc.seconds, 1);
        assert_eq!(tc.frames, 15);
        assert_eq!(tc.total_frames(30), 109_845);
    }

    #[test]
    fn timecode_increment_wraps_all_fields() {
        let mut tc = Timecode {
            hours: 23,
            minutes: 59,
            seconds: 59,
            frames: 29,
        };
        tc.increment(30);
        assert_eq!(tc.hours, 0);
        assert_eq!(tc.minutes, 0);
        assert_eq!(tc.seconds, 0);
        assert_eq!(tc.frames, 0);
    }

    #[test]
    fn quarter_frame_piece_encoding_is_correct() {
        let tc = Timecode {
            hours: 21,
            minutes: 43,
            seconds: 59,
            frames: 27,
        };
        let r = MtcFrameRate::Fps25;

        assert_eq!(quarter_frame_data(&tc, 0, &r), 0x0B);
        assert_eq!(quarter_frame_data(&tc, 1, &r), 0x11);
        assert_eq!(quarter_frame_data(&tc, 2, &r), 0x2B);
        assert_eq!(quarter_frame_data(&tc, 3, &r), 0x33);
        assert_eq!(quarter_frame_data(&tc, 4, &r), 0x4B);
        assert_eq!(quarter_frame_data(&tc, 5, &r), 0x52);
        assert_eq!(quarter_frame_data(&tc, 6, &r), 0x65);
        assert_eq!(quarter_frame_data(&tc, 7, &r), 0x73);
    }

    #[test]
    fn full_frame_sysex_contains_rate_and_time_fields() {
        let tc = Timecode {
            hours: 10,
            minutes: 11,
            seconds: 12,
            frames: 13,
        };
        let msg = full_frame_sysex(&tc, &MtcFrameRate::Fps30);
        assert_eq!(msg, [0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x6A, 11, 12, 13, 0xF7]);
    }
}

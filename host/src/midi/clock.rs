//! MIDI Clock generator — beat-synced 24 pulses-per-quarter-note.
//!
//! Generates MIDI Timing Clock (0xF8) messages driven directly by beat
//! events from Pro DJ Link or Ableton Link. On each beat, calculates the
//! beat interval and sends 24 pulses distributed evenly until the next beat.
//!
//! Also sends MIDI Start (0xFA), Stop (0xFC), and Continue (0xFB)
//! messages when the master deck starts or stops.
//!
//! This replaces the previous timer-based approach which computed deadlines from BPM.
//! Now beat events drive the clock: each beat schedules 24 pulses evenly
//! distributed through the beat interval.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio::time;


use crate::midi::MidiTransport;

use crate::config::SharedConfig;
use crate::prolink::beat_listener::BeatEvent;
use crate::state::SharedState;
use crate::tui::state::MidiActivity;

// ── MIDI byte constants ───────────────────────────────────────────────────────

const MSG_CLOCK: u8 = 0xF8;
const MSG_START: u8 = 0xFA;
const MSG_STOP: u8 = 0xFC;

/// Pulses per quarter note — MIDI spec, always 24.
const PPQ: u64 = 24;

// ── Beat-synced Clock state ─────────────────────────────────────────────────

struct ClockState {
    /// Current BPM (from beat events).
    bpm: f64,
    /// Beat interval in milliseconds (time between beats = 60000/BPM).
    beat_interval_ms: u64,
    /// Current pulse index within the beat (0–23).
    pulse_index: u64,
    /// Whether we are currently in the "running" state.
    running: bool,
    /// Whether the clock has been started at least once (for Continue vs Start).
    has_started: bool,
    /// Whether we're waiting for a downbeat before sending clock pulses.
    waiting_for_downbeat: bool,
}

impl ClockState {
    fn new() -> Self {
        Self {
            bpm: 0.0,
            beat_interval_ms: 500,
            pulse_index: 0,
            running: false,
            has_started: false,
            waiting_for_downbeat: true,
        }
    }

    fn set_bpm(&mut self, bpm: f64) {
        if bpm > 0.0 {
            self.bpm = bpm;
            self.beat_interval_ms = (60000.0 / bpm) as u64;
        }
    }
}

// ── Pulse scheduling ──────────────────────────────────────────────────────────

/// Send 24 clock pulses distributed evenly through the beat interval.
/// Uses non-blocking tokio timers with exact timing from BeatPacket.
/// This is an async function that schedules pulses at precise intervals.
async fn schedule_pulses(
    cs: &mut ClockState,
    midi: &Arc<dyn MidiTransport>,
    activity: &Arc<Mutex<MidiActivity>>,
    next_beat_ms: u32,
    beat_in_bar: u8,
) {
    let pulse_interval_ms = next_beat_ms as u64 / PPQ;
    let pulse_interval = Duration::from_millis(pulse_interval_ms);
    let now = Instant::now();

    let next_beat_time = now + Duration::from_millis(next_beat_ms as u64);

    let start_index: u64 = if beat_in_bar == 1 {
        0
    } else {
        cs.pulse_index
    };

    for i in start_index..PPQ {
        let pulse_time = next_beat_time + (pulse_interval * (i as u32));

        let delay = if pulse_time > now {
            pulse_time.duration_since(now)
        } else {
            Duration::ZERO
        };

        if delay > Duration::ZERO {
            time::sleep(delay).await;
        }

        let _ = midi.send_message(&[MSG_CLOCK]);
        activity.lock().clock_pulses += 1;
    }

    cs.pulse_index = 0;
}

// ── Beat event handler ────────────────────────────────────────────────────────

/// Process a single beat event, updating clock BPM and scheduling pulses.
/// This is the core of beat-synced clock: on each beat, immediately send
/// 24 pulses distributed through the beat interval.
async fn handle_beat_event(
    cs: &mut ClockState,
    state: &SharedState,
    midi: &Arc<dyn MidiTransport>,
    activity: &Arc<Mutex<MidiActivity>>,
    evt: BeatEvent,
) {
    match evt {
        BeatEvent::Beat(bp) => {
            let master_num = state.read().master.device_number;
            if bp.device_number == master_num || master_num == 0 {
                cs.set_bpm(bp.effective_bpm);

                if cs.waiting_for_downbeat && bp.beat_in_bar == 1 {
                    cs.waiting_for_downbeat = false;
                    cs.running = true;
                    cs.pulse_index = 0;
                    cs.has_started = true;
                    let _ = midi.send_message(&[MSG_START]);
                    let _ = midi.send_message(&[MSG_CLOCK]);
                    activity.lock().clock_pulses += 1;
                }

                if cs.running {
                    schedule_pulses(cs, midi, activity, bp.next_beat_ms, bp.beat_in_bar).await;
                }
            }
        }
        BeatEvent::AbsPosition(ap) => {
            let master_num = state.read().master.device_number;
            if ap.device_number == master_num || master_num == 0 {
                cs.set_bpm(ap.effective_bpm);
            }
        }
        BeatEvent::LinkBeat { bpm, beat_in_bar, .. } => {
            cs.set_bpm(bpm);

            if cs.waiting_for_downbeat && beat_in_bar == 1 {
                cs.waiting_for_downbeat = false;
                cs.running = true;
                cs.pulse_index = 0;
                cs.has_started = true;
                let _ = midi.send_message(&[MSG_START]);
                let _ = midi.send_message(&[MSG_CLOCK]);
                activity.lock().clock_pulses += 1;
            }

            if cs.running {
                let pulse_interval_ms = (60000.0 / bpm) as u32 / PPQ as u32;
                schedule_pulses(cs, midi, activity, pulse_interval_ms * PPQ as u32, beat_in_bar).await;
            }
        }
    }
}

// ── Main clock task ───────────────────────────────────────────────────────────

pub async fn run(
    midi: Arc<dyn MidiTransport>,
    state: SharedState,
    mut beat_rx: broadcast::Receiver<BeatEvent>,
    cfg: SharedConfig,
    activity: Arc<Mutex<MidiActivity>>,
) {
    let mut cs = ClockState::new();
    let mut clock_enabled = cfg.read().midi.clock_enabled;

    tracing::info!("MIDI clock task started");
    if !clock_enabled {
        tracing::info!("MIDI clock disabled in config");
    }

    loop {
        let current_enabled = cfg.read().midi.clock_enabled;
        if current_enabled != clock_enabled {
            clock_enabled = current_enabled;
            if !clock_enabled {
                if cs.running {
                    cs.running = false;
                    cs.has_started = false;
                    let _ = midi.send_message(&[MSG_STOP]);
                    tracing::info!("MIDI clock disabled at runtime");
                }
            } else {
                tracing::info!("MIDI clock enabled at runtime, waiting for downbeat");
                cs.waiting_for_downbeat = true;
            }
        }

// Wait for next beat event - this is the core beat-sync loop
        match beat_rx.recv().await {
            Ok(evt) => handle_beat_event(&mut cs, &state, &midi, &activity, evt).await,
            Err(broadcast::error::RecvError::Closed) => return,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Beat event lagged, dropped {} events", n);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{new_shared, Config};
    use crate::midi::test_utils::MockMidiTransport;
    use crate::state::{BeatSource, MasterState};
    use std::time::Duration;

    fn make_beat(device_number: u8, effective_bpm: f64) -> BeatEvent {
        make_beat_with_bar(device_number, effective_bpm, 1)
    }

    fn make_beat_with_bar(device_number: u8, effective_bpm: f64, beat_in_bar: u8) -> BeatEvent {
        BeatEvent::Beat(crate::prolink::packets::BeatPacket {
            device_number,
            next_beat_ms: 500,
            second_beat_ms: 0,
            next_bar_ms: 0,
            pitch_raw: crate::prolink::PITCH_NORMAL,
            bpm_raw: (effective_bpm * 100.0) as u16,
            beat_in_bar,
            track_bpm: Some(effective_bpm),
            effective_bpm,
            pitch_pct: 0.0,
        })
    }

    fn make_abs_position(device_number: u8, effective_bpm: f64, playhead_ms: u32) -> BeatEvent {
        BeatEvent::AbsPosition(crate::prolink::packets::AbsPositionPacket {
            device_number,
            track_length_s: 180,
            playhead_ms,
            pitch_raw_signed: 0,
            bpm_x10: (effective_bpm * 10.0) as u32,
            effective_bpm,
            pitch_pct: 0.0,
        })
    }

    #[test]
    fn clock_state_starts_idle() {
        let cs = ClockState::new();
        assert!(!cs.running);
        assert!(!cs.has_started);
        assert_eq!(cs.bpm, 0.0);
    }

    #[test]
    fn clock_state_set_bpm_updates_interval() {
        let mut cs = ClockState::new();
        cs.set_bpm(120.0);
        assert_eq!(cs.bpm, 120.0);
        assert_eq!(cs.beat_interval_ms, 500);
    }

    #[test]
    fn clock_state_set_bpm_at_140() {
        let mut cs = ClockState::new();
        cs.set_bpm(140.0);
        assert_eq!(cs.bpm, 140.0);
        assert_eq!(cs.beat_interval_ms, 428);
    }

    #[test]
    fn clock_state_set_bpm_at_dj_standard() {
        let mut cs = ClockState::new();
        cs.set_bpm(128.0);
        assert_eq!(cs.bpm, 128.0);
        assert_eq!(cs.beat_interval_ms, 468);
    }

#[test]
    fn waiting_for_downbeat_can_be_set() {
        let mut cs = ClockState::new();
        assert!(cs.waiting_for_downbeat);
        cs.waiting_for_downbeat = false;
        assert!(!cs.waiting_for_downbeat);
    }

    #[test]
    fn clock_state_starts_idle_no_running() {
        let mut cs = ClockState::new();
        assert!(!cs.running);
        cs.running = true;
        assert!(cs.running);
    }

    #[test]
    fn waiting_for_downbeat_initially_true() {
        let cs = ClockState::new();
        assert!(cs.waiting_for_downbeat);
    }
}

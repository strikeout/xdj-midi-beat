//! MIDI Clock generator — 24 pulses-per-quarter-note.
//!
//! Generates MIDI Timing Clock (0xF8) messages at the correct rate for the
//! current BPM, plus MIDI Start (0xFA), Stop (0xFC), and Continue (0xFB)
//! messages when the master deck starts or stops.
//!
//! Uses deadline-based sleeping: computes the exact instant of the next pulse
//! and sleeps until then (via `tokio::select!` with `sleep_until`), rather
//! than polling at a fixed interval.  Beat events arriving mid-sleep wake
//! the task immediately for phase correction.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::midi::MidiTransport;

use crate::config::SharedConfig;
use crate::prolink::beat_listener::BeatEvent;
use crate::state::SharedState;
use crate::tui::state::MidiActivity;

// ── MIDI byte constants ───────────────────────────────────────────────────────

const MSG_CLOCK: u8 = 0xF8;
const MSG_START: u8 = 0xFA;
const MSG_CONTINUE: u8 = 0xFB;
const MSG_STOP: u8 = 0xFC;

/// Pulses per quarter note — MIDI spec, always 24.
const PPQ: u64 = 24;

// ── Clock state ───────────────────────────────────────────────────────────────

struct ClockState {
    /// Current inter-pulse interval in nanoseconds (derived from BPM).
    interval_ns: u64,
    /// Time we last sent a clock pulse.
    last_pulse: Instant,
    /// Current pulse index within the quarter note (0–23).
    pulse_index: u64,
    /// Whether we are currently in the "running" state.
    running: bool,
    /// Whether the clock has been started at least once (for Continue vs Start).
    has_started: bool,
    /// BPM at last update (used to detect changes).
    last_bpm: f64,
}

impl ClockState {
    fn new() -> Self {
        Self {
            interval_ns: bpm_to_interval_ns(120.0),
            last_pulse: Instant::now(),
            pulse_index: 0,
            running: false,
            has_started: false,
            last_bpm: 0.0,
        }
    }

    fn set_bpm(&mut self, bpm: f64) {
        if bpm > 0.0 && (bpm - self.last_bpm).abs() > 0.01 {
            self.interval_ns = bpm_to_interval_ns(bpm);
            self.last_bpm = bpm;
        }
    }
}

fn bpm_to_interval_ns(bpm: f64) -> u64 {
    // interval = 60s / (bpm * PPQ) converted to nanoseconds
    let ns = 60.0e9 / (bpm * PPQ as f64);
    ns.round() as u64
}

// ── Phase correction ──────────────────────────────────────────────────────────

/// Maximum correction we will apply in one shot (1/4 of a pulse interval).
const MAX_CORRECTION_FRACTION: f64 = 0.25;

/// When a beat arrives, compute how far off our clock is and nudge `last_pulse`
/// to correct it.  We correct by at most MAX_CORRECTION_FRACTION of an
/// interval to avoid audible jumps.
fn apply_phase_correction(cs: &mut ClockState, beat_at: Instant) {
    if cs.interval_ns == 0 {
        return;
    }
    // Ideal: beat_at should land exactly on pulse_index == 0.
    let elapsed_since_last = beat_at.saturating_duration_since(cs.last_pulse);
    let elapsed_ns = elapsed_since_last.as_nanos() as u64;

    // How many pulses should have elapsed?
    let pulses_elapsed = elapsed_ns / cs.interval_ns;
    // Remainder: how far past the last pulse boundary are we?
    let remainder_ns = elapsed_ns % cs.interval_ns;

    // We want remainder to be 0 (we are exactly at a pulse boundary).
    // If remainder > interval/2 we are late; if < interval/2 we are early.
    let interval = cs.interval_ns;
    let max_corr = (interval as f64 * MAX_CORRECTION_FRACTION) as u64;

    if remainder_ns > interval / 2 {
        // Late: advance last_pulse forward by up to max_corr ns.
        let correction = (remainder_ns - interval / 2).min(max_corr);
        cs.last_pulse = beat_at - Duration::from_nanos(elapsed_ns - correction);
    } else if remainder_ns > 0 && remainder_ns < interval / 2 {
        // Early: push last_pulse back by up to max_corr ns.
        let correction = remainder_ns.min(max_corr);
        cs.last_pulse = beat_at - Duration::from_nanos(elapsed_ns + correction);
    }

    // Snap pulse_index to 0 at beat boundary.
    let _ = pulses_elapsed; // already consumed above
    cs.pulse_index = 0;
}

// ── Beat event handler ────────────────────────────────────────────────────────

/// Process a single beat event, updating clock BPM and phase correction.
fn handle_beat_event(cs: &mut ClockState, state: &SharedState, evt: BeatEvent) {
    match evt {
        BeatEvent::Beat(bp) => {
            let master_num = state.read().master.device_number;
            if bp.device_number == master_num || master_num == 0 {
                cs.set_bpm(bp.effective_bpm);
                apply_phase_correction(cs, Instant::now());
            }
        }
        BeatEvent::AbsPosition(ap) => {
            let master_num = state.read().master.device_number;
            if ap.device_number == master_num || master_num == 0 {
                cs.set_bpm(ap.effective_bpm);
            }
        }
        BeatEvent::LinkBeat { bpm, .. } => {
            cs.set_bpm(bpm);
            apply_phase_correction(cs, Instant::now());
        }
    }
}

/// Drain all queued beat events without blocking.
fn drain_beat_events(
    cs: &mut ClockState,
    state: &SharedState,
    beat_rx: &mut broadcast::Receiver<BeatEvent>,
) {
    loop {
        match beat_rx.try_recv() {
            Ok(evt) => handle_beat_event(cs, state, evt),
            Err(_) => break,
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
                tracing::info!("MIDI clock enabled at runtime");
                cs.last_pulse = Instant::now();
            }
        }

        // ── Compute next pulse deadline ──────────────────────────────────────
        // Latency compensation is a ONE-TIME offset applied only on the first pulse
        // after clock starts (to account for output interface delay).
        // After first pulse, normal intervals continue without offset.
        let now = Instant::now();
        let needs_latency = cs.running
            && now.duration_since(cs.last_pulse).as_millis() < 10
            && cfg.read().midi.latency_compensation_ms != 0;

        let next_pulse_at = if clock_enabled && cs.running && cs.interval_ns > 0 {
            let base = cs.last_pulse + Duration::from_nanos(cs.interval_ns);
            if needs_latency {
                let latency_offset_ms = cfg.read().midi.latency_compensation_ms;
                if latency_offset_ms > 0 {
                    base + Duration::from_millis(latency_offset_ms as u64)
                } else {
                    base.checked_sub(Duration::from_millis((-latency_offset_ms) as u64))
                        .unwrap_or(base)
                }
            } else {
                base
            }
        } else {
            // Not running: wake periodically to check state changes.
            Instant::now() + Duration::from_millis(10)
        };
        let sleep_dur = next_pulse_at.saturating_duration_since(Instant::now());
        let deadline = tokio::time::Instant::now() + sleep_dur;

        // ── Wait for next pulse deadline OR a beat event ─────────────────────
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {}
            evt = beat_rx.recv() => {
                match evt {
                    Ok(evt) => {
                        handle_beat_event(&mut cs, &state, evt);
                        drain_beat_events(&mut cs, &state, &mut beat_rx);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        }

        // ── Read master state ────────────────────────────────────────────────
        let (bpm, is_playing) = {
            let st = state.read();
            (st.master.bpm, st.master.is_playing)
        };

        // ── Start / Stop / Continue messages ─────────────────────────────────
        if clock_enabled && is_playing && !cs.running {
            cs.running = true;
            cs.pulse_index = 0;
            cs.last_pulse = Instant::now();
            if bpm > 0.0 {
                cs.set_bpm(bpm);
            }
            let msg = if cs.has_started { MSG_CONTINUE } else { MSG_START };
            cs.has_started = true;
            // Send Start/Continue followed by the first clock pulse
            // immediately (MIDI spec: clock should follow Start without delay).
            let _ = midi.send_message(&[msg]);
            let _ = midi.send_message(&[MSG_CLOCK]);
            activity.lock().clock_pulses += 1;
            tracing::debug!(msg = if msg == MSG_START { "Start" } else { "Continue" }, "MIDI transport sent");
        } else if (!clock_enabled || !is_playing) && cs.running {
            cs.running = false;
            let _ = midi.send_message(&[MSG_STOP]);
            tracing::debug!("MIDI Stop sent");
        }

        // ── Emit clock pulse if deadline reached ─────────────────────────────
        if clock_enabled && cs.running && bpm > 0.0 {
            cs.set_bpm(bpm);
            let now = Instant::now();
            let elapsed = now.duration_since(cs.last_pulse).as_nanos() as u64;
            if elapsed >= cs.interval_ns {
                let overshoot = elapsed - cs.interval_ns;
                cs.last_pulse = now - Duration::from_nanos(overshoot.min(cs.interval_ns / 2));
                cs.pulse_index = (cs.pulse_index + 1) % PPQ;
                let _ = midi.send_message(&[MSG_CLOCK]);
                activity.lock().clock_pulses += 1;
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
        BeatEvent::Beat(crate::prolink::packets::BeatPacket {
            device_number,
            next_beat_ms: 0,
            second_beat_ms: 0,
            next_bar_ms: 0,
            pitch_raw: crate::prolink::PITCH_NORMAL,
            bpm_raw: (effective_bpm * 100.0) as u16,
            beat_in_bar: 1,
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
    fn bpm_to_interval_ns_exact() {
        let ns = bpm_to_interval_ns(120.0);
        let expected: u64 = (60.0e9_f64 / (120.0 * 24.0)).round() as u64;
        assert_eq!(ns, expected);
    }

    #[test]
    fn bpm_to_interval_ns_at_140() {
        let ns = bpm_to_interval_ns(140.0);
        let expected: u64 = (60.0e9_f64 / (140.0 * 24.0)).round() as u64;
        assert_eq!(ns, expected);
    }

    #[test]
    fn bpm_to_interval_ns_edge_cases() {
        // Very fast BPM
        let ns = bpm_to_interval_ns(300.0);
        let expected: u64 = (60.0e9_f64 / (300.0 * 24.0)).round() as u64;
        assert_eq!(ns, expected);
        assert!(ns > 0); // Should not underflow
        
        // Very slow BPM  
        let ns = bpm_to_interval_ns(20.0);
        let expected: u64 = (60.0e9_f64 / (20.0 * 24.0)).round() as u64;
        assert_eq!(ns, expected);
        
        // Standard DJ range
        let ns = bpm_to_interval_ns(128.0);
        let expected: u64 = (60.0e9_f64 / (128.0 * 24.0)).round() as u64;
        assert_eq!(ns, expected);
    }

    #[test]
    fn clock_state_starts_idle() {
        let cs = ClockState::new();
        assert!(!cs.running);
        assert!(!cs.has_started);
        assert_eq!(cs.last_bpm, 0.0);
    }

    #[test]
    fn clock_state_set_bpm_updates_interval() {
        let mut cs = ClockState::new();
        assert!(cs.last_bpm != 120.0);
        cs.set_bpm(120.0);
        assert_eq!(cs.interval_ns, bpm_to_interval_ns(120.0));
        assert_eq!(cs.last_bpm, 120.0);
    }

    #[test]
    fn clock_state_set_bpm_ignores_small_change() {
        let mut cs = ClockState::new();
        cs.set_bpm(120.0);
        let interval_after_120 = cs.interval_ns;
        cs.set_bpm(120.005);
        assert_eq!(cs.interval_ns, interval_after_120);
    }

    #[test]
    fn phase_correction_snaps_pulse_index_to_zero() {
        let mut cs = ClockState::new();
        cs.interval_ns = 1_000_000;
        cs.pulse_index = 5;
        let beat_at = Instant::now() + Duration::from_millis(1);
        apply_phase_correction(&mut cs, beat_at);
        assert_eq!(cs.pulse_index, 0);
    }

    #[test]
    fn handle_beat_event_sets_bpm_from_master_beat() {
        let mut cs = ClockState::new();
        let state = crate::state::new_shared(30);
        state.write().master.device_number = 2;

        handle_beat_event(&mut cs, &state, make_beat(2, 128.0));
        assert!((cs.last_bpm - 128.0).abs() < 0.01);
    }

    #[test]
    fn handle_beat_event_ignores_non_master_when_master_set() {
        let mut cs = ClockState::new();
        cs.interval_ns = bpm_to_interval_ns(100.0);
        cs.last_bpm = 100.0;
        let state = crate::state::new_shared(30);
        state.write().master.device_number = 2;

        handle_beat_event(&mut cs, &state, make_beat(1, 130.0));
        assert!((cs.last_bpm - 100.0).abs() < 0.01);
    }

    #[test]
    fn handle_beat_event_accepts_any_device_when_no_master() {
        let mut cs = ClockState::new();
        let state = crate::state::new_shared(30);
        state.write().master.device_number = 0;

        handle_beat_event(&mut cs, &state, make_beat(3, 135.0));
        assert!((cs.last_bpm - 135.0).abs() < 0.01);
    }

    #[test]
    fn handle_beat_event_abs_position_updates_bpm() {
        let mut cs = ClockState::new();
        let state = crate::state::new_shared(30);
        state.write().master.device_number = 5;

        handle_beat_event(&mut cs, &state, make_abs_position(5, 124.0, 5000));
        assert!((cs.last_bpm - 124.0).abs() < 0.01);
    }

    #[test]
    fn handle_beat_event_link_beat_updates_bpm() {
        let mut cs = ClockState::new();
        let state = crate::state::new_shared(30);

        handle_beat_event(
            &mut cs,
            &state,
            BeatEvent::LinkBeat {
                bpm: 122.0,
                beat_in_bar: 1,
                bar_phase: 0.0,
                beat_phase: 0.0,
            },
        );
        assert!((cs.last_bpm - 122.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn clock_enabled_toggle_takes_effect_at_runtime() {
        let midi = Arc::new(MockMidiTransport::new());
        let midi_transport: Arc<dyn MidiTransport> = midi.clone();
        let cfg = new_shared(Config::default());
        cfg.write().midi.clock_enabled = false;
        let state = crate::state::new_shared(30);
        state.write().master = MasterState {
            source: Some(BeatSource::AbletonLink),
            bpm: 120.0,
            is_playing: true,
            ..Default::default()
        };
        let (_beat_tx, beat_rx) = broadcast::channel(8);
        let activity = Arc::new(Mutex::new(MidiActivity::default()));

        let handle = tokio::spawn(run(midi_transport, state, beat_rx, cfg.clone(), activity));

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(midi.get_messages().is_empty());

        cfg.write().midi.clock_enabled = true;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let messages = midi.get_messages();
        assert!(messages.contains(&vec![MSG_START]));
        assert!(messages.contains(&vec![MSG_CLOCK]));

        handle.abort();
    }

    #[tokio::test]
    async fn latency_compensation_changes_take_effect_at_runtime() {
        let midi = Arc::new(MockMidiTransport::new());
        let midi_transport: Arc<dyn MidiTransport> = midi.clone();
        let cfg = new_shared(Config::default());
        cfg.write().midi.clock_enabled = true;
        let state = crate::state::new_shared(30);
        state.write().master = MasterState {
            source: Some(BeatSource::AbletonLink),
            bpm: 120.0,
            is_playing: true,
            ..Default::default()
        };
        let (_beat_tx, beat_rx) = broadcast::channel(8);
        let activity = Arc::new(Mutex::new(MidiActivity::default()));

        let handle = tokio::spawn(run(midi_transport, state, beat_rx, cfg.clone(), activity));

        tokio::time::sleep(Duration::from_millis(80)).await;
        let baseline = midi.get_messages().len();

        cfg.write().midi.latency_compensation_ms = 200;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let delayed_count = midi.get_messages().len() - baseline;
        assert!(delayed_count <= 1);

        let after_delay_total = midi.get_messages().len();
        cfg.write().midi.latency_compensation_ms = 0;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let resumed_delta = midi.get_messages().len() - after_delay_total;
        assert!(resumed_delta >= 3);

        handle.abort();
    }
}

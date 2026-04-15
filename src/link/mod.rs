//! Ableton Link engine — polls the Link session timeline and pushes beat/tempo/phase
//! updates into SharedState so the MIDI clock and mapper tasks can consume them.
//!
//! # How it works
//!
//! Ableton Link provides a *continuous* beat timeline: you call `beat_at_time(now, quantum)`
//! and `phase_at_time(now, quantum)` at any time to get the current position.  There are no
//! discrete "beat packet" events like Pro DJ Link — instead we poll at ~500µs intervals and
//! detect when the integer part of `beat_at_time` advances (= a beat crossing).
//!
//! On every beat crossing we:
//!   1. Update SharedState with the new beat-in-bar, phases, BPM, and playing state.
//!   2. Fire a `BeatEvent::LinkBeat` down the existing beat broadcast channel so the MIDI
//!      clock and mapper tasks wake up and react (same channel as Pro DJ Link beats).
//!
//! Between crossings we still push phase updates at ~10ms intervals so bar_phase and
//! beat_phase CCs stay smooth.
//!
//! # Auto mode
//!
//! When `source == Auto` this engine runs alongside Pro DJ Link listeners.
//! `DjState::apply_link_state` is a no-op if a Pro DJ Link master is already present,
//! so the two sources coexist gracefully — Link only takes over when no hardware CDJs
//! are broadcasting.

use std::time::{Duration, Instant};

use rusty_link::{AblLink, SessionState};
use tokio::sync::broadcast;

use crate::config::LinkConfig;
use crate::prolink::beat_listener::BeatEvent; // reuse existing channel
use crate::state::SharedState;

/// Run the Ableton Link polling engine.
///
/// - `link_cfg`   – Link-specific config (quantum, poll interval, enabled flag).
/// - `state`      – shared DJ state (written by this task, read by MIDI tasks).
/// - `beat_tx`    – the existing beat broadcast channel (shared with Pro DJ Link).
///
/// This function loops forever (until the runtime shuts down).
pub async fn run(
    link_cfg: LinkConfig,
    state: SharedState,
    beat_tx: broadcast::Sender<BeatEvent>,
) {
    if !link_cfg.enabled {
        tracing::info!("Ableton Link engine disabled by config");
        return;
    }

    // ── Initialise the Link instance ─────────────────────────────────────────
    // Default tempo 120 BPM — overridden by the session as soon as we connect.
    let mut link = AblLink::new(120.0);
    link.enable(true);
    link.enable_start_stop_sync(true);

    // Simple peer-count callback — logging only, no state capture needed.
    link.set_num_peers_callback(|n| {
        if n > 0 {
            tracing::info!(peers = n, "Ableton Link peer(s) connected");
        } else {
            tracing::info!("Ableton Link: no peers (session is empty)");
        }
    });

    tracing::info!(
        quantum = link_cfg.quantum,
        poll_us = link_cfg.poll_interval_us,
        "Ableton Link engine started (waiting for peers…)"
    );

    // ── Session state (re-used each iteration, allocated once) ───────────────
    // SessionState is !Sync so it lives on this task's stack.
    let mut session = SessionState::new();

    let poll_interval = Duration::from_micros(link_cfg.poll_interval_us);
    let quantum = link_cfg.quantum;

    // We emit phase CCs at ~10ms intervals even without a beat crossing.
    let phase_update_interval = Duration::from_millis(10);
    let mut last_phase_update = Instant::now();

    // Track the last integer beat index to detect crossings.
    let mut last_beat_floor: i64 = -1;
    // Track the last playing state to detect transitions.
    let mut last_is_playing: bool = false;

    loop {
        // Sleep for the poll interval.
        tokio::time::sleep(poll_interval).await;

        // ── Capture current Link session state ───────────────────────────────
        link.capture_app_session_state(&mut session);
        let now_us = link.clock_micros();

        let bpm = session.tempo();
        let is_playing = session.is_playing();

        // Beat value as a continuous f64 (e.g. 5.73 = somewhere through beat 6).
        let beat = session.beat_at_time(now_us, quantum);
        // Phase within the quantum window: [0, quantum).
        let phase = session.phase_at_time(now_us, quantum);

        // ── Detect beat crossing ─────────────────────────────────────────────
        // A beat crossing happens when floor(beat) advances.
        let beat_floor = beat.floor() as i64;
        let first_sample = last_beat_floor < 0;
        let beat_crossed = !first_sample && beat_floor > last_beat_floor;

        if first_sample {
            last_beat_floor = beat_floor;
        }

        // ── Compute beat-within-bar (1-based) ────────────────────────────────
        // phase is [0, quantum), so floor(phase) gives which beat within the bar.
        let beat_in_bar = (phase.floor() as u8).saturating_add(1); // 1-based

        // ── Bar phase and beat phase ─────────────────────────────────────────
        // bar_phase: where are we within the whole quantum window? (0.0–1.0)
        let bar_phase = (phase / quantum).clamp(0.0, 1.0);
        // beat_phase: fractional part of phase = position within current beat (0.0–1.0)
        let beat_phase = (phase - phase.floor()).clamp(0.0, 1.0);

        // ── Push to shared state ─────────────────────────────────────────────
        let should_push_phase = {
            let now = Instant::now();
            if now.duration_since(last_phase_update) >= phase_update_interval {
                last_phase_update = now;
                true
            } else {
                false
            }
        };

        let playing_changed = is_playing != last_is_playing;

        if beat_crossed || should_push_phase || playing_changed || first_sample {
            state.write().apply_link_state(
                bpm,
                beat_in_bar,
                bar_phase,
                beat_phase,
                is_playing,
                beat_crossed,
            );
        }

        // ── Broadcast beat event on crossing ─────────────────────────────────
        if beat_crossed && is_playing {
            let _ = beat_tx.send(BeatEvent::LinkBeat {
                bpm,
                beat_in_bar,
                bar_phase,
                beat_phase,
            });
            last_beat_floor = beat_floor;
            tracing::debug!(
                bpm = %format!("{:.2}", bpm),
                beat_in_bar,
                bar_phase = %format!("{:.3}", bar_phase),
                "Link beat"
            );
        }

        // ── Log play state transitions ────────────────────────────────────────
        if playing_changed {
            tracing::info!(is_playing, "Ableton Link transport state changed");
            last_is_playing = is_playing;
        }
    }
}

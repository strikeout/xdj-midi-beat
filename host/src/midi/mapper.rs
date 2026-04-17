use std::sync::Arc;
use std::time::Instant;

use midir::MidiOutputConnection;
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::config::{CcConfig, NoteConfig, SharedConfig};
use crate::midi::{MidirTransport, MidiTransport};
use crate::prolink::beat_listener::BeatEvent;
use crate::prolink::status_listener::StatusEvent;
use crate::state::SharedState;
use crate::tui::state::MidiActivity;

// ── MIDI byte helpers ─────────────────────────────────────────────────────────

fn note_on(ch: u8, note: u8, vel: u8) -> [u8; 3] {
    [0x90 | (ch & 0x0f), note & 0x7f, vel & 0x7f]
}

fn note_off(ch: u8, note: u8) -> [u8; 3] {
    [0x80 | (ch & 0x0f), note & 0x7f, 0]
}

fn cc(ch: u8, num: u8, val: u8) -> [u8; 3] {
    [0xB0 | (ch & 0x0f), num & 0x7f, val & 0x7f]
}

/// Scale a float in [0.0, 1.0] to a 7-bit MIDI value (0–127).
fn scale01(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// Map BPM range 60–187 to 0–127.
fn bpm_coarse(bpm: f64) -> u8 {
    ((bpm - 60.0).clamp(0.0, 127.0)) as u8
}

/// Map fractional BPM (0.00–0.99) to 0–127.
fn bpm_fine(bpm: f64) -> u8 {
    let frac = bpm.fract();
    (frac * 127.0).round() as u8
}

/// Map pitch percent (-10.0 … +10.0) to 0–127, centre = 64.
/// Clamps at ±10 % (standard pitch fader range).
fn pitch_cc(pct: f64) -> u8 {
    let normalised = (pct / 10.0 + 1.0) / 2.0; // 0.0–1.0
    scale01(normalised)
}

// ── Beat note helper ──────────────────────────────────────────────────────────

/// Beat velocity by position: 127 on beat 1, 64 on beat 3, 80 on beats 2/4.
fn beat_velocity(beat_in_bar: u8) -> u8 {
    match beat_in_bar {
        1 => 127,
        3 => 64,
        _ => 80,
    }
}

/// Fire beat + downbeat notes through the MIDI connection.
fn fire_beat_notes(
    midi: &dyn MidiTransport,
    note_cfg: &NoteConfig,
    beat_in_bar: u8,
    activity: &Arc<Mutex<MidiActivity>>,
) {
    let vel = beat_velocity(beat_in_bar);
    let _ = midi.send_message(&note_on(note_cfg.channel, note_cfg.beat, vel));
    let _ = midi.send_message(&note_off(note_cfg.channel, note_cfg.beat));
    if beat_in_bar == 1 {
        let _ = midi.send_message(&note_on(note_cfg.channel, note_cfg.downbeat, 127));
        let _ = midi.send_message(&note_off(note_cfg.channel, note_cfg.downbeat));
    }
    let mut act = activity.lock();
    act.notes_sent += 1;
    act.last_note = Some((note_cfg.beat, Instant::now()));
}

// ── Previous CC values (to suppress duplicate sends) ─────────────────────────

#[derive(Default)]
struct PrevCc {
    bpm_coarse: u8,
    bpm_fine: u8,
    pitch: u8,
    bar_phase: u8,
    beat_phase: u8,
    playing: u8,
    master_deck: u8,
    phrase_16: u8,
}

// ── Main mapper task ──────────────────────────────────────────────────────────

pub async fn run(
    conn: Arc<Mutex<Option<MidiOutputConnection>>>,
    state: SharedState,
    mut beat_rx: broadcast::Receiver<BeatEvent>,
    mut status_rx: broadcast::Receiver<StatusEvent>,
    cfg: SharedConfig,
    activity: Arc<Mutex<MidiActivity>>,
) {
    let midi = MidirTransport::new(Arc::clone(&conn));
    let midi: &'static dyn MidiTransport = Box::leak(Box::new(midi));
    let mut prev = PrevCc::default();
    let mut prev_phrase_idx: Option<usize> = None;
    let mut prev_phrase_16_beat: u8 = 0;
    tracing::info!("MIDI mapper task started");

    loop {
        tokio::select! {
            evt = beat_rx.recv() => {
                match evt {
                    Ok(BeatEvent::Beat(bp)) => {
                        // Only trigger notes / CCs for the master deck.
                        let master_num = state.read().master.device_number;
                        if bp.device_number != master_num && master_num != 0 {
                            continue;
                        }

                        let note_cfg = cfg.read().midi.notes.clone();
                        fire_beat_notes(midi, &note_cfg, bp.beat_in_bar, &activity);
                        tracing::debug!(
                            device = bp.device_number,
                            beat = bp.beat_in_bar,
                            bpm = %format!("{:.2}", bp.effective_bpm),
                            "Beat"
                        );
                    }
                    Ok(BeatEvent::AbsPosition(_)) => {
                        // Phase updates handled via shared state CCs below.
                    }
                    Ok(BeatEvent::LinkBeat { bpm, beat_in_bar, .. }) => {
                        let note_cfg = cfg.read().midi.notes.clone();
                        fire_beat_notes(midi, &note_cfg, beat_in_bar, &activity);
                        tracing::debug!(
                            beat = beat_in_bar,
                            bpm = %format!("{:.2}", bpm),
                            "Link beat"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }

            evt = status_rx.recv() => {
                match evt {
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        }

        // ── CC updates ────────────────────────────────────────────────────────
        let master = state.read().master.clone();
        let cc_cfg: CcConfig = cfg.read().midi.cc.clone();

        let new_bpm_coarse = bpm_coarse(master.bpm);
        let new_bpm_fine = bpm_fine(master.bpm);
        let new_pitch = pitch_cc(master.pitch_pct);
        let new_bar_phase = scale01(master.bar_phase);
        let new_beat_phase = scale01(master.beat_phase);
        let new_playing: u8 = if master.is_playing { 127 } else { 0 };
        let new_master_deck = master.device_number.min(127);

        macro_rules! send_cc_if_changed {
            ($prev:expr, $new:expr, $num:expr, $ch:expr) => {
                if $prev != $new {
                    $prev = $new;
                    let _ = midi.send_message(&cc($ch, $num, $new));
                    let mut act = activity.lock();
                    act.cc_sent += 1;
                    act.last_cc = Some(($num, $new, Instant::now()));
                }
            };
        }

        send_cc_if_changed!(prev.bpm_coarse, new_bpm_coarse, cc_cfg.bpm_coarse, cc_cfg.channel);
        send_cc_if_changed!(prev.bpm_fine, new_bpm_fine, cc_cfg.bpm_fine, cc_cfg.channel);
        send_cc_if_changed!(prev.pitch, new_pitch, cc_cfg.pitch, cc_cfg.channel);
        send_cc_if_changed!(prev.bar_phase, new_bar_phase, cc_cfg.bar_phase, cc_cfg.channel);
        send_cc_if_changed!(prev.beat_phase, new_beat_phase, cc_cfg.beat_phase, cc_cfg.channel);
        send_cc_if_changed!(prev.playing, new_playing, cc_cfg.playing, cc_cfg.channel);
        send_cc_if_changed!(prev.master_deck, new_master_deck, cc_cfg.master_deck, cc_cfg.channel);

        if master.phrase_16_beat == 0 && prev_phrase_16_beat != 0 {
            let new_phrase_16 = 0u8;
            if prev.phrase_16 != new_phrase_16 {
                prev.phrase_16 = new_phrase_16;
                let _ = midi.send_message(&cc(cc_cfg.channel, cc_cfg.phrase_16, new_phrase_16));
                let mut act = activity.lock();
                act.cc_sent += 1;
                act.last_cc = Some((cc_cfg.phrase_16, new_phrase_16, Instant::now()));
            }
        }
        prev_phrase_16_beat = master.phrase_16_beat;

        // ── Phrase change detection ───────────────────────────────────────────
        // Check if the master deck's phrase changed; fire a MIDI note if so.
        if master.device_number > 0 {
            let cur_phrase = state
                .read()
                .devices
                .get(&master.device_number)
                .and_then(|d| d.current_phrase_idx);

            if cur_phrase != prev_phrase_idx && cur_phrase.is_some() && prev_phrase_idx.is_some() {
                let note_cfg = cfg.read().midi.notes.clone();
                let _ = midi.send_message(&note_on(note_cfg.channel, note_cfg.phrase_change, 127));
                let _ = midi.send_message(&note_off(note_cfg.channel, note_cfg.phrase_change));
                let mut act = activity.lock();
                act.notes_sent += 1;
                act.last_note = Some((note_cfg.phrase_change, Instant::now()));
                tracing::debug!(note = note_cfg.phrase_change, "Phrase change MIDI note fired");
            }
            prev_phrase_idx = cur_phrase;
        }
    }
}

use std::sync::Arc;

use tokio::sync::broadcast;

use super::TaskContext;
use crate::midi::MidirTransport;
use crate::prolink::beat_listener::BeatEvent;
use crate::prolink::status_listener::StatusEvent;

pub fn spawn(
    ctx: TaskContext,
    beat_rx: broadcast::Receiver<BeatEvent>,
    status_rx: broadcast::Receiver<StatusEvent>,
) {
    let midi_conn = Arc::clone(&ctx.midi_conn);
    let dj_state = Arc::clone(&ctx.dj_state);
    let cfg = Arc::clone(&ctx.cfg);
    let midi_activity = Arc::clone(&ctx.midi_activity);

    tokio::spawn(crate::midi::mapper::run(
        midi_conn,
        dj_state.clone(),
        beat_rx.resubscribe(),
        status_rx,
        cfg.clone(),
        midi_activity.clone(),
    ));

    let midi_conn2 = Arc::clone(&ctx.midi_conn);
    let dj_state2 = Arc::clone(&ctx.dj_state);
    let cfg2 = Arc::clone(&ctx.cfg);
    let midi_activity2 = Arc::clone(&ctx.midi_activity);
    let beat_rx2 = beat_rx.resubscribe();

    tokio::spawn(async move {
        let midi_transport = Arc::new(MidirTransport::new(midi_conn2));
        crate::midi::clock::run(
            midi_transport,
            dj_state2,
            beat_rx2,
            cfg2,
            midi_activity2,
        ).await;
    });

    let midi_conn3 = Arc::clone(&ctx.midi_conn);
    let dj_state3 = Arc::clone(&ctx.dj_state);
    let cfg3 = Arc::clone(&ctx.cfg);
    let midi_activity3 = Arc::clone(&ctx.midi_activity);

    tokio::spawn(crate::midi::timecode::run(
        midi_conn3,
        dj_state3,
        cfg3,
        midi_activity3,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex as ParkingMutex;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::{new_shared, Config};
    use crate::state::{new_shared as new_shared_state, BeatSource, MasterState, TrackChange};
    use crate::tui::state::MidiActivity;

    #[tokio::test]
    async fn mtc_can_be_enabled_and_disabled_after_startup() {
        let mut cfg = Config::default();
        cfg.midi.clock_enabled = false;
        cfg.midi.mtc.enabled = false;
        let cfg = new_shared(cfg);

        let state = new_shared_state(30);
        state.write().master = MasterState {
            source: Some(BeatSource::AbletonLink),
            bpm: 120.0,
            is_playing: true,
            ..Default::default()
        };

        let (beat_tx, beat_rx) = broadcast::channel(8);
        let (status_tx, status_rx) = broadcast::channel(8);
        let (device_tx, _) = broadcast::channel(8);
        let (vcdjready_tx, _) = broadcast::channel(8);
        let (track_change_tx, _track_change_rx) = tokio::sync::mpsc::channel::<TrackChange>(8);

        let midi_activity = Arc::new(ParkingMutex::new(MidiActivity::default()));
        let ctx = TaskContext {
            dj_state: Arc::clone(&state),
            cfg: Arc::clone(&cfg),
            device_tx,
            beat_tx,
            status_tx,
            vcdjready_tx,
            midi_conn: Arc::new(ParkingMutex::new(None)),
            midi_activity: Arc::clone(&midi_activity),
            track_change_tx,
        };

        spawn(ctx, beat_rx, status_rx);

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(midi_activity.lock().clock_pulses, 0);

        cfg.write().midi.mtc.enabled = true;
        tokio::time::sleep(Duration::from_millis(120)).await;
        let pulses_after_enable = midi_activity.lock().clock_pulses;
        assert!(pulses_after_enable > 0);

        cfg.write().midi.mtc.enabled = false;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let pulses_after_disable = midi_activity.lock().clock_pulses;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(midi_activity.lock().clock_pulses, pulses_after_disable);
    }
}

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::prolink::beat_listener::BeatEvent;
use crate::prolink::status_listener::StatusEvent;
use crate::state::TrackChange;
use super::TaskContext;

pub fn spawn(
    ctx: TaskContext,
    beat_rx: broadcast::Receiver<BeatEvent>,
    status_rx: broadcast::Receiver<StatusEvent>,
    track_change_tx: mpsc::Sender<TrackChange>,
) {
    let dj_state = Arc::clone(&ctx.dj_state);
    let cfg = Arc::clone(&ctx.cfg);

    tokio::spawn(beat_applier(dj_state.clone(), cfg.clone(), beat_rx));

    tokio::spawn(status_applier(dj_state, cfg, status_rx, track_change_tx));
}

async fn beat_applier(
    state: Arc<parking_lot::RwLock<crate::state::DjState>>,
    cfg: Arc<parking_lot::RwLock<crate::config::Config>>,
    mut rx: broadcast::Receiver<BeatEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(BeatEvent::Beat(bp)) => {
                let smoothing_ms = cfg.read().midi.smoothing_ms;
                let mut state = state.write();
                state.set_smoothing_ms(smoothing_ms);
                state.apply_beat(&bp);
            }
            Ok(BeatEvent::AbsPosition(ap)) => {
                let smoothing_ms = cfg.read().midi.smoothing_ms;
                let mut state = state.write();
                state.set_smoothing_ms(smoothing_ms);
                state.apply_abs_position(&ap);
            }
            Ok(BeatEvent::LinkBeat { .. }) => {}
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Beat applier lagged, dropped {n} events");
            }
        }
    }
}

async fn status_applier(
    state: Arc<parking_lot::RwLock<crate::state::DjState>>,
    cfg: Arc<parking_lot::RwLock<crate::config::Config>>,
    mut rx: broadcast::Receiver<StatusEvent>,
    track_change_tx: mpsc::Sender<TrackChange>,
) {
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(StatusEvent::Cdj(s)) => {
                        let smoothing_ms = cfg.read().midi.smoothing_ms;
                        let mut state = state.write();
                        state.set_smoothing_ms(smoothing_ms);
                        let change = state.apply_cdj_status(&s);
                        drop(state);
                        if let Some(tc) = change {
                            let _ = track_change_tx.try_send(tc);
                        }
                    }
                    Ok(StatusEvent::Mixer(s)) => {
                        let smoothing_ms = cfg.read().midi.smoothing_ms;
                        let mut state = state.write();
                        state.set_smoothing_ms(smoothing_ms);
                        state.apply_mixer_status(&s);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Status applier lagged, dropped {n} events");
                    }
                }
            }
        }
    }
}
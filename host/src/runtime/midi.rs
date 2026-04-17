use std::sync::Arc;

use tokio::sync::broadcast;

use super::TaskContext;
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
        dj_state,
        beat_rx,
        status_rx,
        cfg,
        midi_activity,
    ));
}

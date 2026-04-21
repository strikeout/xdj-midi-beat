use std::sync::Arc;

use super::TaskContext;
use crate::link::run as link_run;

pub fn spawn(ctx: TaskContext, startup_cfg: crate::config::Config) {
    let link_cfg = startup_cfg.link.clone();
    let dj_state = Arc::clone(&ctx.dj_state);
    let beat_tx = ctx.beat_tx.clone();
    let timing_tx = ctx.timing_tx.clone();
    let link = rusty_link::AblLink::new(120.0);
    tokio::spawn(async move {
        link_run(link_cfg, dj_state, beat_tx, timing_tx, link).await;
    });
}

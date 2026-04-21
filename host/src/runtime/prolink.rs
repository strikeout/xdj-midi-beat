use std::sync::Arc;

use super::TaskContext;
use crate::prolink::discovery::DeviceTable;

pub fn spawn(ctx: &TaskContext, startup_cfg: crate::config::Config, device_table: DeviceTable) {
    let TaskContext {
        dj_state: _,
        cfg: _,
        timing_tx: _,
        device_tx,
        beat_tx,
        status_tx,
        vcdjready_tx,
        midi_out: _,
        midi_activity: _,
        track_change_tx: _,
    } = ctx;

    let bind_ip = startup_cfg.bind_ip;
    let bcast_ip = startup_cfg.bcast_ip;
    let mac = startup_cfg.mac;

    let disc_table = Arc::clone(&device_table);
    let disc_tx = device_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::prolink::discovery::run(bind_ip, disc_table, disc_tx).await {
            tracing::error!("Discovery task error: {e}");
        }
    });

    let vcdj_name = startup_cfg.device_name.clone();
    let vcdj_ready_tx = vcdjready_tx.clone();
    let vcdj_dev_num = startup_cfg.device_number;
    let vcdj_table = Arc::clone(&device_table);
    tokio::spawn(async move {
        if let Err(e) = crate::prolink::virtual_cdj::run(
            bind_ip,
            bcast_ip,
            mac,
            vcdj_dev_num,
            &vcdj_name,
            vcdj_table,
            vcdj_ready_tx,
        )
        .await
        {
            tracing::error!("Virtual CDJ task error: {e}");
        }
    });

    let beat_tx2 = beat_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::prolink::beat_listener::run(bind_ip, beat_tx2).await {
            tracing::error!("Beat listener error: {e}");
        }
    });

    let status_tx2 = status_tx.clone();
    let vcdjready_tx2 = vcdjready_tx.clone();
    tokio::spawn(async move {
        let mut vcdjready_rx = vcdjready_tx2.subscribe();
        let ready = match vcdjready_rx.recv().await {
            Ok(ready) => ready,
            Err(e) => {
                tracing::error!("Status listener could not receive virtual CDJ ready event: {e}");
                return;
            }
        };

        if let Err(e) =
            crate::prolink::status_listener::run(bind_ip, ready.device_number, status_tx2).await
        {
            tracing::error!("Status listener error: {e}");
        }
    });

    let meta_table = Arc::clone(&device_table);
    let meta_state = Arc::clone(&ctx.dj_state);
    let meta_dev_num = startup_cfg.device_number;
    let (_meta_tx, meta_rx) = tokio::sync::mpsc::channel::<crate::state::TrackChange>(64);
    tokio::spawn(async move {
        crate::prolink::metadata::run(meta_dev_num, meta_table, meta_state, meta_rx).await;
    });
    let track_tx = ctx.track_change_tx.clone();
    tokio::spawn(async move {
        track_tx
            .send(crate::state::TrackChange {
                device_number: 0,
                track_source_player: 0,
                track_slot: 0,
                track_type: 0,
                rekordbox_id: 0,
            })
            .await
            .ok();
    });
}

use std::sync::Arc;
use tokio::sync::broadcast;

use super::TaskContext;
use crate::prolink::discovery::DeviceEvent;

pub fn spawn(ctx: TaskContext, device_rx: broadcast::Receiver<DeviceEvent>) {
    let dj_state = Arc::clone(&ctx.dj_state);
    tokio::spawn(async move {
        let mut rx = device_rx;
        loop {
            match rx.recv().await {
                Ok(DeviceEvent::Appeared(d)) => {
                    tracing::info!(
                        device = d.device_number,
                        name = %d.name,
                        ip = ?d.ip,
                        "DJ device appeared on network"
                    );
                    dj_state.write().mark_prolink_seen();
                }
                Ok(DeviceEvent::Disappeared(num)) => {
                    tracing::info!(device = num, "DJ device left network");
                    dj_state.write().remove_device(num);
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(_) => {}
            }
        }
    });
}

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::app::AppContext;
use crate::config::SharedConfig;
use crate::prolink::beat_listener::BeatEvent;
use crate::prolink::status_listener::StatusEvent;
use crate::prolink::virtual_cdj::VirtualCdjReady;
use crate::state::SharedState;
use crate::tui::state::{LogBuffer, MidiActivity};
use crate::tui::SwappableMidiConn;

pub mod applier;
pub mod link;
pub mod logger;
pub mod midi;
pub mod prolink;

#[derive(Clone)]
pub struct TaskContext {
    pub dj_state: SharedState,
    pub cfg: SharedConfig,
    pub device_tx: broadcast::Sender<crate::prolink::discovery::DeviceEvent>,
    pub beat_tx: broadcast::Sender<BeatEvent>,
    pub status_tx: broadcast::Sender<StatusEvent>,
    pub vcdjready_tx: broadcast::Sender<VirtualCdjReady>,
    pub midi_conn: Arc<Mutex<Option<midir::MidiOutputConnection>>>,
    pub midi_activity: Arc<Mutex<MidiActivity>>,
    pub track_change_tx: mpsc::Sender<crate::state::TrackChange>,
    pub use_prolink: bool,
    pub use_link: bool,
}

pub async fn run(ctx: AppContext, use_tui: bool) -> anyhow::Result<()> {
    let AppContext {
        cli: _,
        startup_cfg,
        midi_conn,
        dj_state,
        cfg,
        device_table,
        bind_ip: _,
        bcast_ip: _,
        mac: _,
        device_tx,
        beat_tx,
        status_tx,
        vcdjready_tx,
        track_change_tx,
    } = ctx;

    let beat_rx1 = beat_tx.subscribe();
    let beat_rx3 = beat_tx.subscribe();
    let status_rx1 = status_tx.subscribe();
    let status_rx2 = status_tx.subscribe();
    let device_rx = device_tx.subscribe();

    let use_prolink = startup_cfg.source != crate::config::Source::Link;
    let use_link = startup_cfg.source != crate::config::Source::ProLink;

    let midi_activity: Arc<Mutex<MidiActivity>> =
        Arc::new(Mutex::new(MidiActivity::default()));

    let midi_conn_owned: Arc<Mutex<Option<midir::MidiOutputConnection>>> =
        Arc::new(Mutex::new(midi_conn.lock().take()));

    let task_ctx = TaskContext {
        dj_state: Arc::clone(&dj_state),
        cfg: Arc::clone(&cfg),
        device_tx: device_tx.clone(),
        beat_tx: beat_tx.clone(),
        status_tx: status_tx.clone(),
        vcdjready_tx: vcdjready_tx.clone(),
        midi_conn: Arc::clone(&midi_conn_owned),
        midi_activity: Arc::clone(&midi_activity),
        track_change_tx: track_change_tx.clone(),
        use_prolink,
        use_link,
    };

    if use_prolink {
        prolink::spawn(&task_ctx, startup_cfg.clone(), device_table.clone());
    }

    if use_link {
        link::spawn(task_ctx.clone(), startup_cfg.clone());
    }

    applier::spawn(task_ctx.clone(), beat_rx1, status_rx1, track_change_tx);

    logger::spawn(task_ctx.clone(), device_rx);

    midi::spawn(task_ctx, beat_rx3, status_rx2);

    if use_tui {
        let midi_conn_swap: SwappableMidiConn = midi_conn_owned;
        crate::tui::run(
            dj_state,
            device_table,
            cfg,
            midi_conn_swap,
            LogBuffer::new(),
            midi_activity,
        )
        .await
    } else {
        crate::app::headless_loop(dj_state, midi_conn_owned).await
    }
}
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use midir::{MidiOutput, MidiOutputConnection};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum MidiError {
    NotConnected,
    SendError(String),
}

impl std::fmt::Display for MidiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MidiError::NotConnected => write!(f, "MIDI output not connected"),
            MidiError::SendError(s) => write!(f, "MIDI send error: {s}"),
        }
    }
}

impl std::error::Error for MidiError {}

pub type MidiResult = std::result::Result<(), MidiError>;

pub trait MidiTransport: Send + Sync {
    fn send_message(&self, msg: &[u8]) -> MidiResult;
}

/// Open a MIDI output port by substring match.
///
/// Behavior matches the host CLI `--midi` selection:
/// - "auto" selects the first available output port.
/// - Otherwise, selects the first port whose name contains `port_name` (case-insensitive).
pub fn open_midi_output(port_name: &str) -> anyhow::Result<MidiOutputConnection> {
    let midi_out = MidiOutput::new("xdj-clock")?;
    let ports = midi_out.ports();
    if ports.is_empty() {
        anyhow::bail!("No MIDI output ports available");
    }

    if port_name == "auto" {
        let port = &ports[0];
        let name = midi_out.port_name(port)?;
        tracing::info!(%name, "Auto-selected MIDI output port");
        return midi_out
            .connect(port, "xdj-clock")
            .map_err(|e| anyhow::anyhow!("{}", e));
    }

    for port in &ports {
        let name = midi_out.port_name(port)?;
        if name.to_lowercase().contains(&port_name.to_lowercase()) {
            tracing::info!(%name, "Selected MIDI output port");
            return midi_out
                .connect(port, "xdj-clock")
                .map_err(|e| anyhow::anyhow!("{}", e));
        }
    }

    anyhow::bail!("MIDI port matching {:?} not found", port_name)
}

/// A sendable MIDI output connection.
///
/// This indirection lets the output worker own the connection and also allows
/// tests to provide a fake connection without requiring `midir`.
pub trait MidiOutConnection: Send {
    fn send(&mut self, msg: &[u8]) -> MidiResult;
}

/// Wraps a `midir::MidiOutputConnection` as a [`MidiOutConnection`].
pub struct MidirOutConnection(pub MidiOutputConnection);

impl MidiOutConnection for MidirOutConnection {
    fn send(&mut self, msg: &[u8]) -> MidiResult {
        self.0
            .send(msg)
            .map_err(|e| MidiError::SendError(e.to_string()))
    }
}

enum MidiOutCommand {
    Send(Vec<u8>),
    /// Swap the owned output connection.
    ///
    /// When `stop_before_drop` is true, a MIDI Stop (0xFC) is sent on the old
    /// connection (if present) before it is dropped.
    SwitchConnection {
        new_conn: Option<Box<dyn MidiOutConnection>>,
        stop_before_drop: bool,
        respond_to: Option<oneshot::Sender<()>>,
    },
    Stop {
        respond_to: Option<oneshot::Sender<()>>,
    },
    #[cfg(test)]
    Barrier(oneshot::Sender<()>),
}

/// A cheap, clonable handle used by producers (clock/mapper/MTC) to enqueue
/// MIDI bytes.
///
/// All sends are serialized by a single worker task which owns the output
/// connection.
///
/// Backpressure policy: the worker uses a bounded queue. `send_message()` is
/// non-blocking and will return an error if the queue is full. Callers are
/// expected to drop that message to avoid blocking timing-critical tasks.
#[derive(Clone)]
pub struct MidiOutHandle {
    tx: mpsc::Sender<MidiOutCommand>,
    connected: Arc<AtomicBool>,
    dropped_messages: Arc<AtomicUsize>,
}

impl MidiOutHandle {
    pub fn start(queue_capacity: usize, initial: Option<Box<dyn MidiOutConnection>>) -> Self {
        let (tx, mut rx) = mpsc::channel::<MidiOutCommand>(queue_capacity);
        let connected = Arc::new(AtomicBool::new(initial.is_some()));
        let connected_worker = Arc::clone(&connected);
        let dropped_messages = Arc::new(AtomicUsize::new(0));
        let dropped_messages_worker = Arc::clone(&dropped_messages);

        tokio::spawn(async move {
            let mut conn: Option<Box<dyn MidiOutConnection>> = initial;
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            let mut sent_total: u64 = 0;
            let mut sent_last: u64 = 0;
            let mut dropped_last: usize = 0;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if tracing::level_enabled!(tracing::Level::TRACE) {
                            let queue_len = rx.len();
                            let dropped_total = dropped_messages_worker.load(Ordering::Relaxed);
                            let dropped_delta = dropped_total.saturating_sub(dropped_last);
                            let sent_delta = sent_total.saturating_sub(sent_last);

                            tracing::trace!(
                                target: "midi.out",
                                connected = conn.is_some(),
                                queue_len,
                                sent_messages = sent_delta,
                                dropped_messages = dropped_delta,
                                dropped_messages_total = dropped_total,
                                "MIDI output worker metrics"
                            );

                            dropped_last = dropped_total;
                            sent_last = sent_total;
                        }
                    }

                    cmd = rx.recv() => {
                        let Some(cmd) = cmd else {
                            break;
                        };

                        match cmd {
                    MidiOutCommand::Send(msg) => {
                        let Some(ref mut c) = conn else {
                            // We may have been disconnected after producers enqueued messages.
                            dropped_messages_worker.fetch_add(1, Ordering::Relaxed);
                            continue;
                        };

                        if let Err(err) = c.send(&msg) {
                            tracing::warn!(error = %err, "MIDI send failed; dropping output connection");
                            conn = None;
                            connected_worker.store(false, Ordering::Relaxed);
                        } else {
                            sent_total = sent_total.saturating_add(1);
                        }
                    }
                    MidiOutCommand::SwitchConnection {
                        mut new_conn,
                        stop_before_drop,
                        respond_to,
                    } => {
                        if stop_before_drop {
                            if let Some(ref mut c) = conn {
                                let _ = c.send(&[0xFC]);
                            }
                        }

                        conn = new_conn.take();
                        connected_worker.store(conn.is_some(), Ordering::Relaxed);

                        tracing::trace!(
                            target: "midi.out",
                            connected = conn.is_some(),
                            stop_before_drop,
                            "MIDI output connection switched"
                        );

                        if let Some(tx) = respond_to {
                            let _ = tx.send(());
                        }
                    }
                    MidiOutCommand::Stop { respond_to } => {
                        if let Some(ref mut c) = conn {
                            let _ = c.send(&[0xFC]);
                        }

                        tracing::trace!(target: "midi.out", "MIDI output Stop sent");

                        if let Some(tx) = respond_to {
                            let _ = tx.send(());
                        }
                    }
                    #[cfg(test)]
                    MidiOutCommand::Barrier(tx) => {
                        let _ = tx.send(());
                    }
                }
                    }
                }
            }
        });

        Self {
            tx,
            connected,
            dropped_messages,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Number of MIDI messages dropped by this handle.
    ///
    /// Drops can happen when the bounded queue is full (producer-side) or after
    /// a disconnect (worker-side) if messages were already enqueued.
    #[allow(dead_code)]
    pub fn dropped_messages(&self) -> usize {
        self.dropped_messages.load(Ordering::Relaxed)
    }

    pub async fn stop(&self) {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(MidiOutCommand::Stop {
                respond_to: Some(tx),
            })
            .await;
        let _ = rx.await;
    }

    pub async fn switch_connection(
        &self,
        new_conn: Option<Box<dyn MidiOutConnection>>,
        stop_before_drop: bool,
    ) {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(MidiOutCommand::SwitchConnection {
                new_conn,
                stop_before_drop,
                respond_to: Some(tx),
            })
            .await;
        let _ = rx.await;
    }

    #[cfg(test)]
    pub async fn barrier(&self) {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(MidiOutCommand::Barrier(tx)).await;
        let _ = rx.await;
    }
}

impl MidiTransport for MidiOutHandle {
    fn send_message(&self, msg: &[u8]) -> MidiResult {
        if !self.is_connected() {
            return Err(MidiError::NotConnected);
        }

        self.tx.try_send(MidiOutCommand::Send(msg.to_vec())).map_err(|e| {
            // Backpressure policy: never block timing-critical producers.
            // If the queue is full, drop the message and increment a counter.
            let dropped = self.dropped_messages.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::trace!(dropped_messages = dropped, error = %e, "MIDI output queue full; dropping message");
            MidiError::SendError(e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex as StdMutex,
    };
    use std::time::Duration;

    #[derive(Default)]
    struct RecordingConn {
        sent: Arc<StdMutex<Vec<Vec<u8>>>>,
        in_send: Arc<AtomicBool>,
        fail_after: Option<usize>,
        send_count: Arc<AtomicUsize>,
    }

    impl RecordingConn {
        fn new(sent: Arc<StdMutex<Vec<Vec<u8>>>>) -> Self {
            Self {
                sent,
                in_send: Arc::new(AtomicBool::new(false)),
                fail_after: None,
                send_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn with_failure_after(mut self, n: usize) -> Self {
            self.fail_after = Some(n);
            self
        }
    }

    impl MidiOutConnection for RecordingConn {
        fn send(&mut self, msg: &[u8]) -> MidiResult {
            if self
                .in_send
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(MidiError::SendError(
                    "concurrent send detected (worker is not serializing)".to_string(),
                ));
            }

            let count = self.send_count.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(n) = self.fail_after {
                if count > n {
                    self.in_send.store(false, Ordering::SeqCst);
                    return Err(MidiError::SendError("simulated disconnect".to_string()));
                }
            }

            // Simulate a slow OS send to increase contention chance.
            std::thread::sleep(Duration::from_millis(1));
            self.sent.lock().unwrap().push(msg.to_vec());
            self.in_send.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn mixed_producers_are_serialized_by_worker() {
        let sent = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
        let conn: Box<dyn MidiOutConnection> = Box::new(RecordingConn::new(Arc::clone(&sent)));
        let midi = MidiOutHandle::start(4096, Some(conn));

        let mut tasks = Vec::new();
        for prefix in [0xA0u8, 0xB0u8, 0xC0u8] {
            let m = midi.clone();
            tasks.push(tokio::spawn(async move {
                for i in 0u8..50 {
                    let _ = m.send_message(&[prefix, i]);
                    tokio::task::yield_now().await;
                }
            }));
        }

        for t in tasks {
            t.await.unwrap();
        }
        midi.barrier().await;

        let msgs = sent.lock().unwrap().clone();
        assert_eq!(msgs.len(), 150);
        assert!(msgs.iter().all(|m| m.len() == 2));
    }

    #[tokio::test]
    async fn fifo_order_is_preserved_across_producers() {
        let sent = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
        let conn: Box<dyn MidiOutConnection> = Box::new(RecordingConn::new(Arc::clone(&sent)));
        let midi = MidiOutHandle::start(4096, Some(conn));

        // Multiple independent producers, with a controller enforcing a known enqueue order.
        const PRODUCERS: usize = 3;
        const MSGS: u16 = 200;

        let mut producer_txs = Vec::new();
        let mut producer_tasks = Vec::new();

        for p in 0..PRODUCERS {
            let (tx, mut rx) = mpsc::channel::<(u16, oneshot::Sender<()>)>(64);
            producer_txs.push(tx);

            let m = midi.clone();
            let prefix = 0xA0u8 + (p as u8);
            producer_tasks.push(tokio::spawn(async move {
                while let Some((seq, ack)) = rx.recv().await {
                    let msg = [prefix, (seq & 0xff) as u8, (seq >> 8) as u8];
                    m.send_message(&msg).expect("send_message should succeed");
                    let _ = ack.send(());
                }
            }));
        }

        let mut expected: Vec<Vec<u8>> = Vec::with_capacity(MSGS as usize);
        for seq in 0..MSGS {
            let p = (seq as usize) % PRODUCERS;
            let prefix = 0xA0u8 + (p as u8);
            expected.push(vec![prefix, (seq & 0xff) as u8, (seq >> 8) as u8]);

            let (ack_tx, ack_rx) = oneshot::channel();
            producer_txs[p]
                .send((seq, ack_tx))
                .await
                .expect("producer channel should be open");
            ack_rx.await.expect("producer ack should arrive");
        }

        drop(producer_txs);
        for t in producer_tasks {
            t.await.unwrap();
        }

        midi.barrier().await;

        let msgs = sent.lock().unwrap().clone();
        assert_eq!(msgs, expected);
        assert_eq!(midi.dropped_messages(), 0);
    }

    #[tokio::test]
    async fn disconnect_does_not_deadlock_or_crash_worker() {
        let sent = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
        let conn: Box<dyn MidiOutConnection> =
            Box::new(RecordingConn::new(Arc::clone(&sent)).with_failure_after(5));
        let midi = MidiOutHandle::start(128, Some(conn));

        // First few will enqueue; after simulated disconnect, handle reports NotConnected.
        for i in 0u8..20 {
            let _ = midi.send_message(&[0xD0, i]);
            tokio::task::yield_now().await;
        }

        tokio::time::timeout(Duration::from_millis(200), midi.barrier())
            .await
            .expect("worker barrier should complete")
            ;

        // Once disconnected, we should not block; we should get a fast error.
        assert!(matches!(midi.send_message(&[0xFC]), Err(MidiError::NotConnected)));
    }

    #[tokio::test]
    async fn not_connected_is_fast_and_does_not_deadlock() {
        let midi = MidiOutHandle::start(16, None);
        assert!(matches!(midi.send_message(&[0xF8]), Err(MidiError::NotConnected)));

        tokio::time::timeout(Duration::from_millis(200), midi.barrier())
            .await
            .expect("barrier should complete even when not connected");
    }
}

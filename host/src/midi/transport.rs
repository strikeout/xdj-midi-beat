use std::sync::Arc;

use midir::MidiOutputConnection;
use parking_lot::Mutex;

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

pub struct MidirTransport {
    conn: Arc<Mutex<Option<MidiOutputConnection>>>,
}

impl MidirTransport {
    pub fn new(conn: Arc<Mutex<Option<MidiOutputConnection>>>) -> Self {
        Self { conn }
    }
}

impl MidiTransport for MidirTransport {
    fn send_message(&self, msg: &[u8]) -> MidiResult {
        let mut c = self.conn.lock();
        match &mut *c {
            Some(ref mut conn) => conn
                .send(msg)
                .map_err(|e| MidiError::SendError(e.to_string())),
            None => Err(MidiError::NotConnected),
        }
    }
}

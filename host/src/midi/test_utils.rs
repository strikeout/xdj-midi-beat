use crate::midi::transport::MidiResult;
use crate::midi::{MidiError, MidiTransport};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct MockMidiTransport {
    messages: Arc<Mutex<Vec<Vec<u8>>>>,
    should_fail: bool,
}

impl MockMidiTransport {
    pub fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
            should_fail: false,
        }
    }

    pub fn get_messages(&self) -> Vec<Vec<u8>> {
        self.messages.lock().unwrap().clone()
    }

    pub fn clear_messages(&self) {
        self.messages.lock().unwrap().clear();
    }
}

impl MidiTransport for MockMidiTransport {
    fn send_message(&self, msg: &[u8]) -> MidiResult {
        if self.should_fail {
            return Err(MidiError::SendError("Mock failure".to_string()));
        }
        self.messages.lock().unwrap().push(msg.to_vec());
        Ok(())
    }
}

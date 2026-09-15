//! Correlates asynchronous game chat replies with HTTP control requests.

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct PendingReplies {
    senders: Arc<Mutex<HashMap<&'static str, mpsc::Sender<String>>>>,
}

impl PendingReplies {
    pub(crate) fn register(&self, tag: &'static str) -> Option<mpsc::Receiver<String>> {
        let (sender, receiver) = mpsc::channel();
        let mut senders = self.senders.lock().ok()?;
        if senders.contains_key(tag) {
            return None;
        }
        senders.insert(tag, sender);
        Some(receiver)
    }

    pub(crate) fn resolve(&self, tag: &str, payload: String) -> bool {
        let sender = self
            .senders
            .lock()
            .ok()
            .and_then(|mut senders| senders.remove(tag));
        sender.is_some_and(|sender| sender.send(payload).is_ok())
    }

    pub(crate) fn cancel(&self, tag: &str) {
        if let Ok(mut senders) = self.senders.lock() {
            senders.remove(tag);
        }
    }
}

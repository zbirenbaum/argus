// Rust guideline compliant 2026-02-21
//! Async sink that forwards events to a tokio broadcast channel.

use anyhow::Result;
use tokio::sync::broadcast;

use crate::events::Event;
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// Async sink that publishes events to a broadcast channel.
///
/// Subscribers receive a clone of each event. If there are no active
/// subscribers, the send is silently dropped — this is intentional since
/// broadcast is a best-effort fan-out for live observers (e.g., WebSocket
/// streams) that should not block the write path.
#[derive(Debug, Clone)]
pub struct BroadcastSink {
    tx: broadcast::Sender<Event>,
}

impl BroadcastSink {
    /// Creates a sink that publishes to `tx`.
    pub fn new(tx: broadcast::Sender<Event>) -> Self {
        Self { tx }
    }

    /// Returns a new receiver subscribed to the broadcast channel.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Sink for BroadcastSink {
    fn priority(&self) -> SinkPriority {
        SinkPriority::Async
    }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Event(_))
    }

    fn write(&mut self, record: Record) -> Result<()> {
        if let Record::Event(event) = record {
            // SendError means all receivers were dropped; that is fine
            // since broadcast is purely for live observers.
            let _ = self.tx.send(event);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "broadcast"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventPayload, control::AgentStart};
    use crate::pipeline::record::Record;

    fn make_event(seq: u64) -> Event {
        Event {
            seq,
            ts_monotonic: 0,
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test".to_owned(),
            vclock: None,
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test".to_owned(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".to_owned(),
                node: None,
                pod: None,
                container: None,
            }),
        }
    }

    #[test]
    fn delivers_event_to_subscriber() {
        let (tx, mut rx) = broadcast::channel(8);
        let mut sink = BroadcastSink::new(tx);
        let event = make_event(7);
        sink.write(Record::Event(event.clone())).expect("write");
        let received = rx.try_recv().expect("recv");
        assert_eq!(received.seq, event.seq);
    }

    #[test]
    fn no_subscribers_does_not_error() {
        let (tx, _rx) = broadcast::channel::<Event>(8);
        // Drop the receiver so there are no subscribers.
        let mut sink = BroadcastSink::new(tx);
        let event = make_event(1);
        sink.write(Record::Event(event)).expect("write with no subscribers");
    }

    #[test]
    fn accept_rejects_content() {
        let (tx, _rx) = broadcast::channel::<Event>(8);
        let sink = BroadcastSink::new(tx);
        let hash = crate::cas::ContentHash::from_data(b"x");
        assert!(!sink.accept(&Record::Content { hash, data: vec![] }));
    }

    #[test]
    fn accept_allows_events() {
        let (tx, _rx) = broadcast::channel::<Event>(8);
        let sink = BroadcastSink::new(tx);
        let event = make_event(2);
        assert!(sink.accept(&Record::Event(event)));
    }

    #[test]
    fn name_is_broadcast() {
        let (tx, _rx) = broadcast::channel::<Event>(8);
        let sink = BroadcastSink::new(tx);
        assert_eq!(sink.name(), "broadcast");
    }
}

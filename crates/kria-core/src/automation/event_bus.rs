use std::sync::Arc;
use tokio::sync::broadcast;

pub type EventHandler = Arc<dyn Fn(&Event) + Send + Sync>;

/// Internal event bus for decoupled communication between subsystems.
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub topic: String,
    pub payload: serde_json::Value,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event to all subscribers.
    pub fn publish(&self, topic: &str, payload: serde_json::Value) {
        let event = Event {
            topic: topic.to_string(),
            payload,
            timestamp: chrono::Utc::now(),
        };
        let _ = self.tx.send(event);
    }

    /// Subscribe to events. Returns a receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Subscribe to events matching a topic prefix.
    pub fn subscribe_filtered(&self, prefix: String) -> FilteredSubscriber {
        FilteredSubscriber {
            rx: self.tx.subscribe(),
            prefix,
        }
    }
}

pub struct FilteredSubscriber {
    rx: broadcast::Receiver<Event>,
    prefix: String,
}

impl FilteredSubscriber {
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.rx.recv().await {
                Ok(event) if event.topic.starts_with(&self.prefix) => return Some(event),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event bus subscriber lagged by {n} events");
                    continue;
                }
                Err(_) => return None,
            }
        }
    }
}

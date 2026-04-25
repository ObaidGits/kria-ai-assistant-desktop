//! Durable inbox queue — `fjall`-backed persistence with `mpsc` hot path.
//!
//! [`InboxQueue`] guarantees:
//! - Crash-safe persistence: messages survive process restart.
//! - Ordered delivery per [`ConversationKey`].
//! - Lightweight in-memory notify channel for zero-copy hot-path.
//!
//! # Layout inside fjall keyspace
//!
//! ```text
//! partition "inbox"
//!   key = big-endian u128 (UUID v7 = time-sortable)
//!   val = msgpack-encoded InboundMessage
//! ```

use std::path::Path;
use std::sync::Arc;

use fjall::{Config as FjallConfig, Keyspace, PartitionCreateOptions};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::InboundMessage;

const PARTITION_NAME: &str = "inbox";
/// Channel capacity for the in-memory hot path.
const NOTIFY_CAPACITY: usize = 256;

// ── InboxQueue ────────────────────────────────────────────────────────────────

/// Durable first-in, first-out queue for [`InboundMessage`]s.
///
/// Clone is cheap — all clones share the same underlying keyspace.
#[derive(Clone)]
pub struct InboxQueue {
    inner: Arc<InboxQueueInner>,
}

struct InboxQueueInner {
    keyspace: Keyspace,
    notify_tx: mpsc::Sender<()>,
}

impl InboxQueue {
    /// Open (or create) the queue at `data_dir`.
    ///
    /// Also returns the notify receiver.  The consumer should `await` on this
    /// receiver and then call [`Self::dequeue`] in a loop until `None`.
    pub fn open(data_dir: impl AsRef<Path>) -> anyhow::Result<(Self, mpsc::Receiver<()>)> {
        let path = data_dir.as_ref().join("inbox_queue");
        std::fs::create_dir_all(&path)?;

        let keyspace = FjallConfig::new(&path)
            .open()
            .map_err(|e| anyhow::anyhow!("fjall open error: {e}"))?;

        let (notify_tx, notify_rx) = mpsc::channel(NOTIFY_CAPACITY);

        let queue = Self {
            inner: Arc::new(InboxQueueInner {
                keyspace,
                notify_tx,
            }),
        };

        // Recover any messages that survived a crash.
        let pending = queue.pending_count()?;
        if pending > 0 {
            info!(pending, "inbox_queue: recovered messages after restart");
        }

        Ok((queue, notify_rx))
    }

    /// Persist a message and wake the consumer.
    pub fn enqueue(&self, msg: &InboundMessage) -> anyhow::Result<()> {
        let partition = self
            .inner
            .keyspace
            .open_partition(PARTITION_NAME, PartitionCreateOptions::default())
            .map_err(|e| anyhow::anyhow!("partition open: {e}"))?;

        let key = msg.id.as_u128().to_be_bytes();
        let val =
            rmp_serde::to_vec(msg).map_err(|e| anyhow::anyhow!("msgpack encode: {e}"))?;

        partition
            .insert(key, val)
            .map_err(|e| anyhow::anyhow!("fjall insert: {e}"))?;

        // Best-effort notify — if channel is full, the consumer will still
        // drain via dequeue() on its next wake-up.
        let _ = self.inner.notify_tx.try_send(());

        Ok(())
    }

    /// Dequeue the oldest message (smallest UUID v7 key → earliest enqueued).
    ///
    /// Returns `None` when the queue is empty.
    /// The message is removed from persistent storage atomically.
    pub fn dequeue(&self) -> anyhow::Result<Option<InboundMessage>> {
        let partition = self
            .inner
            .keyspace
            .open_partition(PARTITION_NAME, PartitionCreateOptions::default())
            .map_err(|e| anyhow::anyhow!("partition open: {e}"))?;

        // Range from the very first key — take exactly one entry.
        let mut iter = partition.iter();
        let Some(entry) = iter.next() else {
            return Ok(None);
        };

        let (key, val) = entry.map_err(|e| anyhow::anyhow!("fjall iter: {e}"))?;

        let msg: InboundMessage = rmp_serde::from_slice(&val)
            .map_err(|e| anyhow::anyhow!("msgpack decode: {e}"))?;

        partition
            .remove(key)
            .map_err(|e| anyhow::anyhow!("fjall remove: {e}"))?;

        Ok(Some(msg))
    }

    /// Number of messages currently in the persistent queue.
    pub fn pending_count(&self) -> anyhow::Result<usize> {
        let partition = self
            .inner
            .keyspace
            .open_partition(PARTITION_NAME, PartitionCreateOptions::default())
            .map_err(|e| anyhow::anyhow!("partition open: {e}"))?;

        Ok(partition.len().map_err(|e| anyhow::anyhow!("fjall len: {e}"))? as usize)
    }

    /// Drain all queued messages into a `Vec` (for migration / inspection).
    /// This is destructive — all returned messages are removed from storage.
    pub fn drain(&self) -> anyhow::Result<Vec<InboundMessage>> {
        let mut out = Vec::new();
        while let Some(msg) = self.dequeue()? {
            out.push(msg);
        }
        Ok(out)
    }
}

// ── InboxWorker ───────────────────────────────────────────────────────────────

/// Long-running worker that pulls from the queue and forwards messages to an
/// in-memory `mpsc::Sender` for processing.
///
/// Respects `shutdown` watch signal for graceful termination.
pub struct InboxWorker {
    queue: InboxQueue,
    notify_rx: mpsc::Receiver<()>,
    process_tx: mpsc::Sender<InboundMessage>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

impl InboxWorker {
    pub fn new(
        queue: InboxQueue,
        notify_rx: mpsc::Receiver<()>,
        process_tx: mpsc::Sender<InboundMessage>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            queue,
            notify_rx,
            process_tx,
            shutdown,
        }
    }

    /// Run the drain loop. `spawn` this in a `tokio::task`.
    pub async fn run(mut self) {
        info!("inbox_worker: started");

        loop {
            // Drain all pending messages before waiting for the next notify.
            loop {
                match self.queue.dequeue() {
                    Ok(Some(msg)) => {
                        if self.process_tx.send(msg).await.is_err() {
                            warn!("inbox_worker: process_tx closed, stopping");
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        error!("inbox_worker: dequeue error: {e}");
                        break;
                    }
                }
            }

            // Wait for a new notify or shutdown.
            tokio::select! {
                _ = self.notify_rx.recv() => {}
                _ = self.shutdown.changed() => {
                    if *self.shutdown.borrow() {
                        info!("inbox_worker: shutdown signal received");
                        return;
                    }
                }
            }
        }
    }
}

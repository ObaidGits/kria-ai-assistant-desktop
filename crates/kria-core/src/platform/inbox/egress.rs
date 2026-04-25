//! Egress router — dispatches [`OutboundMessage`]s to the correct platform
//! adapter.

use std::collections::HashMap;
use std::sync::Arc;

use tracing::error;

use super::adapter::{DeliveryReceipt, EgressAdapter};
use super::{OutboundMessage, Platform};

// ── EgressRouter ──────────────────────────────────────────────────────────────

/// Routes outbound messages to the registered [`EgressAdapter`] for each platform.
///
/// Safe to clone — all clones share the same adapters map.
#[derive(Clone)]
pub struct EgressRouter {
    adapters: Arc<HashMap<String, Arc<dyn EgressAdapter>>>,
}

impl EgressRouter {
    pub fn builder() -> EgressRouterBuilder {
        EgressRouterBuilder {
            adapters: HashMap::new(),
        }
    }

    /// Send `msg` to the correct platform adapter.
    ///
    /// Returns the [`DeliveryReceipt`] from the adapter, or a synthetic
    /// failed receipt if no adapter is registered for the target platform.
    pub async fn send(&self, msg: OutboundMessage) -> DeliveryReceipt {
        let platform_id = msg.conversation.platform.to_string();

        match self.adapters.get(&platform_id) {
            Some(adapter) => adapter.send(msg).await,
            None => {
                error!(
                    platform = %platform_id,
                    "egress_router: no adapter registered for platform"
                );
                DeliveryReceipt {
                    outbound_id: msg.id,
                    native_message_id: None,
                    delivered: false,
                    error: Some(format!("no egress adapter for platform '{platform_id}'")),
                }
            }
        }
    }

    /// Whether an adapter is registered for `platform`.
    pub fn has_adapter(&self, platform: &Platform) -> bool {
        self.adapters.contains_key(&platform.to_string())
    }

    /// Number of registered adapters.
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

pub struct EgressRouterBuilder {
    adapters: HashMap<String, Arc<dyn EgressAdapter>>,
}

impl EgressRouterBuilder {
    /// Register an adapter.  Panics if an adapter for the same platform was
    /// already registered (programming error).
    pub fn register(mut self, adapter: impl EgressAdapter) -> Self {
        let id = adapter.platform_id().to_string();
        if self.adapters.contains_key(&id) {
            panic!("egress_router: duplicate adapter registered for platform '{id}'");
        }
        self.adapters.insert(id, Arc::new(adapter));
        self
    }

    pub fn build(self) -> EgressRouter {
        EgressRouter {
            adapters: Arc::new(self.adapters),
        }
    }
}

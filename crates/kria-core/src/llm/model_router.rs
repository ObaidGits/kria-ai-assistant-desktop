use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::llm::{LlmBackend, local::LocalBackend, cloud::CloudBackend};
use crate::config::KriaConfig;

/// Routing modes for model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingMode {
    Local,
    Gemini,
    External,
}

impl RoutingMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "gemini" => Self::Gemini,
            "external" => Self::External,
            _ => Self::Local,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Gemini => "gemini",
            Self::External => "external",
        }
    }
}

/// Config-driven model router. Selects which backend to use per request.
pub struct ModelRouter {
    mode: RwLock<RoutingMode>,
    local: Option<Arc<dyn LlmBackend>>,
    cloud_clients: RwLock<HashMap<String, Arc<dyn LlmBackend>>>,
}

impl ModelRouter {
    /// Create a model router from configuration.
    pub fn from_config(config: &KriaConfig) -> Self {
        let local = if !config.llm.local_api_url.is_empty() {
            Some(Arc::new(LocalBackend::new(
                config.llm.local_api_url.clone(),
                config.llm.active_model.clone(),
                vec!["text".into()],
                config.llm.context_window,
            )) as Arc<dyn LlmBackend>)
        } else {
            None
        };

        let mut cloud_clients: HashMap<String, Arc<dyn LlmBackend>> = HashMap::new();

        if !config.llm.cloud_api_key.is_empty() && !config.llm.cloud_endpoint.is_empty() {
            let name = if config.llm.cloud_provider.is_empty() {
                "external".to_string()
            } else {
                config.llm.cloud_provider.clone()
            };
            cloud_clients.insert(
                name.clone(),
                Arc::new(CloudBackend::new(
                    config.llm.cloud_endpoint.clone(),
                    config.llm.cloud_api_key.clone(),
                    config.llm.cloud_model_id.clone(),
                    name,
                    vec!["text".into()],
                    Some(30),
                )),
            );
        }

        let mode = RoutingMode::from_str(&config.llm.routing_mode);

        Self {
            mode: RwLock::new(mode),
            local,
            cloud_clients: RwLock::new(cloud_clients),
        }
    }

    /// Get the current routing mode.
    pub async fn mode(&self) -> RoutingMode {
        *self.mode.read().await
    }

    /// Set the routing mode.
    pub async fn set_mode(&self, mode: RoutingMode) {
        *self.mode.write().await = mode;
    }

    /// Route a request to the appropriate backend.
    pub async fn route(&self, _intent: &str) -> Option<Arc<dyn LlmBackend>> {
        let mode = self.mode().await;

        match mode {
            RoutingMode::Local => self.local.clone(),
            RoutingMode::Gemini => {
                let clients = self.cloud_clients.read().await;
                clients.get("gemini").cloned()
                    .or_else(|| self.local.clone())
            }
            RoutingMode::External => {
                let clients = self.cloud_clients.read().await;
                clients.get("external").cloned()
                    .or_else(|| clients.values().next().cloned())
                    .or_else(|| self.local.clone())
            }
        }
    }

    /// Always returns local client (for classification, planning).
    pub fn get_local(&self) -> Option<Arc<dyn LlmBackend>> {
        self.local.clone()
    }

    /// Register a new cloud client at runtime.
    pub async fn register_cloud(
        &self,
        name: String,
        endpoint: String,
        api_key: String,
        model_id: String,
        rpm: Option<u32>,
    ) {
        let client = Arc::new(CloudBackend::new(
            endpoint,
            api_key,
            model_id,
            name.clone(),
            vec!["text".into()],
            rpm,
        ));
        self.cloud_clients.write().await.insert(name, client);
    }

    /// Get status dict for dashboard.
    pub async fn status(&self) -> serde_json::Value {
        let mode = self.mode().await;
        let local_healthy = match &self.local {
            Some(l) => l.health_check().await,
            None => false,
        };
        let cloud_count = self.cloud_clients.read().await.len();

        serde_json::json!({
            "mode": mode.as_str(),
            "local_healthy": local_healthy,
            "local_model": self.local.as_ref().map(|l| l.model_label().to_string()),
            "cloud_backends": cloud_count,
        })
    }
}

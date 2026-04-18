use crate::config::KriaConfig;
use crate::llm::{cloud::CloudBackend, local::LocalBackend, LlmBackend};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    vision_local: Option<Arc<dyn LlmBackend>>,
    cloud_clients: RwLock<HashMap<String, Arc<dyn LlmBackend>>>,
    /// Local API URL (stored for server probing).
    local_api_url: String,
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

        // Create a vision-capable backend if a vision model is explicitly defined
        let vision_local = config
            .llm
            .models
            .iter()
            .find(|m| m.capabilities.contains(&"vision".to_string()) && m.mmproj_file.is_some())
            .map(|vm| {
                Arc::new(LocalBackend::new(
                    config.llm.local_api_url.clone(),
                    vm.name.clone(),
                    vec!["text".into(), "vision".into()],
                    vm.context_window,
                )) as Arc<dyn LlmBackend>
            })
            // If no explicit vision model but local backend exists, treat local
            // as vision-capable: the user may have loaded a vision model (e.g.
            // Qwen2.5-VL with --mmproj) on their llama.cpp server.
            .or_else(|| {
                if !config.llm.local_api_url.is_empty() {
                    Some(Arc::new(LocalBackend::new(
                        config.llm.local_api_url.clone(),
                        config.llm.active_model.clone(),
                        vec!["text".into(), "vision".into()],
                        config.llm.context_window,
                    )) as Arc<dyn LlmBackend>)
                } else {
                    None
                }
            });

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
            vision_local,
            cloud_clients: RwLock::new(cloud_clients),
            local_api_url: config.llm.local_api_url.clone(),
        }
    }

    /// Query the local LLM server's `/v1/models` endpoint and return
    /// the model ID if the server is reachable.
    pub async fn detect_server_model(&self) -> Option<String> {
        if self.local_api_url.is_empty() {
            return None;
        }
        let url = format!("{}/models", self.local_api_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok()?;
        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        body["data"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|m| m["id"].as_str())
            .map(|s| s.to_string())
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
                clients
                    .get("gemini")
                    .cloned()
                    .or_else(|| self.local.clone())
            }
            RoutingMode::External => {
                let clients = self.cloud_clients.read().await;
                clients
                    .get("external")
                    .cloned()
                    .or_else(|| clients.values().next().cloned())
                    .or_else(|| self.local.clone())
            }
        }
    }

    /// Route a request with images to a vision-capable backend.
    /// Falls back to regular routing if no vision backend is available.
    pub async fn route_vision(&self) -> Option<Arc<dyn LlmBackend>> {
        if let Some(ref v) = self.vision_local {
            return Some(v.clone());
        }
        // Fall back to cloud if available (cloud models often support vision)
        let clients = self.cloud_clients.read().await;
        if let Some(client) = clients.values().next() {
            return Some(client.clone());
        }
        // Last resort: use local text model (LLM will respond about image without seeing it)
        self.local.clone()
    }

    /// Check if a vision-capable backend is available.
    pub fn has_vision(&self) -> bool {
        self.vision_local.is_some()
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

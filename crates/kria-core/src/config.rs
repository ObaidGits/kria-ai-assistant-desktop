use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::platform::HardwareTier;

/// Root configuration loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KriaConfig {
    pub llm: LlmConfig,
    pub voice: VoiceConfig,
    pub memory: MemoryConfig,
    pub safety: SafetyConfig,
    pub server: ServerConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub active_model: String,
    pub local_api_url: String,
    pub cloud_provider: String,
    pub cloud_api_key: String,
    pub cloud_model_id: String,
    pub cloud_endpoint: String,
    pub routing_mode: String,
    pub context_window: usize,
    pub max_tokens: usize,
    pub temperature: f32,
    pub max_iterations: usize,
    pub gpu_layers: i32,
    pub models: Vec<LocalModelDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelDef {
    pub name: String,
    pub file: String,
    pub display_name: String,
    pub context_window: usize,
    pub max_tokens: usize,
    pub vram_estimate_gb: f32,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub mmproj_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub enabled: bool,
    pub mode: String,
    pub stt_model: String,
    pub tts_voice: String,
    pub vad_silence_ms: u64,
    pub energy_threshold: f32,
    pub mic_device: String,
    pub speaker_device: String,
    pub push_to_talk_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub max_context_turns: usize,
    pub max_facts: usize,
    pub decay_threshold: f32,
    pub retrieval_top_k: usize,
    pub embedding_model: String,
    pub embedding_dim: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    pub hitl_timeout_secs: u64,
    pub rollback_retention_hours: u64,
    pub tool_timeout_secs: u64,
    pub emergency_mode: bool,
    pub max_concurrent_tools: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub enable_auth: bool,
    pub jwt_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub window_width: u32,
    pub window_height: u32,
}

// ── Defaults ────────────────────────────────────────────────────────

impl Default for KriaConfig {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            voice: VoiceConfig::default(),
            memory: MemoryConfig::default(),
            safety: SafetyConfig::default(),
            server: ServerConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            active_model: "phi-4-mini".into(),
            local_api_url: "http://127.0.0.1:8080/v1".into(),
            cloud_provider: String::new(),
            cloud_api_key: String::new(),
            cloud_model_id: String::new(),
            cloud_endpoint: String::new(),
            routing_mode: "local".into(),
            context_window: 4096,
            max_tokens: 2048,
            temperature: 0.6,
            max_iterations: 10,
            gpu_layers: -1,
            models: Vec::new(),
        }
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "push_to_talk".into(),
            stt_model: "ggml-base.en.bin".into(),
            tts_voice: "en_US-lessac-high".into(),
            vad_silence_ms: 1000,
            energy_threshold: 2000.0,
            mic_device: "auto".into(),
            speaker_device: "auto".into(),
            push_to_talk_key: "ctrl+space".into(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_context_turns: 20,
            max_facts: 1000,
            decay_threshold: 0.05,
            retrieval_top_k: 5,
            embedding_model: "all-MiniLM-L6-v2".into(),
            embedding_dim: 384,
        }
    }
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            hitl_timeout_secs: 30,
            rollback_retention_hours: 72,
            tool_timeout_secs: 30,
            emergency_mode: false,
            max_concurrent_tools: 3,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 8088,
            enable_auth: false,
            jwt_secret: String::new(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            window_width: 1200,
            window_height: 800,
        }
    }
}

// ── Loading ─────────────────────────────────────────────────────────

impl KriaConfig {
    /// Load config from default paths (convenience method).
    pub fn load(override_path: Option<&Path>) -> anyhow::Result<Self> {
        let paths = crate::platform::paths::KriaPaths::resolve();
        let default_path = paths.config_dir.join("config.toml");
        load_config(&default_path, override_path)
    }

    /// Resolve standard data paths.
    pub fn resolve_paths(&self) -> anyhow::Result<crate::platform::paths::KriaPaths> {
        Ok(crate::platform::paths::KriaPaths::resolve())
    }
}

/// Load config from default.toml + optional user override.
pub fn load_config(default_path: &Path, override_path: Option<&Path>) -> anyhow::Result<KriaConfig> {
    let mut config: KriaConfig = if default_path.exists() {
        let text = std::fs::read_to_string(default_path)?;
        toml::from_str(&text)?
    } else {
        KriaConfig::default()
    };

    // Merge user override (if exists)
    if let Some(p) = override_path {
        if p.exists() {
            let text = std::fs::read_to_string(p)?;
            let user: KriaConfig = toml::from_str(&text)?;
            merge_config(&mut config, &user);
        }
    }

    // Environment variable overrides
    if let Ok(v) = std::env::var("KRIA_LLM_MODE") {
        config.llm.routing_mode = v;
    }
    if let Ok(v) = std::env::var("KRIA_CLOUD_API_KEY") {
        config.llm.cloud_api_key = v;
    }

    Ok(config)
}

fn merge_config(base: &mut KriaConfig, user: &KriaConfig) {
    if !user.llm.active_model.is_empty() {
        base.llm.active_model = user.llm.active_model.clone();
    }
    if !user.llm.routing_mode.is_empty() {
        base.llm.routing_mode = user.llm.routing_mode.clone();
    }
    if !user.llm.cloud_api_key.is_empty() {
        base.llm.cloud_api_key = user.llm.cloud_api_key.clone();
    }
    if !user.llm.cloud_endpoint.is_empty() {
        base.llm.cloud_endpoint = user.llm.cloud_endpoint.clone();
    }
    if user.voice.enabled {
        base.voice = user.voice.clone();
    }
    if user.safety.emergency_mode {
        base.safety.emergency_mode = true;
    }
}

/// Select model config based on hardware tier.
pub fn auto_select_model(tier: HardwareTier) -> &'static str {
    match tier {
        HardwareTier::Lite => "qwen2.5-3b",
        HardwareTier::Standard => "phi-4-mini",
        HardwareTier::Performance | HardwareTier::High => "qwen2.5-vl-7b",
    }
}

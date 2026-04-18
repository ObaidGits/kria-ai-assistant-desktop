use crate::platform::HardwareTier;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Root configuration loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KriaConfig {
    pub llm: LlmConfig,
    pub voice: VoiceConfig,
    pub memory: MemoryConfig,
    pub safety: SafetyConfig,
    pub agent: AgentConfig,
    pub server: ServerConfig,
    pub ui: UiConfig,
    pub search: SearchConfig,
    pub mcp: McpConfig,
    pub telegram: TelegramConfig,
    pub hardware: HardwareConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub language: String,
    pub partial_update_ms: u64,
    pub confidence_threshold: f32,
    pub noise_suppression_mode: String,
    pub follow_system_default_mic: bool,
    pub follow_system_default_speaker: bool,
    pub persist_transcripts: bool,
    pub persist_raw_audio: bool,
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

/// Agent intelligence behavior controls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentConfig {
    /// `conservative`, `balanced`, or `aggressive`.
    pub autonomy_profile: String,
    /// Minimum confidence before autonomous action on ambiguous tasks.
    pub min_confidence_to_act: f32,
    /// If confidence is below this threshold, ask a targeted clarification.
    pub clarify_threshold: f32,
    /// Require explicit internal planning for multi-step tasks.
    pub require_plan_for_complex_tasks: bool,
    /// Require observed tool evidence before claiming completion.
    pub require_evidence_for_completion: bool,
    /// Maximum tool-action rounds per turn.
    pub max_tool_rounds: usize,
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
    pub language: String,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    pub font_scale: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Search engine backend: "duckduckgo" or "searxng"
    pub engine: String,
    /// SearXNG instance URL (when engine = "searxng")
    pub searxng_url: String,
    /// News RSS feeds (comma-separated or Vec)
    pub news_feeds: Vec<String>,
}

/// Telegram integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: String,
    /// Comma-separated allowed chat IDs. Empty = allow all.
    pub allowed_chat_ids: String,
    /// Whether to auto-register the Telegram MCP server on startup.
    pub auto_start: bool,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            allowed_chat_ids: String::new(),
            auto_start: true,
        }
    }
}

/// MCP (Model Context Protocol) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub servers: Vec<McpServerConfig>,
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_trust_level")]
    pub trust_level: String,
    #[serde(default)]
    pub tool_overrides: std::collections::HashMap<String, String>,
}

/// Hardware tier configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HardwareConfig {
    /// Manual tier override: "lite", "standard", "performance", "high". Empty = auto-detect.
    pub tier: String,
    /// Maximum context tokens (0 = auto based on tier).
    pub max_context_tokens: usize,
    /// GPU layers for llama.cpp (-1 = auto based on tier).
    pub gpu_layers: i32,
    /// Thread count for inference (0 = auto based on tier).
    pub threads: usize,
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            tier: String::new(),
            max_context_tokens: 0,
            gpu_layers: -1,
            threads: 0,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_trust_level() -> String {
    "YELLOW".into()
}

// ── Defaults ────────────────────────────────────────────────────────

impl Default for KriaConfig {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            voice: VoiceConfig::default(),
            memory: MemoryConfig::default(),
            safety: SafetyConfig::default(),
            agent: AgentConfig::default(),
            server: ServerConfig::default(),
            ui: UiConfig::default(),
            search: SearchConfig::default(),
            mcp: McpConfig::default(),
            telegram: TelegramConfig::default(),
            hardware: HardwareConfig::default(),
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
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
            energy_threshold: 0.02,
            mic_device: "auto".into(),
            speaker_device: "auto".into(),
            push_to_talk_key: "ctrl+space".into(),
            language: "auto".into(),
            partial_update_ms: 2000,
            confidence_threshold: 0.30,
            noise_suppression_mode: "off".into(),
            follow_system_default_mic: true,
            follow_system_default_speaker: true,
            persist_transcripts: true,
            persist_raw_audio: false,
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

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            autonomy_profile: "balanced".into(),
            min_confidence_to_act: 0.55,
            clarify_threshold: 0.40,
            require_plan_for_complex_tasks: true,
            require_evidence_for_completion: true,
            max_tool_rounds: 10,
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
            language: "en".into(),
            high_contrast: false,
            reduce_motion: false,
            font_scale: 1.0,
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            engine: "duckduckgo".into(),
            searxng_url: "http://localhost:8888".into(),
            news_feeds: vec![
                "https://feeds.arstechnica.com/arstechnica/index".into(),
                "https://hnrss.org/frontpage".into(),
            ],
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

    /// Save the current config to the user override file (`~/.kria/config.toml`).
    pub fn save(&self) -> anyhow::Result<()> {
        let paths = crate::platform::paths::KriaPaths::resolve();
        let user_config_path = paths.user_config();
        if let Some(parent) = user_config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml_str = toml::to_string_pretty(self)?;
        std::fs::write(&user_config_path, toml_str)?;
        tracing::info!(path = %user_config_path.display(), "config saved");
        Ok(())
    }
}

/// Load config from default.toml + optional user override.
pub fn load_config(
    default_path: &Path,
    override_path: Option<&Path>,
) -> anyhow::Result<KriaConfig> {
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
    if let Ok(v) = std::env::var("KRIA_TIER") {
        if !v.trim().is_empty() {
            config.hardware.tier = v;
        }
    }
    if let Ok(v) = std::env::var("KRIA_AGENT_AUTONOMY_PROFILE") {
        if !v.trim().is_empty() {
            config.agent.autonomy_profile = v;
        }
    }
    if let Ok(v) = std::env::var("KRIA_AGENT_MAX_TOOL_ROUNDS") {
        if let Ok(parsed) = v.parse::<usize>() {
            if parsed > 0 {
                config.agent.max_tool_rounds = parsed;
            }
        }
    }
    if let Ok(v) = std::env::var("KRIA_AGENT_MIN_CONFIDENCE") {
        if let Ok(parsed) = v.parse::<f32>() {
            if (0.0..=1.0).contains(&parsed) {
                config.agent.min_confidence_to_act = parsed;
            }
        }
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
    if user.voice != VoiceConfig::default() {
        base.voice = user.voice.clone();
    }
    if user.safety.emergency_mode {
        base.safety.emergency_mode = true;
    }
    if user.agent != AgentConfig::default() {
        base.agent = user.agent.clone();
    }
    if !user.hardware.tier.is_empty() {
        base.hardware.tier = user.hardware.tier.clone();
    }
    if user.hardware.max_context_tokens > 0 {
        base.hardware.max_context_tokens = user.hardware.max_context_tokens;
    }
    if user.hardware.gpu_layers >= 0 {
        base.hardware.gpu_layers = user.hardware.gpu_layers;
    }
    if user.hardware.threads > 0 {
        base.hardware.threads = user.hardware.threads;
    }
}

/// Load MCP server configs from `mcp_servers.json` next to the running executable
/// or in the standard config directory. Merges into the existing McpConfig.
pub fn load_mcp_servers(config: &mut KriaConfig) {
    // Search order: alongside exe, then in config dir
    let candidates: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        // 1. Next to the executable (dev mode: workspace config/)
        if let Ok(exe) = std::env::current_exe() {
            tracing::debug!("[MCP config] exe path: {}", exe.display());
            if let Some(parent) = exe.parent() {
                // In dev builds the exe is in target/debug, so walk up to find config/
                let mut dir = parent.to_path_buf();
                for i in 0..5 {
                    let candidate = dir.join("config").join("mcp_servers.json");
                    tracing::debug!(
                        "[MCP config] checking candidate [{}]: {}",
                        i,
                        candidate.display()
                    );
                    if candidate.exists() {
                        tracing::info!(
                            "[MCP config] found mcp_servers.json at: {}",
                            candidate.display()
                        );
                        v.push(candidate);
                        break;
                    }
                    if !dir.pop() {
                        tracing::debug!("[MCP config] reached filesystem root, stopping walk");
                        break;
                    }
                }
            }
        } else {
            tracing::warn!("[MCP config] could not determine current exe path");
        }
        // 2. Standard config dir (~/.kria/mcp_servers.json)
        let paths = crate::platform::paths::KriaPaths::resolve();
        let user_cfg = paths.config_dir.join("mcp_servers.json");
        tracing::debug!("[MCP config] user config candidate: {}", user_cfg.display());
        v.push(user_cfg);
        v
    };

    for path in &candidates {
        if path.exists() {
            tracing::info!("[MCP config] reading: {}", path.display());
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    match serde_json::from_str::<McpConfig>(&text) {
                        Ok(mcp_cfg) => {
                            let enabled = mcp_cfg.servers.iter().filter(|s| s.enabled).count();
                            tracing::info!(
                                "[MCP config] loaded {} server(s) ({} enabled) from {}",
                                mcp_cfg.servers.len(),
                                enabled,
                                path.display()
                            );
                            // Merge: JSON servers supplement TOML servers (no duplicates by name)
                            for server in mcp_cfg.servers {
                                if !config.mcp.servers.iter().any(|s| s.name == server.name) {
                                    tracing::info!(
                                        "[MCP config] adding server '{}' (enabled={}) from JSON",
                                        server.name,
                                        server.enabled
                                    );
                                    config.mcp.servers.push(server);
                                } else {
                                    tracing::debug!(
                                        "[MCP config] server '{}' already in config — skipping duplicate",
                                        server.name
                                    );
                                }
                            }
                            return;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "[MCP config] failed to parse {}: {}",
                                path.display(),
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("[MCP config] failed to read {}: {}", path.display(), e);
                }
            }
        }
    }
    tracing::warn!("[MCP config] no mcp_servers.json found in any candidate path — MCP servers from TOML config only");
}

/// Select model config based on hardware tier.
pub fn auto_select_model(tier: HardwareTier) -> &'static str {
    match tier {
        HardwareTier::Lite => "qwen2.5-3b",
        HardwareTier::Standard => "phi-4-mini",
        HardwareTier::Performance | HardwareTier::High => "qwen2.5-vl-7b",
    }
}

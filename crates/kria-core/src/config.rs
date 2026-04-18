use crate::platform::HardwareTier;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Root configuration loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    pub orchestrator: OrchestratorConfig,
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

/// Hardware orchestrator configuration — manages llama-server lifecycle and
/// dynamic GPU layer offloading based on real-time VRAM/RAM telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorConfig {
    /// Enable the hardware orchestrator. When false, llama-server is not managed.
    pub enabled: bool,
    /// Telemetry polling interval in seconds.
    pub poll_interval_secs: u64,
    /// Free VRAM (MB) below which a yield swap is triggered (sustained).
    pub yield_threshold_mb: u64,
    /// Free VRAM (MB) below which an emergency swap fires immediately.
    pub emergency_threshold_mb: u64,
    /// Free VRAM (MB) above which recovery to higher ngl is allowed (sustained).
    pub recover_threshold_mb: u64,
    /// Minimum seconds between non-emergency transitions.
    pub cooldown_secs: u64,
    /// Maximum swap transitions per hour before locking state.
    pub max_transitions_per_hour: u32,
    /// Minimum |Δngl| required to trigger a swap (prevents micro-adjustments).
    pub min_ngl_delta: u32,
    /// VRAM safety margin (MB) reserved to prevent OOM.
    pub safety_margin_mb: u64,
    /// Path or name of the llama-server binary.
    pub llama_server_binary: String,
    /// Enable flash attention in llama-server.
    pub flash_attention: bool,
    /// Lock model weights in RAM (mlock).
    pub mlock: bool,
    /// Batch size for llama-server.
    pub batch_size: u32,
    /// macOS: free RAM (MB) below which a yield triggers.
    pub macos_yield_ram_mb: u64,
    /// macOS: free RAM (MB) below which an emergency triggers.
    pub macos_emergency_ram_mb: u64,
    /// macOS: free RAM (MB) above which recovery is allowed.
    pub macos_recover_ram_mb: u64,
    /// Model profile for VRAM budget calculations.
    pub model_profile: ModelProfile,
}

/// Per-model memory profile used by the layer strategy calculator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelProfile {
    /// Total transformer layers in the model.
    pub total_layers: u32,
    /// Approximate VRAM per offloaded layer (MB).
    pub per_layer_vram_mb: u32,
    /// Base VRAM overhead for CUDA context + embeddings (MB).
    pub base_vram_overhead_mb: u32,
    /// KV cache VRAM per 1024 context tokens (MB).
    pub kv_per_1k_ctx_mb: u32,
    /// Minimum context window (hard floor — never go below).
    pub min_context: u32,
    /// Maximum context window.
    pub max_context: u32,
    /// Whether the model has a vision projector (mmproj).
    pub has_vision_projector: bool,
    /// Approximate VRAM used by the vision projector (MB). Only relevant when
    /// `has_vision_projector` is true.
    #[serde(default)]
    pub mmproj_vram_mb: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 2,
            yield_threshold_mb: 512,
            emergency_threshold_mb: 128,
            recover_threshold_mb: 2048,
            cooldown_secs: 60,
            max_transitions_per_hour: 6,
            min_ngl_delta: 3,
            safety_margin_mb: 256,
            llama_server_binary: "llama-server".into(),
            flash_attention: true,
            mlock: true,
            batch_size: 256,
            macos_yield_ram_mb: 2048,
            macos_emergency_ram_mb: 1024,
            macos_recover_ram_mb: 4096,
            model_profile: ModelProfile::default(),
        }
    }
}

impl Default for ModelProfile {
    fn default() -> Self {
        Self {
            total_layers: 28,
            per_layer_vram_mb: 165,
            base_vram_overhead_mb: 200,
            kv_per_1k_ctx_mb: 100,
            min_context: 2048,
            max_context: 8192,
            has_vision_projector: true,
            mmproj_vram_mb: 1300,
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
    ///
    /// Searches for the project's `config/default.toml` by walking up from the
    /// current exe / CWD (covers both dev and installed layouts). If found it is
    /// used as the base config and `~/.kria/config.toml` is merged on top as a
    /// user override.  If no project default is found, `~/.kria/config.toml` is
    /// used as the sole config (production fallback).
    pub fn load(override_path: Option<&Path>) -> anyhow::Result<Self> {
        let paths = crate::platform::paths::KriaPaths::resolve();
        let user_config = paths.user_config();

        // Try to locate the project's config/default.toml by walking up from exe
        // and CWD (whichever finds it first).
        let project_default = Self::find_project_default();

        match project_default {
            Some(ref base_path) => {
                eprintln!("[config] using project default: {}", base_path.display());
                // Use project default.toml as base, merge user config on top
                let user_override = if user_config.exists() {
                    eprintln!("[config] merging user override: {}", user_config.display());
                    Some(user_config.as_path())
                } else {
                    None
                };
                let cfg = load_config(base_path, override_path.or(user_override))?;
                eprintln!("[config] loaded {} model(s), orchestrator.enabled={}", cfg.llm.models.len(), cfg.orchestrator.enabled);
                Ok(cfg)
            }
            None => {
                eprintln!("[config] no project default.toml found, falling back to {}", user_config.display());
                // No project default found → fall back to user config as sole source
                load_config(&user_config, override_path)
            }
        }
    }

    /// Walk up from the current exe and CWD looking for `config/default.toml`.
    fn find_project_default() -> Option<std::path::PathBuf> {
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                roots.push(parent.to_path_buf());
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }

        for start in roots {
            let mut dir = Some(start.as_path());
            while let Some(d) = dir {
                let candidate = d.join("config").join("default.toml");
                if candidate.exists() {
                    return Some(candidate);
                }
                dir = d.parent();
                // Don't walk all the way to /
                if dir.map(|d| d == std::path::Path::new("/")).unwrap_or(true) {
                    break;
                }
            }
        }
        None
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

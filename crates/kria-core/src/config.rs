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
    pub colab: ColabConfig,
    pub routing: RoutingConfig,
    pub image_generation: ImageGenerationConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ColabConfig {
    /// Enable Colab cloud tier controls.
    pub enabled: bool,
    /// MCP server name used for the official Colab sidecar.
    pub mcp_server_name: String,
    /// Browser/session connect timeout budget.
    pub connect_timeout_secs: u64,
    /// Keepalive interval while cloud tasks are active.
    pub keepalive_interval_secs: u64,
    /// Periodic checkpoint interval for long-running training.
    pub checkpoint_interval_secs: u64,
    /// Whether local insufficiency can auto-escalate to Colab.
    pub auto_escalate: bool,
    /// Fallback to local runtime if Colab is unavailable.
    pub fallback_to_local: bool,
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
    /// Whether to emit live (partial) transcripts while the user is still speaking.
    /// Disabled by default for the v1 CLI backend because each partial spawns a
    /// fresh `whisper-cpp` subprocess that cold-loads the model — this piles up
    /// and starves the final transcription, causing STT timeouts. Re-enable
    /// once a persistent backend (whisper-server / whisper-rs / v2) is in use.
    pub enable_partial_transcripts: bool,
    pub confidence_threshold: f32,
    pub noise_suppression_mode: String,
    pub follow_system_default_mic: bool,
    pub follow_system_default_speaker: bool,
    pub persist_transcripts: bool,
    pub persist_raw_audio: bool,
    /// Pipeline engine: `"v1"` (legacy, CLI-subprocess) or `"v2"` (in-process streaming).
    /// Default `"v1"` until v2 is validated on every tier/platform.
    pub engine: String,
    /// Hardware tier override: `"auto" | "s" | "a" | "c"`. `auto` = derive from
    /// `HardwareTier` at startup.
    pub tier: String,
    /// Optional explicit STT engine: `"auto" | "whisper-rs" | "whisper-cuda" | "sidecar"`.
    pub stt_engine: String,
    /// Optional explicit TTS engine: `"auto" | "piper-cli" | "piper-rs"`.
    pub tts_engine: String,
    pub wake_word: WakeWordConfig,
    pub aec: AecConfig,
    pub barge_in: BargeInConfig,
    pub post_edit: PostEditConfig,
}

/// Wake-word ("Hey Ria") settings — Phase 4.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WakeWordConfig {
    pub enabled: bool,
    /// Path to the openWakeWord ONNX model for the keyword head.
    pub model_path: String,
    /// 0.0..1.0; 0.5 = "balanced".
    pub sensitivity: f32,
    /// Aliases that should also wake the assistant (informational; the trained
    /// model itself covers all aliases — listed here for documentation/UX).
    pub aliases: Vec<String>,
}

/// Acoustic Echo Cancellation settings — Phase 3. STRICTLY opt-in via the
/// `aec` cargo feature on `kria-voice`. When the feature is not compiled,
/// these fields are accepted for forward compatibility but ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AecConfig {
    pub enabled: bool,
    /// `"low" | "medium" | "high"` — maps to WebRTC APM NS aggressiveness.
    pub aggressiveness: String,
}

/// Barge-in (interrupt-while-speaking) settings — Phase 2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BargeInConfig {
    pub enabled: bool,
    /// Minimum continuous speech (ms) before aborting playback. Debounces
    /// AEC residue and single-cough false positives.
    pub min_speech_ms: u64,
}

/// LLM post-edit / Hinglish fix-pass settings — Phase 5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PostEditConfig {
    pub enabled: bool,
    /// Model name (must exist in the configured local model set).
    /// Preferred: `"qwen2.5-3b-instruct"`. Fallback: `"phi-4-mini"`.
    pub model: String,
    /// `"always" | "on_low_confidence"`.
    pub mode: String,
    /// Hard timeout per tier — overridden by `VoiceTier` when not set explicitly.
    pub timeout_ms: u64,
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
    /// Minimum |Δngl| required to trigger a swap (prevents micro-adjustments
    /// on scale-down from Idle/Pressured).
    pub min_ngl_delta: u32,
    /// Minimum ngl increase required to trigger a scale-up swap from Recovering.
    /// Asymmetric: higher threshold favours stability over reclaim.
    pub min_ngl_delta_up: u32,
    /// VRAM safety margin (MB) reserved to prevent OOM.
    pub safety_margin_mb: u64,
    /// Deadband (MB) above yield_threshold_mb required to leave Pressured state.
    /// Prevents oscillation when VRAM hovers at the threshold.
    pub hysteresis_band_mb: u64,
    /// Minimum seconds VRAM must be below yield_threshold before triggering swap.
    pub pressure_dwell_secs: u64,
    /// Milliseconds VRAM must be below emergency_threshold before triggering
    /// emergency swap. Guards against transient driver spikes.
    pub emergency_dwell_ms: u64,
    /// Minimum seconds of stable recovery headroom before scaling back up.
    pub recovery_dwell_secs: u64,
    /// Maximum seconds any watchdog state can persist before forcing a resync.
    pub state_max_dwell_secs: u64,
    /// Separate rate budget for emergency transitions (per hour). Never zero.
    /// Keeps the emergency path from thrashing while still self-throttling.
    pub max_emergency_transitions_per_hour: u32,
    /// Path or name of the llama-server binary.
    pub llama_server_binary: String,
    /// Directory passed to llama-server via `--slot-save-path`. Required for
    /// `/slots/{id}?action=save|restore` (used by the Tier B drop-and-swap
    /// path to persist KV cache across hard process restarts).
    /// Empty string -> resolve at spawn time to `<system_tmp>/kria_llama_slots`.
    pub slot_save_path: String,
    /// Enable flash attention in llama-server.
    pub flash_attention: bool,
    /// Lock model weights in RAM (mlock).
    pub mlock: bool,
    /// Batch size for llama-server.
    pub batch_size: u32,
    /// Max seconds to wait for graceful server stop before kill escalation.
    pub graceful_stop_timeout_secs: u64,
    /// Max seconds to wait for llama-server health endpoint readiness on spawn.
    pub health_check_timeout_secs: u64,
    /// Max seconds to wait for ephemeral port discovery from llama-server logs.
    pub port_discovery_timeout_secs: u64,
    /// Max seconds to wait for GPU memory release after shutdown/swap.
    pub vram_release_timeout_secs: u64,
    /// Minimum cooldown between automatic orchestrator restart attempts.
    pub restart_cooldown_secs: u64,
    /// Backoff delay (milliseconds) before fallback spawn after restart failure.
    pub restart_backoff_ms: u64,
    /// Enable idle-time llama-server release to free GPU memory when no turns are running.
    pub idle_release_enabled: bool,
    /// Idle duration (seconds) after which llama-server is released.
    pub idle_release_after_secs: u64,
    /// Poll interval (seconds) for idle-release checks in desktop runtime.
    pub idle_release_check_interval_secs: u64,
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
    /// Minimum ngl required to enable vision. Config-driven per model
    /// (replaces the hardcoded `ngl >= 15` magic constant).
    #[serde(default = "default_vision_min_ngl")]
    pub vision_min_ngl: u32,
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
            min_ngl_delta_up: 6,
            safety_margin_mb: 512,
            hysteresis_band_mb: 256,
            pressure_dwell_secs: 5,
            emergency_dwell_ms: 750,
            recovery_dwell_secs: 30,
            state_max_dwell_secs: 300,
            max_emergency_transitions_per_hour: 3,
            llama_server_binary: "llama-server".into(),
            slot_save_path: String::new(),
            // Safety: dangerous flags default to OFF. The orchestrator's
            // `tune_for_tier()` opts in only when free RAM/VRAM is provably
            // sufficient. Hardcoding mlock=true on a 16GB laptop with a 5GB
            // model is a guaranteed system freeze.
            flash_attention: false,
            mlock: false,
            batch_size: 128,
            graceful_stop_timeout_secs: 5,
            health_check_timeout_secs: 120,
            port_discovery_timeout_secs: 60,
            vram_release_timeout_secs: 5,
            restart_cooldown_secs: 10,
            restart_backoff_ms: 350,
            idle_release_enabled: true,
            idle_release_after_secs: 300,
            idle_release_check_interval_secs: 10,
            macos_yield_ram_mb: 2048,
            macos_emergency_ram_mb: 1024,
            macos_recover_ram_mb: 4096,
            model_profile: ModelProfile::default(),
        }
    }
}

impl OrchestratorConfig {
    /// Adapt memory-sensitive defaults to the detected hardware tier.
    ///
    /// This is the **freeze prevention** layer: a 16 GB laptop loading a
    /// 5 GB Qwen2.5-VL with `mlock=true` + `flash_attention=true` +
    /// `batch_size=256` will OOM-freeze. Calling this method right after
    /// hardware detection clamps the config to values that are safe for
    /// the actual machine.
    ///
    /// Inputs:
    /// * `tier` — coarse classification (Lite/Standard/Performance/High)
    /// * `total_ram_mb` — physical RAM (system-wide)
    /// * `vram_mb` — discrete GPU VRAM, if any
    /// * `model_size_mb` — on-disk size of the active GGUF (used to decide
    ///   whether `--mlock` would actually fit)
    ///
    /// Rules (conservative on purpose):
    /// * `mlock` only enabled when `total_ram_mb >= model_size_mb * 2 + 4 GB`
    ///   AND tier is Performance or High.
    /// * `flash_attention` enabled only on Performance/High tiers (it adds
    ///   intermediate VRAM allocations that can tip a 6 GB GPU into OOM).
    /// * `batch_size` clamped per tier: 64 / 96 / 128 / 256.
    /// * `safety_margin_mb` raised on lower tiers so the watchdog leaves
    ///   more headroom for the desktop/browser/IDE.
    /// * `poll_interval_secs` raised on Lite to reduce telemetry overhead.
    pub fn tune_for_tier(
        &mut self,
        tier: crate::platform::detect::HardwareTier,
        total_ram_mb: u64,
        vram_mb: Option<u64>,
        model_size_mb: u64,
    ) {
        use crate::platform::detect::HardwareTier;

        // Clamp batch_size to a per-tier ceiling.
        let max_batch: u32 = match tier {
            HardwareTier::Lite => 64,
            HardwareTier::Standard => 96,
            HardwareTier::Performance => 128,
            HardwareTier::High => 256,
        };
        if self.batch_size > max_batch {
            self.batch_size = max_batch;
        }

        // flash_attention: only on tiers with discrete GPU and enough VRAM.
        let flash_safe = matches!(tier, HardwareTier::Performance | HardwareTier::High)
            && vram_mb.map(|v| v >= 6 * 1024).unwrap_or(false);
        if !flash_safe {
            self.flash_attention = false;
        }

        // mlock: requires headroom of model_size + 4 GB on top of model RAM,
        // and only reliable on Performance/High tiers.
        let mlock_safe = matches!(tier, HardwareTier::Performance | HardwareTier::High)
            && total_ram_mb >= model_size_mb.saturating_add(4 * 1024).saturating_add(model_size_mb);
        if !mlock_safe {
            self.mlock = false;
        }

        // Safety margin (MB held back by the watchdog for OS/desktop apps).
        // On low-RAM tiers we want a much larger margin to keep the system
        // responsive even when the model spikes.
        let min_safety = match tier {
            HardwareTier::Lite => 1024,
            HardwareTier::Standard => 768,
            HardwareTier::Performance => 512,
            HardwareTier::High => 256,
        };
        if self.safety_margin_mb < min_safety {
            self.safety_margin_mb = min_safety;
        }

        // Telemetry poll interval — lower tiers should not hammer NVML/sysinfo.
        if matches!(tier, HardwareTier::Lite) && self.poll_interval_secs < 5 {
            self.poll_interval_secs = 5;
        }

        // Idle release: aggressive on Lite/Standard so the model is dropped
        // when not in use; lazy on Performance/High where users want low
        // first-token latency.
        match tier {
            HardwareTier::Lite => {
                self.idle_release_enabled = true;
                if self.idle_release_after_secs > 60 {
                    self.idle_release_after_secs = 60;
                }
            }
            HardwareTier::Standard => {
                self.idle_release_enabled = true;
                if self.idle_release_after_secs > 180 {
                    self.idle_release_after_secs = 180;
                }
            }
            _ => {}
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
            vision_min_ngl: 15,
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

fn default_vision_min_ngl() -> u32 {
    15
}

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
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

impl Default for ColabConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mcp_server_name: "colab-mcp".into(),
            connect_timeout_secs: 60,
            keepalive_interval_secs: 120,
            checkpoint_interval_secs: 300,
            auto_escalate: true,
            fallback_to_local: true,
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
            enable_partial_transcripts: false,
            confidence_threshold: 0.30,
            noise_suppression_mode: "off".into(),
            follow_system_default_mic: true,
            follow_system_default_speaker: true,
            persist_transcripts: true,
            persist_raw_audio: false,
            engine: "v1".into(),
            tier: "auto".into(),
            stt_engine: "auto".into(),
            tts_engine: "auto".into(),
            wake_word: WakeWordConfig::default(),
            aec: AecConfig::default(),
            barge_in: BargeInConfig::default(),
            post_edit: PostEditConfig::default(),
        }
    }
}

impl Default for WakeWordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_path: "models/wake/hey_ria.onnx".into(),
            sensitivity: 0.5,
            aliases: vec![
                "hey ria".into(),
                "hey riya".into(),
                "hello ria".into(),
                "hello riya".into(),
            ],
        }
    }
}

impl Default for AecConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            aggressiveness: "medium".into(),
        }
    }
}

impl Default for BargeInConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_speech_ms: 180,
        }
    }
}

impl Default for PostEditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "qwen2.5-3b-instruct".into(),
            mode: "on_low_confidence".into(),
            timeout_ms: 0, // 0 = use tier default
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
                tracing::debug!(path = %base_path.display(), "config: using project default");
                // Use project default.toml as base, merge user config on top
                let user_override = if user_config.exists() {
                    tracing::debug!(path = %user_config.display(), "config: merging user override");
                    Some(user_config.as_path())
                } else {
                    None
                };
                let cfg = load_config(base_path, override_path.or(user_override))?;
                tracing::debug!(
                    model_count = cfg.llm.models.len(),
                    orchestrator_enabled = cfg.orchestrator.enabled,
                    "config: loaded"
                );
                Ok(cfg)
            }
            None => {
                tracing::debug!(
                    path = %user_config.display(),
                    "config: project default.toml not found, using user config"
                );
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
    if let Ok(v) = std::env::var("KRIA_COLAB_ENABLED") {
        if let Some(parsed) = parse_env_bool(&v) {
            config.colab.enabled = parsed;
        }
    }
    if let Ok(v) = std::env::var("KRIA_COLAB_MCP_SERVER") {
        if !v.trim().is_empty() {
            config.colab.mcp_server_name = v;
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
    if user.colab != ColabConfig::default() {
        base.colab = user.colab.clone();
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

/// Semantic routing configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoutingConfig {
    /// Embedding model identifier (passed to fastembed-rs).
    pub embedding_model: String,
    /// Cache subdirectory under ~/.kria.
    pub cache_dir: String,
    /// Enable llguidance / json_schema constrained decoding.
    pub grammar_enabled: bool,
    /// OOD z-score threshold (relative, model-agnostic).
    pub ood_z_threshold: f32,
    /// OOD entropy fraction of H_max threshold.
    pub ood_entropy_threshold: f32,
    /// Margin below which two domains trigger multi-intent check.
    pub multi_intent_margin: f32,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            embedding_model: "multilingual-e5-small".into(),
            cache_dir: "cache/router".into(),
            grammar_enabled: true,
            ood_z_threshold: 0.5,
            ood_entropy_threshold: 0.85,
            multi_intent_margin: 0.04,
        }
    }
}

// ─── Image Generation Configuration ──────────────────────────────────────────

/// Image generation subsystem configuration.
///
/// Controls ComfyUI sidecar lifecycle, model selection, Tier B swap budget,
/// cloud fallback policy, and background pre-warm strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageGenerationConfig {
    /// Enable image generation tool. When false, `generate_image` returns a
    /// friendly "feature disabled" message.
    pub enabled: bool,

    /// Manual image-gen tier override: "s_high_res" | "a_standard" |
    /// "b_drop_swap" | "c_reject_or_cloud". Empty string = auto-detect from
    /// VRAM at request time. Also overridable via KRIA_IMG_TIER env var.
    pub tier_override: String,

    /// Port for the headless ComfyUI API server.
    pub comfy_port: u16,

    /// Directory where ComfyUI venv is provisioned (`uv sync`).
    /// Relative paths are expanded under `~/.kria/`.
    pub comfy_venv_dir: String,

    /// Directory for ComfyUI model checkpoints (GGUF).
    pub comfy_models_dir: String,

    /// Directory for generated image output.
    pub output_dir: String,

    /// Directory for conditioning tensor cache (SHA-256 indexed).
    pub conditioning_cache_dir: String,

    /// Maximum MiB for the conditioning tensor LRU cache.
    pub conditioning_cache_max_mb: u64,

    /// Idle timeout in seconds before the ComfyUI sidecar unloads Flux from
    /// VRAM (keeping Python/CUDA context alive). 0 = never.
    pub idle_unload_secs: u64,

    /// Pre-warm strategy: "auto" | "always" | "never".
    /// "auto" → Tier S/A pre-warm fully at boot (after 30s delay);
    ///           Tier B pre-warm interpreter only.
    pub prewarm: String,

    /// Seconds after app window-ready to start the background pre-warm task.
    pub prewarm_delay_secs: u64,

    /// Cloud fallback policy on Tier C: "auto_offer" | "opt_in" | "off".
    /// "auto_offer" = ask once per session then use without prompting.
    pub cloud_fallback: String,

    /// Pollinations.ai base URL (cloud fallback, no key required).
    pub pollinations_base_url: String,

    /// Maximum concurrent image jobs per session (Tier S/A only).
    pub max_concurrent_jobs: usize,

    /// Maximum Tier B drop-and-swap jobs queued before rejecting new ones.
    pub max_queued_swap_jobs: usize,

    /// Seconds to wait for the ComfyUI /system_stats health-check on startup.
    pub health_check_timeout_secs: u64,

    /// Swap defragmentation: restart the ComfyUI sidecar after this many
    /// drop-and-swap cycles to clear VRAM fragmentation. 0 = disabled.
    pub defrag_every_n_swaps: usize,

    /// Per-style default LoRA strength (0.0–1.0).
    pub default_lora_strength: f32,

    /// Default quality profile when the caller does not specify one.
    /// One of: "fast" | "balanced" | "high". Default: "balanced".
    pub default_quality: String,

    /// Checkpoint filename for SDXL high-quality path (JuggernautXL / Lightning variant).
    pub sdxl_model_high: String,

    /// Master switch for the SDXL High profile.  Requires Tier S + model file.
    pub enable_sdxl_high_profile: bool,

    /// Ordered list of cloud providers to try.  Recognised values: "pollinations", "hf_flux".
    pub cloud_providers: Vec<String>,

    /// Per-image timeout for local ComfyUI generation (seconds).
    /// 0 = use hard-coded 5-minute cap.
    pub local_timeout_secs: u64,

    /// HuggingFace Inference API token for the hf_flux provider.
    /// Empty string = provider silently skipped.
    pub hf_inference_token: String,

    /// Prompt enhancement mode: "auto" | "always" | "never".
    /// "auto" = enhance only when the raw prompt is short (< 50 chars).
    pub prompt_enhance_mode: String,

    /// Image generation routing mode.
    /// One of: "auto" | "local_only" | "cloud_only" |
    ///         "local_with_cloud_fallback" | "cloud_with_local_fallback".
    /// Override at runtime with `KRIA_IMAGE_MODE` env var (env takes priority).
    pub image_mode: String,
}

impl Default for ImageGenerationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tier_override: String::new(),
            comfy_port: 8188,
            comfy_venv_dir: "comfyui/.venv".into(),
            comfy_models_dir: "comfyui/models".into(),
            output_dir: "cache/images".into(),
            conditioning_cache_dir: "cache/conditioning".into(),
            conditioning_cache_max_mb: 500,
            idle_unload_secs: 300,   // 5 minutes
            prewarm: "auto".into(),
            prewarm_delay_secs: 30,
            cloud_fallback: "auto_offer".into(),
            pollinations_base_url: "https://image.pollinations.ai".into(),
            max_concurrent_jobs: 2,
            max_queued_swap_jobs: 4,
            // 60s was tight for cold ComfyUI starts: scanning models, importing
            // ComfyUI-GGUF / ControlNet custom nodes, and CUDA context init can
            // easily push past 90s on a cold disk or older GPU. 180s gives
            // headroom; the early-exit detector still fast-fails on real errors.
            health_check_timeout_secs: 180,
            defrag_every_n_swaps: 15,
            default_lora_strength: 0.85,
            default_quality: "balanced".into(),
            sdxl_model_high: "juggernautXL_v9Lightning.safetensors".into(),
            enable_sdxl_high_profile: false,
            cloud_providers: vec!["pollinations".into(), "hf_flux".into()],
            local_timeout_secs: 180,
            hf_inference_token: String::new(),
            prompt_enhance_mode: "auto".into(),
            image_mode: "auto".into(),
        }
    }
}

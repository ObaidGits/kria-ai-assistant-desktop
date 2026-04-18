//! Provisioning state machine — orchestrates first-boot setup.
//!
//! States flow: NotStarted → HardwareDetection → BackendChoice → ModelDownload
//! → SidecarSetup → ServerVerification → Complete
//!
//! Or: NotStarted → HardwareDetection → BackendChoice → ExternalLlm
//! → SidecarSetup → Complete
//!
//! Each state is independently resumable. State is persisted to
//! `~/.kria/provisioning.json` so interrupted setups resume.

use crate::infra::component::{ComponentInfo, ComponentManifest, ComponentType};
use crate::infra::download::DownloadProgress;
use crate::infra::hardware_profiler::{self, HardwareProfile};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// Provisioning step identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningStep {
    NotStarted,
    HardwareDetection,
    BackendChoice,
    ModelDownload,
    SidecarSetup,
    ServerVerification,
    Complete,
}

impl ProvisioningStep {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::HardwareDetection => "hardware_detection",
            Self::BackendChoice => "backend_choice",
            Self::ModelDownload => "model_download",
            Self::SidecarSetup => "sidecar_setup",
            Self::ServerVerification => "server_verification",
            Self::Complete => "complete",
        }
    }
}

/// What the user chose for their LLM backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendChoice {
    /// Download and run local llama-server.
    Local,
    /// Connect to an existing OpenAI-compatible endpoint.
    External {
        url: String,
        api_key: Option<String>,
        model_name: Option<String>,
    },
}

/// Per-step status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed { error: String },
    Skipped,
}

/// Full provisioning state, persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningState {
    pub current_step: ProvisioningStep,
    pub steps: std::collections::HashMap<String, StepStatus>,
    pub hardware_profile: Option<HardwareProfile>,
    pub backend_choice: Option<BackendChoice>,
    pub models_dir: Option<String>,
    pub errors: Vec<ProvisioningError>,
}

/// A provisioning error with context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningError {
    pub step: String,
    pub message: String,
    pub timestamp: String,
    pub retryable: bool,
}

impl Default for ProvisioningState {
    fn default() -> Self {
        Self {
            current_step: ProvisioningStep::NotStarted,
            steps: std::collections::HashMap::new(),
            hardware_profile: None,
            backend_choice: None,
            models_dir: None,
            errors: Vec::new(),
        }
    }
}

impl ProvisioningState {
    fn state_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".kria").join("provisioning.json")
    }

    /// Load persisted state, or create new.
    pub fn load() -> Self {
        let path = Self::state_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist current state to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Mark a step as running.
    pub fn start_step(&mut self, step: ProvisioningStep) {
        self.current_step = step;
        self.steps
            .insert(step.as_str().to_string(), StepStatus::Running);
        let _ = self.save();
    }

    /// Mark a step as complete.
    pub fn complete_step(&mut self, step: ProvisioningStep) {
        self.steps
            .insert(step.as_str().to_string(), StepStatus::Done);
        let _ = self.save();
    }

    /// Mark a step as failed.
    pub fn fail_step(&mut self, step: ProvisioningStep, error: &str) {
        self.steps.insert(
            step.as_str().to_string(),
            StepStatus::Failed {
                error: error.to_string(),
            },
        );
        self.errors.push(ProvisioningError {
            step: step.as_str().to_string(),
            message: error.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            retryable: !matches!(step, ProvisioningStep::BackendChoice),
        });
        let _ = self.save();
    }

    /// Mark a step as skipped.
    pub fn skip_step(&mut self, step: ProvisioningStep) {
        self.steps
            .insert(step.as_str().to_string(), StepStatus::Skipped);
        let _ = self.save();
    }

    /// Check if provisioning is complete.
    pub fn is_complete(&self) -> bool {
        self.current_step == ProvisioningStep::Complete
    }

    /// Check if a step was completed.
    pub fn is_step_done(&self, step: &str) -> bool {
        matches!(self.steps.get(step), Some(StepStatus::Done))
    }
}

/// The provisioning engine. Drives the state machine forward.
pub struct ProvisioningEngine {
    pub state: ProvisioningState,
    cancel: CancellationToken,
}

impl ProvisioningEngine {
    /// Create or resume provisioning.
    pub fn new(cancel: CancellationToken) -> Self {
        let state = ProvisioningState::load();
        Self { state, cancel }
    }

    /// Run the hardware detection step.
    pub fn run_hardware_detection(&mut self) -> anyhow::Result<&HardwareProfile> {
        self.state
            .start_step(ProvisioningStep::HardwareDetection);

        let profile = hardware_profiler::profile_hardware();

        // Save the profile to disk
        if let Err(e) = hardware_profiler::save_profile(&profile) {
            tracing::warn!("failed to save hardware profile: {e}");
        }

        tracing::info!(
            tier = ?profile.info.tier,
            gpu_vendor = ?profile.gpu_vendor,
            ram_mb = profile.info.total_ram_mb,
            "hardware detection complete"
        );

        self.state.hardware_profile = Some(profile);
        self.state
            .complete_step(ProvisioningStep::HardwareDetection);

        Ok(self.state.hardware_profile.as_ref().unwrap())
    }

    /// Set the user's backend choice.
    pub fn set_backend_choice(&mut self, choice: BackendChoice) {
        self.state.start_step(ProvisioningStep::BackendChoice);
        self.state.backend_choice = Some(choice);
        self.state.complete_step(ProvisioningStep::BackendChoice);
    }

    /// Run model download step (only for local backend).
    pub async fn run_model_download<F>(&mut self, _on_progress: F) -> anyhow::Result<()>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        // Skip if external backend
        if matches!(self.state.backend_choice, Some(BackendChoice::External { .. })) {
            self.state.skip_step(ProvisioningStep::ModelDownload);
            return Ok(());
        }

        self.state.start_step(ProvisioningStep::ModelDownload);

        let profile = self
            .state
            .hardware_profile
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("hardware profile not available"))?;

        let model_name = profile.info.tier.recommended_model();
        tracing::info!(model = model_name, "downloading recommended model for tier");

        // Determine models directory
        let models_dir = self
            .state
            .models_dir
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("KRIA_MODELS_DIR").ok().map(PathBuf::from))
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".kria")
                    .join("models")
            });

        // TODO: Fetch model manifest and download based on tier
        // For now, mark as done since the full manifest system depends on the
        // remote manifest being set up. The download infrastructure is in place.
        tracing::info!(
            dir = %models_dir.display(),
            model = model_name,
            "model download infrastructure ready (manifest-based download pending)"
        );

        self.state.complete_step(ProvisioningStep::ModelDownload);
        Ok(())
    }

    /// Run the sidecar setup step.
    pub async fn run_sidecar_setup(&mut self) -> anyhow::Result<()> {
        self.state.start_step(ProvisioningStep::SidecarSetup);

        let venv_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".kria")
            .join("python-env");

        let venv_str = venv_dir.to_string_lossy().to_string();

        match crate::sidecar::bootstrap::bootstrap(&venv_str, "python3").await {
            Ok(result) => {
                tracing::info!(
                    python = %result.python_path,
                    used_uv = result.used_uv,
                    "sidecar bootstrap complete"
                );
                self.state.complete_step(ProvisioningStep::SidecarSetup);
                Ok(())
            }
            Err(e) => {
                tracing::warn!("sidecar setup failed: {e}");
                self.state
                    .fail_step(ProvisioningStep::SidecarSetup, &e.to_string());
                // Soft failure — don't propagate. User can still chat without sidecar.
                Ok(())
            }
        }
    }

    /// Run server verification step (check llama-server can start).
    pub async fn run_server_verification<F>(&mut self, on_progress: F) -> anyhow::Result<()>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        // Skip if external backend
        if matches!(self.state.backend_choice, Some(BackendChoice::External { .. })) {
            self.state
                .skip_step(ProvisioningStep::ServerVerification);
            return Ok(());
        }

        self.state
            .start_step(ProvisioningStep::ServerVerification);

        match crate::llm::server_binary::ensure_llama_server(&self.cancel, on_progress).await {
            Ok(status) => {
                let path = status.path().clone();
                tracing::info!(path = %path.display(), "llama-server verified");

                // Register in component manifest
                let mut manifest = ComponentManifest::load();
                manifest.register(ComponentInfo {
                    name: "llama-server".to_string(),
                    version: "latest".to_string(), // TODO: detect version from binary
                    installed_at: chrono::Utc::now().to_rfc3339(),
                    path,
                    component_type: ComponentType::LlamaServer,
                });
                let _ = manifest.save();

                self.state
                    .complete_step(ProvisioningStep::ServerVerification);
                Ok(())
            }
            Err(e) => {
                self.state
                    .fail_step(ProvisioningStep::ServerVerification, &e.to_string());
                Err(e)
            }
        }
    }

    /// Run all provisioning steps in sequence.
    ///
    /// Resumes from the last incomplete step.
    pub async fn run_all<F>(&mut self, on_progress: F) -> anyhow::Result<()>
    where
        F: Fn(DownloadProgress) + Send + Sync + Clone,
    {
        if self.state.is_complete() {
            tracing::info!("provisioning already complete");
            return Ok(());
        }

        // Step 1: Hardware detection
        if !self.state.is_step_done("hardware_detection") {
            self.run_hardware_detection()?;
        }

        // Step 2: Backend choice (requires user input — caller must set it)
        // If not set, we default to local
        if !self.state.is_step_done("backend_choice") {
            if self.state.backend_choice.is_none() {
                self.set_backend_choice(BackendChoice::Local);
            } else {
                self.state.complete_step(ProvisioningStep::BackendChoice);
            }
        }

        // Step 3: Model download
        if !self.state.is_step_done("model_download")
            && !matches!(
                self.state.steps.get("model_download"),
                Some(StepStatus::Skipped)
            )
        {
            self.run_model_download(on_progress.clone()).await?;
        }

        // Step 4: Sidecar setup
        if !self.state.is_step_done("sidecar_setup") {
            self.run_sidecar_setup().await?;
        }

        // Step 5: Server verification
        if !self.state.is_step_done("server_verification")
            && !matches!(
                self.state.steps.get("server_verification"),
                Some(StepStatus::Skipped)
            )
        {
            self.run_server_verification(on_progress).await?;
        }

        // Mark complete
        self.state.current_step = ProvisioningStep::Complete;
        let _ = self.state.save();
        tracing::info!("provisioning complete");

        Ok(())
    }

    /// Get diagnostic info for troubleshooting.
    pub fn diagnostic_info(&self) -> String {
        let mut info = String::new();
        info.push_str("=== K.R.I.A. Provisioning Diagnostics ===\n\n");

        info.push_str(&format!(
            "Current step: {:?}\n",
            self.state.current_step
        ));

        if let Some(ref profile) = self.state.hardware_profile {
            info.push_str(&format!(
                "Hardware: {:?} tier, {:?} GPU, {} MB RAM, arch={}\n",
                profile.info.tier, profile.gpu_vendor, profile.info.total_ram_mb, profile.arch
            ));
        }

        info.push_str("\nStep status:\n");
        for (step, status) in &self.state.steps {
            info.push_str(&format!("  {}: {:?}\n", step, status));
        }

        if !self.state.errors.is_empty() {
            info.push_str("\nErrors:\n");
            for err in &self.state.errors {
                info.push_str(&format!(
                    "  [{}] {}: {}\n",
                    err.timestamp, err.step, err.message
                ));
            }
        }

        info
    }
}

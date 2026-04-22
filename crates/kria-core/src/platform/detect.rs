use std::process::Command;
use sysinfo::System;

/// Detected operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Windows,
    MacOS,
}

/// Detected hardware tier based on available resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareTier {
    Lite,
    Standard,
    Performance,
    High,
}

impl std::str::FromStr for HardwareTier {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let tier = match s.to_lowercase().as_str() {
            "lite" => Self::Lite,
            "performance" => Self::Performance,
            "high" => Self::High,
            _ => Self::Standard,
        };
        Ok(tier)
    }
}

/// Detected package manager on the host system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Brew,
    Winget,
    Choco,
    Snap,
    Flatpak,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
            Self::Brew => "brew",
            Self::Winget => "winget",
            Self::Choco => "choco",
            Self::Snap => "snap",
            Self::Flatpak => "flatpak",
        }
    }
}

/// Snapshot of detected hardware capabilities.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HardwareInfo {
    pub os: Os,
    pub tier: HardwareTier,
    pub cpu_cores: usize,
    pub total_ram_mb: u64,
    pub vram_mb: Option<u64>,
    pub gpu_name: Option<String>,
    pub package_manager: Option<PackageManager>,
    pub hostname: String,
}

impl HardwareTier {
    /// Tier name as lowercase str.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Standard => "standard",
            Self::Performance => "performance",
            Self::High => "high",
        }
    }

    /// Recommended context window tokens for this tier.
    pub fn context_window(&self) -> usize {
        match self {
            Self::Lite => 1024,
            Self::Standard => 2048,
            Self::Performance => 4096,
            Self::High => 8192,
        }
    }

    /// Recommended thread count for inference.
    pub fn thread_count(&self) -> usize {
        match self {
            Self::Lite => 4,
            Self::Standard => 6,
            Self::Performance | Self::High => 8,
        }
    }

    /// Recommended GPU layers (0 = CPU only, 99 = all layers).
    pub fn gpu_layers(&self) -> i32 {
        match self {
            Self::Lite | Self::Standard => 0,
            Self::Performance | Self::High => 99,
        }
    }

    /// Whether vision capabilities are available at this tier.
    pub fn has_vision(&self) -> bool {
        matches!(self, Self::Performance | Self::High)
    }

    /// Recommended STT model for this tier.
    pub fn stt_model(&self) -> &'static str {
        match self {
            Self::Lite => "ggml-small-q5_1.bin",
            Self::Standard => "ggml-medium-q5_0.bin",
            Self::Performance | Self::High => "ggml-large-v3-turbo-q5_0.bin",
        }
    }

    /// Recommended LLM model name for this tier.
    pub fn recommended_model(&self) -> &'static str {
        match self {
            Self::Lite => "qwen2.5-3b-q4_k_m",
            Self::Standard => "phi-4-mini-q4_k_m",
            Self::Performance | Self::High => "qwen2.5-vl-7b-q4_k_m",
        }
    }
}

/// Detect the current operating system.
pub fn get_os() -> Os {
    if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::MacOS
    } else {
        Os::Linux // fallback
    }
}

/// Check if a command exists on the system PATH.
pub fn has_command(name: &str) -> bool {
    let check = if cfg!(target_os = "windows") {
        Command::new("where").arg(name).output()
    } else {
        Command::new("which").arg(name).output()
    };
    check.map(|o| o.status.success()).unwrap_or(false)
}

/// Detect the primary package manager.
pub fn get_package_manager() -> Option<PackageManager> {
    match get_os() {
        Os::Windows => {
            if has_command("winget") {
                Some(PackageManager::Winget)
            } else if has_command("choco") {
                Some(PackageManager::Choco)
            } else {
                None
            }
        }
        Os::MacOS => {
            if has_command("brew") {
                Some(PackageManager::Brew)
            } else {
                None
            }
        }
        Os::Linux => {
            if has_command("apt-get") {
                Some(PackageManager::Apt)
            } else if has_command("dnf") {
                Some(PackageManager::Dnf)
            } else if has_command("pacman") {
                Some(PackageManager::Pacman)
            } else if has_command("zypper") {
                Some(PackageManager::Zypper)
            } else {
                None
            }
        }
    }
}

/// Detect ALL available package managers on the system.
/// On Linux a machine may have apt + snap + flatpak simultaneously.
pub fn get_available_package_managers() -> Vec<PackageManager> {
    let mut pms = Vec::new();
    match get_os() {
        Os::Linux => {
            if has_command("apt-get") {
                pms.push(PackageManager::Apt);
            }
            if has_command("dnf") {
                pms.push(PackageManager::Dnf);
            }
            if has_command("pacman") {
                pms.push(PackageManager::Pacman);
            }
            if has_command("zypper") {
                pms.push(PackageManager::Zypper);
            }
            if has_command("snap") {
                pms.push(PackageManager::Snap);
            }
            if has_command("flatpak") {
                pms.push(PackageManager::Flatpak);
            }
        }
        Os::MacOS => {
            if has_command("brew") {
                pms.push(PackageManager::Brew);
            }
        }
        Os::Windows => {
            if has_command("winget") {
                pms.push(PackageManager::Winget);
            }
            if has_command("choco") {
                pms.push(PackageManager::Choco);
            }
        }
    }
    pms
}

/// Attempt to detect NVIDIA GPU VRAM via nvidia-smi.
///
/// A 5-second timeout is enforced. If the NVIDIA driver is wedged or
/// `nvidia-smi` hangs, returns `(None, None)` rather than blocking
/// the caller indefinitely.
fn detect_gpu() -> (Option<u64>, Option<String>) {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let mut child = match Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.total,name",
            "--format=csv,noheader,nounits",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    // Take stdout *before* waiting so we can read it after process exits.
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return (None, None),
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return (None, None);
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    tracing::warn!("detect_gpu: nvidia-smi timed out after 5s, killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    return (None, None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return (None, None),
        }
    }

    let mut buf = String::new();
    if stdout.read_to_string(&mut buf).is_err() {
        return (None, None);
    }

    let line = buf.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    let vram = parts.first().and_then(|s| s.parse::<u64>().ok());
    let name = parts.get(1).map(|s| s.to_string());
    (vram, name)
}

/// Determine hardware tier from RAM + VRAM.
fn classify_tier(ram_mb: u64, vram_mb: Option<u64>) -> HardwareTier {
    let vram = vram_mb.unwrap_or(0);
    if vram >= 8192 || ram_mb >= 16384 {
        HardwareTier::High
    } else if vram >= 4096 || ram_mb >= 12288 {
        HardwareTier::Performance
    } else if vram >= 2048 || ram_mb >= 8192 {
        HardwareTier::Standard
    } else {
        HardwareTier::Lite
    }
}

/// Full hardware detection — call once at startup.
pub fn detect_hardware() -> HardwareInfo {
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpu_cores = sys.cpus().len();
    let total_ram_mb = sys.total_memory() / (1024 * 1024);
    let (vram_mb, gpu_name) = detect_gpu();
    let tier = classify_tier(total_ram_mb, vram_mb);
    let hostname = System::host_name().unwrap_or_else(|| "unknown".into());

    HardwareInfo {
        os: get_os(),
        tier,
        cpu_cores,
        total_ram_mb,
        vram_mb,
        gpu_name,
        package_manager: get_package_manager(),
        hostname,
    }
}

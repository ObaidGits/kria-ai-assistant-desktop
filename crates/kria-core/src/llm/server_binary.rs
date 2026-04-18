//! llama-server binary management: detect, download, and update.
//!
//! Manages the `llama-server` (llama.cpp) binary that the `LlamaServerManager`
//! needs to spawn. Handles platform-specific binary selection, download from
//! GitHub releases, and quarantine stripping on macOS.

use crate::infra::download::{DownloadClient, DownloadClientConfig, DownloadProgress};
use crate::platform::{
    detect::{self, Os},
    os,
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// GitHub repo that publishes llama.cpp releases.
const LLAMA_CPP_REPO: &str = "ggml-org/llama.cpp";
/// Minimum supported version tag.
#[allow(dead_code)]
const MIN_VERSION: &str = "b5170";

/// Outcome of ensuring llama-server is available.
#[derive(Debug)]
pub enum ServerBinaryStatus {
    /// Binary was already present at the given path.
    AlreadyPresent(PathBuf),
    /// Binary was freshly downloaded.
    Downloaded(PathBuf),
    /// Binary is on PATH (system install).
    SystemPath(PathBuf),
}

impl ServerBinaryStatus {
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::AlreadyPresent(p) | Self::Downloaded(p) | Self::SystemPath(p) => p,
        }
    }
}

/// Ensure `llama-server` is available, downloading if necessary.
///
/// Resolution order:
/// 1. `~/.kria/bin/llama-server` (managed binary)
/// 2. System PATH
/// 3. Download from GitHub releases
pub async fn ensure_llama_server<F>(
    cancel: &CancellationToken,
    on_progress: F,
) -> anyhow::Result<ServerBinaryStatus>
where
    F: Fn(DownloadProgress) + Send + Sync,
{
    // 1. Check managed binary location
    let managed_path = os::managed_binary_path("llama-server");
    if managed_path.exists() {
        tracing::info!(path = %managed_path.display(), "llama-server found in managed dir");
        return Ok(ServerBinaryStatus::AlreadyPresent(managed_path));
    }

    // 2. Check system PATH
    if detect::has_command("llama-server") {
        let which = if cfg!(target_os = "windows") {
            "where"
        } else {
            "which"
        };
        let output = std::process::Command::new(which)
            .arg("llama-server")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                    )
                } else {
                    None
                }
            });

        if let Some(path) = output {
            if !path.is_empty() {
                tracing::info!(path = %path, "llama-server found on PATH");
                return Ok(ServerBinaryStatus::SystemPath(PathBuf::from(path)));
            }
        }
    }

    // 3. Download from GitHub releases
    tracing::info!("llama-server not found; downloading from GitHub");
    let path = download_llama_server(cancel, on_progress).await?;
    Ok(ServerBinaryStatus::Downloaded(path))
}

/// Download the appropriate llama-server binary for this platform.
async fn download_llama_server<F>(
    cancel: &CancellationToken,
    on_progress: F,
) -> anyhow::Result<PathBuf>
where
    F: Fn(DownloadProgress) + Send + Sync,
{
    let asset_name = platform_asset_name()?;
    let url = format!(
        "https://github.com/{LLAMA_CPP_REPO}/releases/latest/download/{asset_name}"
    );

    let bin_dir = os::ensure_bin_dir()?;
    let client = DownloadClient::new(DownloadClientConfig::default())?;

    let result = client
        .download(&url, &bin_dir, &asset_name, None, cancel, on_progress)
        .await?;

    // Extract the binary from the downloaded archive
    let server_path = extract_llama_server(&result.path, &bin_dir)?;

    // Clean up archive
    let _ = std::fs::remove_file(&result.path);

    // Set executable bit / strip quarantine
    let _ = os::set_executable(&server_path);
    let _ = os::strip_quarantine(&server_path);

    tracing::info!(path = %server_path.display(), "llama-server downloaded and installed");
    Ok(server_path)
}

/// Determine the correct GitHub release asset name for this platform.
fn platform_asset_name() -> anyhow::Result<String> {
    let os = detect::get_os();
    let arch = std::env::consts::ARCH;

    let name = match (os, arch) {
        (Os::Linux, "x86_64") => "llama-server-linux-x86_64.tar.gz",
        (Os::Linux, "aarch64") => "llama-server-linux-aarch64.tar.gz",
        (Os::MacOS, "x86_64") => "llama-server-macos-x86_64.tar.gz",
        (Os::MacOS, "aarch64") => "llama-server-macos-arm64.tar.gz",
        (Os::Windows, "x86_64") => "llama-server-windows-x86_64.zip",
        _ => anyhow::bail!("unsupported platform: {:?}-{}", os, arch),
    };

    Ok(name.to_string())
}

/// Extract `llama-server` binary from a downloaded archive.
///
/// Uses CLI tools (`tar`, `unzip`) for extraction — avoids extra crate deps.
fn extract_llama_server(archive_path: &std::path::Path, dest_dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    let archive_name = archive_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let binary_name = if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    };

    // Extract to a temp directory, then find and move the binary
    let extract_dir = dest_dir.join("_llama_extract");
    let _ = std::fs::create_dir_all(&extract_dir);

    let result = if archive_name.ends_with(".tar.gz") || archive_name.ends_with(".tgz") {
        std::process::Command::new("tar")
            .args(["xzf"])
            .arg(archive_path)
            .arg("-C")
            .arg(&extract_dir)
            .output()
    } else if archive_name.ends_with(".zip") {
        if cfg!(target_os = "windows") {
            // PowerShell Expand-Archive
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                        archive_path.display(),
                        extract_dir.display()
                    ),
                ])
                .output()
        } else {
            std::process::Command::new("unzip")
                .args(["-o"])
                .arg(archive_path)
                .args(["-d"])
                .arg(&extract_dir)
                .output()
        }
    } else {
        // Assume plain binary
        let dest = dest_dir.join(binary_name);
        std::fs::rename(archive_path, &dest)?;
        return Ok(dest);
    };

    let output = result.map_err(|e| anyhow::anyhow!("extraction command failed: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("extraction failed: {}", err.trim());
    }

    // Walk the extracted directory to find the binary
    let dest = dest_dir.join(binary_name);
    let found = find_file_recursive(&extract_dir, binary_name);

    match found {
        Some(src) => {
            std::fs::copy(&src, &dest)?;
            // Clean up extraction directory
            let _ = std::fs::remove_dir_all(&extract_dir);
            Ok(dest)
        }
        None => {
            let _ = std::fs::remove_dir_all(&extract_dir);
            anyhow::bail!(
                "{} not found in archive {}",
                binary_name,
                archive_path.display()
            )
        }
    }
}

/// Recursively find a file by name in a directory.
fn find_file_recursive(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file_recursive(&path, name) {
                return Some(found);
            }
        } else if path.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(path);
        }
    }
    None
}

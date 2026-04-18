//! Cross-platform OS utilities for binary resolution and quarantine handling.

use std::path::{Path, PathBuf};

/// Resolve a binary by name, checking managed locations first.
///
/// Search order:
/// 1. `~/.kria/bin/<name>[.exe]`
/// 2. The provided `configured_path` as-is (PATH lookup or absolute)
///
/// On Windows, `.exe` is appended automatically if not already present.
pub fn resolve_binary(name: &str, configured_path: &str) -> String {
    let managed = managed_binary_path(name);
    if managed.exists() {
        return managed.to_string_lossy().into_owned();
    }
    // On Windows, ensure .exe suffix for PATH lookup
    if cfg!(target_os = "windows") && !configured_path.ends_with(".exe") {
        format!("{configured_path}.exe")
    } else {
        configured_path.to_string()
    }
}

/// Return the platform-correct path for a managed binary in `~/.kria/bin/`.
pub fn managed_binary_path(name: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let bin_dir = home.join(".kria").join("bin");
    if cfg!(target_os = "windows") {
        bin_dir.join(format!("{name}.exe"))
    } else {
        bin_dir.join(name)
    }
}

/// Ensure `~/.kria/bin/` exists.
pub fn ensure_bin_dir() -> std::io::Result<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let bin_dir = home.join(".kria").join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    Ok(bin_dir)
}

/// Strip macOS quarantine extended attribute from a downloaded binary.
///
/// On macOS, Gatekeeper quarantines files downloaded from the internet,
/// which blocks execution of unsigned binaries like llama-server or uv.
/// This is a no-op on Linux and Windows.
#[allow(unused_variables)]
pub fn strip_quarantine(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        // Ignore failure — attribute may not exist, which is fine
        let _ = status;
    }
    Ok(())
}

/// Set executable permission on Unix systems. No-op on Windows.
#[allow(unused_variables)]
pub fn set_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

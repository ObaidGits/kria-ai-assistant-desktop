//! Python sidecar bootstrapping: venv creation, `kria-modules` installation.
//!
//! Prefers `uv` (fast, hermetic) over `python -m venv`. Falls back gracefully.
//! Replaces shell-specific operations (e.g. `cp -r`) with portable Rust equivalents.

use crate::platform::detect;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Result of bootstrapping the Python sidecar environment.
#[derive(Debug)]
pub struct BootstrapResult {
    /// Path to the venv Python binary (ready to execute).
    pub python_path: String,
    /// Whether `uv` was used (vs. stdlib venv).
    pub used_uv: bool,
}

/// Bootstrap the Python sidecar environment.
///
/// 1. Detect or install `uv` if available
/// 2. Create venv (prefer `uv venv`, fallback `python -m venv`)
/// 3. Install `kria-modules` into the venv
pub async fn bootstrap(venv_dir: &str, python_cmd: &str) -> anyhow::Result<BootstrapResult> {
    let venv_python = venv_python_path(venv_dir);

    // If the venv already exists and has kria_modules, skip setup
    if Path::new(&venv_python).exists() {
        let check = Command::new(&venv_python)
            .args(["-c", "import kria_modules.bridge"])
            .output()
            .await;

        if check.map(|o| o.status.success()).unwrap_or(false) {
            tracing::info!("sidecar bootstrap: existing venv OK at {}", venv_dir);
            return Ok(BootstrapResult {
                python_path: venv_python,
                used_uv: false,
            });
        }
    }

    tracing::info!("sidecar bootstrap: creating venv at {venv_dir}");

    // ── Step 1: Create venv (prefer uv) ──
    let used_uv = if detect::has_command("uv") {
        match create_venv_uv(venv_dir, python_cmd).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("uv venv creation failed ({}), falling back to stdlib", e);
                create_venv_stdlib(venv_dir, python_cmd).await?;
                false
            }
        }
    } else {
        create_venv_stdlib(venv_dir, python_cmd).await?;
        false
    };

    // ── Step 2: Upgrade pip (skip if uv was used — uv handles this) ──
    if !used_uv {
        let pip = venv_pip_path(venv_dir);
        let _ = Command::new(&pip)
            .args(["install", "--upgrade", "pip", "--quiet"])
            .output()
            .await;
    }

    // ── Step 3: Install kria-modules ──
    install_kria_modules(venv_dir, used_uv).await?;

    Ok(BootstrapResult {
        python_path: venv_python,
        used_uv,
    })
}

/// Create venv using `uv venv`.
async fn create_venv_uv(venv_dir: &str, python_cmd: &str) -> anyhow::Result<()> {
    let out = Command::new("uv")
        .args(["venv", venv_dir, "--python", python_cmd])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run uv: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("uv venv creation failed: {}", err.trim());
    }
    tracing::info!("sidecar bootstrap: venv created via uv");
    Ok(())
}

/// Create venv using Python stdlib.
async fn create_venv_stdlib(venv_dir: &str, python_cmd: &str) -> anyhow::Result<()> {
    let out = Command::new(python_cmd)
        .args(["-m", "venv", venv_dir])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run `{python_cmd} -m venv`: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("venv creation failed: {}", err.trim());
    }
    tracing::info!("sidecar bootstrap: venv created via stdlib");
    Ok(())
}

/// Install kria-modules into the venv.
///
/// Finds kria-modules source relative to the running binary, copies it into
/// site-packages using Rust (not `cp -r`), and installs runtime deps.
async fn install_kria_modules(venv_dir: &str, used_uv: bool) -> anyhow::Result<()> {
    let venv_python = venv_python_path(venv_dir);

    // Find kria-modules source directory
    let exe = std::env::current_exe().unwrap_or_default();
    let candidates = [
        // target/debug/kria-desktop → ../../.. → workspace root
        exe.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|ws| ws.join("kria-modules"))
            .unwrap_or_default(),
        // direct sibling of exe
        exe.parent()
            .map(|p| p.join("kria-modules"))
            .unwrap_or_default(),
    ];

    let modules_dir = candidates
        .iter()
        .find(|p| p.join("pyproject.toml").exists());

    match modules_dir {
        Some(src) => {
            tracing::info!(
                "sidecar bootstrap: installing kria-modules from {}",
                src.display()
            );

            // Determine site-packages path
            let site_out = Command::new(&venv_python)
                .args(["-c", "import site; print(site.getsitepackages()[0])"])
                .output()
                .await?;

            let site_pkgs = String::from_utf8_lossy(&site_out.stdout)
                .trim()
                .to_string();
            if site_pkgs.is_empty() {
                anyhow::bail!("could not determine venv site-packages path");
            }

            let src_pkg = src.join("src").join("kria_modules");
            let dst_pkg = PathBuf::from(&site_pkgs).join("kria_modules");

            // Portable copy (no `cp -r` — works on Windows too)
            copy_dir_recursive(&src_pkg, &dst_pkg)?;

            // Install runtime deps
            install_runtime_deps(venv_dir, used_uv).await;

            tracing::info!(
                "sidecar bootstrap: kria-modules installed to {}",
                site_pkgs
            );
        }
        None => {
            tracing::warn!(
                "sidecar bootstrap: kria-modules source not found; installing bridge deps only"
            );
            install_runtime_deps(venv_dir, used_uv).await;
        }
    }

    Ok(())
}

/// Install runtime deps using uv or pip.
async fn install_runtime_deps(venv_dir: &str, used_uv: bool) {
    let deps = ["psutil", "feedparser", "trafilatura"];
    if used_uv {
        let _ = Command::new("uv")
            .args(["pip", "install", "--python", &venv_python_path(venv_dir)])
            .args(deps)
            .args(["--quiet"])
            .output()
            .await;
    } else {
        let pip = venv_pip_path(venv_dir);
        let _ = Command::new(&pip)
            .args(["install"])
            .args(deps)
            .args(["--quiet"])
            .output()
            .await;
    }
}

/// Recursively copy a directory using pure Rust (cross-platform, no `cp`).
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if !src.is_dir() {
        anyhow::bail!("source {} is not a directory", src.display());
    }

    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

// ── Cross-platform path helpers ──
pub fn venv_python_path(venv_dir: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}\\Scripts\\python.exe", venv_dir)
    } else {
        format!("{}/bin/python", venv_dir)
    }
}

pub fn venv_pip_path(venv_dir: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}\\Scripts\\pip.exe", venv_dir)
    } else {
        format!("{}/bin/pip", venv_dir)
    }
}

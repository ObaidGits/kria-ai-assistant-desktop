/// Ring 4 — OS-level sandbox.
///
/// On Linux this installs a seccomp-BPF (syscall) filter that blocks a small set
/// of high-privilege syscalls that KRIA should never need.  The filter uses
/// `SECCOMP_RET_ERRNO(EPERM)` so blocked calls return an error rather than
/// killing the process (KRIA can log them as anomalies).
///
/// The function is a no-op on non-Linux platforms (compile-time cfg).
///
/// # When to call
/// Call once, early in `main()`, before spawning any Tauri/Tokio runtime threads.
/// All threads inherit the filter automatically (that is how seccomp works).
///
/// # Blocked syscalls
/// | Syscall | Rationale |
/// |---|---|
/// | `mount` / `umount2` | No reason to mount filesystems |
/// | `kexec_load` / `kexec_file_load` | Cannot be needed by a desktop assistant |
/// | `init_module` / `finit_module` / `delete_module` | Kernel module manipulation |
/// | `bpf` | eBPF is privileged; KRIA does not need it |
/// | `reboot` | Out of scope |
/// | `swapon` / `swapoff` | Out of scope |
/// | `pivot_root` / `chroot` | Container-escape primitives |

/// Install the seccomp filter.  Returns `Ok(())` on success or if not on Linux.
/// Returns `Err(reason)` if the filter could not be installed — callers should
/// log the error but may choose to continue running (degraded Ring 4 protection).
pub fn install_seccomp_filter() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        install_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // No-op on macOS / Windows — Ring 4 is a Linux-only feature.
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn install_linux() -> Result<(), String> {
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch,
    };
    use std::collections::BTreeMap;

    // Build a rule-map of syscalls to deny.
    macro_rules! deny {
        ($($name:expr),+ $(,)?) => {{
            let mut m: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
            $(
                match libc_syscall_nr($name) {
                    Ok(nr) => { m.insert(nr, vec![SeccompRule::new(vec![]).map_err(|e| e.to_string())?]); }
                    Err(e) => { tracing::warn!("seccomp: skipping '{}': {}", $name, e); }
                }
            )+
            m
        }};
    }

    let rules = deny![
        "mount",
        "umount2",
        "kexec_load",
        "kexec_file_load",
        "init_module",
        "finit_module",
        "delete_module",
        "bpf",
        "reboot",
        "swapon",
        "swapoff",
        "pivot_root",
        "chroot"
    ];

    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|e| format!("seccomp: unsupported arch '{}': {e}", std::env::consts::ARCH))?;

    let filter = SeccompFilter::new(
        rules,
        // Default action for all other syscalls: allow.
        SeccompAction::Allow,
        // Action for matched syscalls: return EPERM.
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| e.to_string())?;

    let bpf: BpfProgram = filter.try_into().map_err(|e| format!("{e}"))?;
    seccompiler::apply_filter(&bpf).map_err(|e| e.to_string())?;

    tracing::info!("seccomp-BPF filter installed (Ring 4 active)");
    Ok(())
}

/// Look up the syscall number for a syscall name on the current Linux kernel.
#[cfg(target_os = "linux")]
fn libc_syscall_nr(name: &str) -> Result<i64, String> {
    // seccompiler v0.4 supports looking up by name via the SyscallId type.
    // We map the string names manually because the syscall ABI numbers are
    // architecture-specific and seccompiler knows about them internally.
    //
    // For x86_64 (the primary target for KRIA desktop):
    #[cfg(target_arch = "x86_64")]
    let nr = match name {
        "mount"           => libc::SYS_mount,
        "umount2"         => libc::SYS_umount2,
        "kexec_load"      => libc::SYS_kexec_load,
        "kexec_file_load" => libc::SYS_kexec_file_load,
        "init_module"     => libc::SYS_init_module,
        "finit_module"    => libc::SYS_finit_module,
        "delete_module"   => libc::SYS_delete_module,
        "bpf"             => libc::SYS_bpf,
        "reboot"          => libc::SYS_reboot,
        "swapon"          => libc::SYS_swapon,
        "swapoff"         => libc::SYS_swapoff,
        "pivot_root"      => libc::SYS_pivot_root,
        "chroot"          => libc::SYS_chroot,
        other             => return Err(format!("unknown syscall name: {other}")),
    };

    #[cfg(not(target_arch = "x86_64"))]
    let nr: i64 = {
        // On other arches (aarch64, riscv, etc.) we leave a compile warning.
        // The filter will simply skip the unknown syscall without breaking startup.
        tracing::warn!(
            syscall = name,
            "seccomp: syscall number unknown on this arch — entry will be skipped"
        );
        return Err(format!("syscall '{name}' not mapped for this arch"));
    };

    Ok(nr as i64)
}

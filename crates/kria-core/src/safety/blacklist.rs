use once_cell::sync::Lazy;
use regex::Regex;

/// All BLACK tier patterns — hardcoded, cannot be overridden.
static BLACK_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    let raw = [
        // Disk destruction
        r"format [a-z]:",
        r"diskpart.*clean",
        r"cipher /w",
        r"mkfs\.",
        r"dd if=.*/dev/",
        // Boot/system integrity
        r"bcdedit",
        r"bootrec",
        r"sfc /scannow.*delete",
        r"grub-install",
        // Security disabling
        r"netsh.*firewall.*disable",
        r"Set-MpPreference.*Disable.*True",
        r"net stop WinDefend",
        r"ufw disable",
        r"iptables -F",
        r"setenforce 0",
        // System file destruction
        r"del.*system32",
        r"rmdir.*windows",
        r"rm -rf /\s*$",
        r"rm -rf /\*",
        r"Remove-Item.*-Recurse.*C:\\Windows",
        r"rm -rf /boot",
        r"rm -rf /etc",
        r"rm -rf /usr",
        // Credential theft
        r"mimikatz",
        r"lsass",
        r"SAM.*dump",
        r"sekurlsa",
        r"/etc/shadow",
        r"passwd.*dump",
        // Reverse shells / remote access
        r"nc -.*-e",
        r"ncat.*-e",
        r"bash -i >& /dev/tcp",
        r"python.*socket.*connect",
        // Cryptocurrency mining
        r"xmrig",
        r"minerd",
        r"cgminer",
    ];
    raw.iter().filter_map(|p| Regex::new(p).ok()).collect()
});

/// Checks input text against the BLACK tier regex patterns.
pub struct BlacklistChecker;

impl BlacklistChecker {
    pub fn new() -> Self {
        Self
    }

    /// Returns true if the input matches any blacklisted pattern.
    pub fn is_blocked(&self, input: &str) -> bool {
        for pat in BLACK_PATTERNS.iter() {
            if pat.is_match(input) {
                return true;
            }
        }
        false
    }

    /// Returns the matched pattern name if blocked, or None.
    pub fn check(&self, input: &str) -> Option<String> {
        for pat in BLACK_PATTERNS.iter() {
            if pat.is_match(input) {
                return Some(pat.as_str().to_string());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_rm_rf_root() {
        let checker = BlacklistChecker::new();
        assert!(checker.is_blocked("rm -rf /"));
        assert!(checker.is_blocked("rm -rf /*"));
        assert!(checker.is_blocked("rm -rf /boot"));
    }

    #[test]
    fn blocks_credential_theft() {
        let checker = BlacklistChecker::new();
        assert!(checker.is_blocked("mimikatz.exe"));
        assert!(checker.is_blocked("cat /etc/shadow"));
    }

    #[test]
    fn allows_normal_commands() {
        let checker = BlacklistChecker::new();
        assert!(!checker.is_blocked("ls -la"));
        assert!(!checker.is_blocked("cat /home/user/file.txt"));
        assert!(!checker.is_blocked("echo hello"));
    }
}

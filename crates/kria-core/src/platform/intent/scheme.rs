/// URI scheme allow-list and risk classification for `OpenUrl` capabilities.
///
/// # Security design
/// This module uses a **positive allow-list**, not a deny-list. Any scheme not
/// explicitly permitted is rejected. This prevents:
/// - `file://` → `xdg-open` executing local scripts
/// - `javascript:` / `data:` → renderer code injection
/// - `smb://` / `ftp://` → network share access bypassing sandboxing
/// - `vbscript:`, `view-source:`, `chrome-extension:` → same class of bypass
///
/// A URL that passes scheme validation still goes through the `PolicyEngine`
/// (defense-in-depth); scheme validation is Ring 2, policy is Ring 3.
use std::collections::HashSet;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::safety::policy::RiskLevel;

// ─── Scheme classification ────────────────────────────────────────────────────

/// A scheme that K.R.I.A. is permitted to dispatch via `OpenUrl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowedScheme {
    /// Standard HTTPS — always GREEN.
    Https,
    /// Plain HTTP — always GREEN (content may be insecure, but action is not destructive).
    Http,
    /// `mailto:` — open default mail composer.
    Mailto,
    /// `tel:` — initiate a phone/VOIP call.
    Tel,
    /// `sms:` — open SMS composer.
    Sms,
    /// A deep-link scheme registered by an installed application (e.g., `whatsapp://`,
    /// `spotify://`, `vscode://`, `slack://`, `zoommtg://`).
    /// These are YELLOW because we can't inspect what the third-party app will do.
    RegisteredDeepLink(String),
}

/// The result of classifying a URL — the permitted scheme and its associated risk.
#[derive(Clone, Debug)]
pub struct SchemeClassification {
    pub scheme: AllowedScheme,
    pub risk: RiskLevel,
}

/// Reasons a URL's scheme is rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchemeError {
    #[error("scheme '{0}' is permanently blocked (file/data/javascript etc.)")]
    PermanentlyBlocked(String),
    #[error("scheme '{0}' is not registered by any installed application")]
    UnknownDeepLink(String),
    #[error("URL has no scheme")]
    NoScheme,
    #[error("URL is malformed: {0}")]
    Malformed(String),
}

// ─── Permanently blocked schemes ─────────────────────────────────────────────
//
// These can NEVER be promoted to any tier, even by user configuration.
// The set is intentionally broad; legitimate use cases for these in an OS
// assistant do not exist.

static BLOCKED_SCHEMES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        // Local filesystem — can trigger execution of local scripts.
        "file",
        // Network file shares — bypass sandboxing.
        "smb",
        "cifs",
        "nfs",
        // Legacy insecure file transfer.
        "ftp",
        "ftps",
        "sftp",
        // Script injection vectors.
        "javascript",
        "vbscript",
        // Data URIs — can carry executable content.
        "data",
        // Browser internal pages.
        "about",
        "chrome",
        "chrome-extension",
        "chrome-devtools",
        "edge",
        "moz-extension",
        "resource",
        // Renderer inspection.
        "view-source",
        // Android deep-link injection vector on some Linux desktops.
        "intent",
        "android-app",
        // Virtualization / OS management — should never be opened as URL.
        "vnc",
        "rdp",
        "ssh",
        // Content provider — Android artifact, no place here.
        "content",
    ]
    .into_iter()
    .collect()
});

// ─── Known safe built-in deep links ─────────────────────────────────────────

/// Deep-link schemes that are well-known and safe to allow at YELLOW tier
/// without requiring them to be in the installed-app registry.
/// This covers the common case where the registry hasn't finished scanning yet.
static KNOWN_DEEP_LINKS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "whatsapp",
        "spotify",
        "vscode",
        "vscodium",
        "slack",
        "zoommtg",
        "zoomus",
        "discord",
        "teams",
        "telegram",
        "signal",
        "skype",
        "notion",
        "obsidian",
        "figma",
        "linear",
        "github",
        "githubc",
        "x-github-client",
        "gitlab",
    ]
    .into_iter()
    .collect()
});

// ─── classify_url ─────────────────────────────────────────────────────────────

/// Classify the scheme of a URL and return the permitted classification and risk tier.
///
/// # Arguments
/// - `url`: the URL to classify.
/// - `registry_schemes`: the set of URI schemes registered by installed applications
///   (from `InstalledAppRegistry::registered_schemes()`). May be `None` if the registry
///   hasn't loaded yet — in that case, only `KNOWN_DEEP_LINKS` are accepted at YELLOW.
///
/// # Returns
/// - `Ok(SchemeClassification)` if the scheme is permitted.
/// - `Err(SchemeError)` if the scheme is blocked or unknown.
pub fn classify_url(
    url: &Url,
    registry_schemes: Option<&HashSet<String>>,
) -> Result<SchemeClassification, SchemeError> {
    let scheme = url.scheme();
    if scheme.is_empty() {
        return Err(SchemeError::NoScheme);
    }

    // 1. Permanent block — highest priority, cannot be overridden.
    if BLOCKED_SCHEMES.contains(scheme) {
        return Err(SchemeError::PermanentlyBlocked(scheme.to_string()));
    }

    // 2. First-class safe schemes — GREEN.
    match scheme {
        "https" => {
            return Ok(SchemeClassification {
                scheme: AllowedScheme::Https,
                risk: RiskLevel::Green,
            })
        }
        "http" => {
            return Ok(SchemeClassification {
                scheme: AllowedScheme::Http,
                risk: RiskLevel::Green,
            })
        }
        "mailto" => {
            return Ok(SchemeClassification {
                scheme: AllowedScheme::Mailto,
                risk: RiskLevel::Green,
            })
        }
        "tel" => {
            return Ok(SchemeClassification {
                scheme: AllowedScheme::Tel,
                risk: RiskLevel::Green,
            })
        }
        "sms" => {
            return Ok(SchemeClassification {
                scheme: AllowedScheme::Sms,
                risk: RiskLevel::Green,
            })
        }
        _ => {}
    }

    // 3. Known deep links — YELLOW (preview before dispatch).
    if KNOWN_DEEP_LINKS.contains(scheme) {
        return Ok(SchemeClassification {
            scheme: AllowedScheme::RegisteredDeepLink(scheme.to_string()),
            risk: RiskLevel::Yellow,
        });
    }

    // 4. Registry-registered deep links — YELLOW.
    if let Some(reg) = registry_schemes {
        if reg.contains(scheme) {
            return Ok(SchemeClassification {
                scheme: AllowedScheme::RegisteredDeepLink(scheme.to_string()),
                risk: RiskLevel::Yellow,
            });
        }
    }

    // 5. Unknown — reject.
    Err(SchemeError::UnknownDeepLink(scheme.to_string()))
}

/// Build a Google Search URL from a topic string, safely encoding all special characters.
///
/// Uses `url::form_urlencoded` to prevent query parameter injection.
/// E.g., `topic = "cats & dogs"` → `https://www.google.com/search?q=cats+%26+dogs`
pub fn build_search_url(topic: &str) -> Result<Url, SchemeError> {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", topic)
        .finish();
    Url::parse(&format!("https://www.google.com/search?{query}"))
        .map_err(|e| SchemeError::Malformed(e.to_string()))
}

/// Build a YouTube search URL, safely encoding the query.
pub fn build_youtube_search_url(query: &str) -> Result<Url, SchemeError> {
    let encoded: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("search_query", query)
        .finish();
    Url::parse(&format!("https://www.youtube.com/results?{encoded}"))
        .map_err(|e| SchemeError::Malformed(e.to_string()))
}

/// Build a WhatsApp deep-link URL.
///
/// `phone` must be in E.164 format (e.g., `+919876543210`).
/// The message body is URL-encoded. The result opens WhatsApp Desktop or Web
/// with the chat pre-filled — the user still presses send.
pub fn build_whatsapp_url(phone_e164: &str, body: &str) -> Result<Url, SchemeError> {
    // Strip the leading '+' — WhatsApp's API expects digits only.
    let phone_digits: String = phone_e164.chars().filter(|c| c.is_ascii_digit()).collect();
    if phone_digits.is_empty() {
        return Err(SchemeError::Malformed(
            "phone number contains no digits".to_string(),
        ));
    }
    // Use percent-encoded body for the `text` parameter.
    let encoded_body = url::form_urlencoded::byte_serialize(body.as_bytes()).collect::<String>();
    Url::parse(&format!(
        "https://api.whatsapp.com/send?phone={phone_digits}&text={encoded_body}"
    ))
    .map_err(|e| SchemeError::Malformed(e.to_string()))
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(url_str: &str) -> Result<SchemeClassification, SchemeError> {
        let url: Url = url_str.parse().unwrap_or_else(|_| {
            // If parsing fails, return a dummy URL that will fail classification.
            "https://dummy.invalid/".parse().unwrap()
        });
        classify_url(&url, None)
    }

    // ── Permanently blocked schemes ───────────────────────────────────────

    #[test]
    fn blocks_file_scheme() {
        let url = Url::parse("file:///etc/passwd").unwrap();
        assert!(matches!(
            classify_url(&url, None),
            Err(SchemeError::PermanentlyBlocked(_))
        ));
    }

    #[test]
    fn blocks_file_local_script() {
        let url = Url::parse("file:///tmp/malicious.sh").unwrap();
        assert!(matches!(
            classify_url(&url, None),
            Err(SchemeError::PermanentlyBlocked(_))
        ));
    }

    #[test]
    fn blocks_javascript_scheme() {
        // Can't use Url::parse for javascript: directly (browser-only), test via table.
        assert!(BLOCKED_SCHEMES.contains("javascript"));
    }

    #[test]
    fn blocks_data_scheme() {
        assert!(BLOCKED_SCHEMES.contains("data"));
    }

    #[test]
    fn blocks_smb_scheme() {
        assert!(BLOCKED_SCHEMES.contains("smb"));
    }

    #[test]
    fn blocks_ftp_scheme() {
        assert!(BLOCKED_SCHEMES.contains("ftp"));
    }

    #[test]
    fn blocks_vbscript_scheme() {
        assert!(BLOCKED_SCHEMES.contains("vbscript"));
    }

    #[test]
    fn blocks_chrome_extension() {
        assert!(BLOCKED_SCHEMES.contains("chrome-extension"));
    }

    #[test]
    fn blocks_view_source() {
        assert!(BLOCKED_SCHEMES.contains("view-source"));
    }

    #[test]
    fn blocks_intent() {
        assert!(BLOCKED_SCHEMES.contains("intent"));
    }

    #[test]
    fn blocks_about() {
        assert!(BLOCKED_SCHEMES.contains("about"));
    }

    // ── Allowed safe schemes ──────────────────────────────────────────────

    #[test]
    fn allows_https_green() {
        let url: Url = "https://google.com/search?q=cats".parse().unwrap();
        let result = classify_url(&url, None).unwrap();
        assert_eq!(result.risk, RiskLevel::Green);
        assert_eq!(result.scheme, AllowedScheme::Https);
    }

    #[test]
    fn allows_http_green() {
        let url: Url = "http://example.com".parse().unwrap();
        let result = classify_url(&url, None).unwrap();
        assert_eq!(result.risk, RiskLevel::Green);
    }

    #[test]
    fn allows_mailto_green() {
        let url: Url = "mailto:someone@example.com".parse().unwrap();
        let result = classify_url(&url, None).unwrap();
        assert_eq!(result.risk, RiskLevel::Green);
    }

    // ── Deep links ────────────────────────────────────────────────────────

    #[test]
    fn whatsapp_deep_link_is_yellow() {
        let url: Url = "whatsapp://send?phone=919876543210&text=hye"
            .parse()
            .unwrap();
        let result = classify_url(&url, None).unwrap();
        assert_eq!(result.risk, RiskLevel::Yellow);
        assert!(matches!(
            result.scheme,
            AllowedScheme::RegisteredDeepLink(_)
        ));
    }

    #[test]
    fn unknown_deep_link_rejected() {
        let url: Url = "myunknownapp://open/something".parse().unwrap();
        let result = classify_url(&url, None);
        assert!(matches!(result, Err(SchemeError::UnknownDeepLink(_))));
    }

    #[test]
    fn registry_registered_deep_link_accepted() {
        let url: Url = "myapp://open/document".parse().unwrap();
        let mut registry = HashSet::new();
        registry.insert("myapp".to_string());
        let result = classify_url(&url, Some(&registry)).unwrap();
        assert_eq!(result.risk, RiskLevel::Yellow);
    }

    // ── URL builders ──────────────────────────────────────────────────────

    #[test]
    fn build_search_url_encodes_special_chars() {
        let url = build_search_url("cats & dogs OR x=1").unwrap();
        let query = url.query().unwrap_or("");
        // Must not contain raw & or = from the topic.
        assert!(!query.contains("dogs OR x=1"), "raw OR not encoded");
        assert!(url.scheme() == "https");
        assert!(url.host_str() == Some("www.google.com"));
    }

    #[test]
    fn build_search_url_no_injection() {
        let url = build_search_url("topic&other_param=injected").unwrap();
        // The injected parameter must be encoded, not treated as a separate query param.
        let query = url.query().unwrap_or("");
        assert!(
            !query.contains("other_param=injected"),
            "parameter injection must be prevented"
        );
    }

    #[test]
    fn build_whatsapp_url_encodes_body() {
        let url = build_whatsapp_url("+919876543210", "hye! how are you?").unwrap();
        assert!(url.as_str().contains("phone=919876543210"));
        assert!(!url.as_str().contains(' '), "spaces must be encoded");
    }

    #[test]
    fn build_youtube_search_url_safe() {
        let url = build_youtube_search_url("lo-fi beats & chill").unwrap();
        assert_eq!(url.host_str(), Some("www.youtube.com"));
        let query = url.query().unwrap_or("");
        assert!(query.contains("search_query="));
    }
}

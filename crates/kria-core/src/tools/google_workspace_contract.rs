//! Shared Google Workspace envelope and error contract helpers.

pub const GW_PROVIDER: &str = "google_workspace";
pub const GW_SCHEMA_VERSION: &str = "1.1";

pub const GW_META_KEY: &str = "_meta";
pub const GW_META_SCHEMA_VERSION_KEY: &str = "schema_version";
pub const GW_META_TIMESTAMP_KEY: &str = "timestamp";
pub const GW_META_CORRELATION_ID_KEY: &str = "correlation_id";
pub const GW_META_ACCOUNT_KEY: &str = "account";

#[derive(Clone, Debug)]
pub struct GwErrorDescriptor {
    pub code: &'static str,
    pub category: &'static str,
    pub recovery_action: &'static str,
    pub retryable: bool,
    pub user_facing: String,
}

pub fn default_account() -> String {
    std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into())
}

pub fn new_correlation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn kind_for_tool(tool: &str) -> &'static str {
    match tool {
        t if t.contains("Gmail") => "gmail",
        t if t.contains("Calendar") => "calendar",
        t if t.contains("Spreadsheet") => "sheets",
        t if t.contains("Presentation") || t.contains("Slides") => "slides",
        t if t.contains("Form") => "forms",
        t if t.contains("Document") || t.contains("GoogleDoc") => "docs",
        t if t.contains("Folder") || t.contains("Drive") || t.contains("File") => "drive",
        _ => GW_PROVIDER,
    }
}

pub fn envelope_for_tool(
    tool: &str,
    data: serde_json::Value,
    raw_text: Option<&str>,
    correlation_id: Option<&str>,
    account: Option<&str>,
) -> serde_json::Value {
    let correlation_id = correlation_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(new_correlation_id);
    let account = account.map(str::to_string).unwrap_or_else(default_account);

    serde_json::json!({
        "provider": GW_PROVIDER,
        "kind": kind_for_tool(tool),
        "tool": tool,
        "data": data,
        "raw_text": raw_text.unwrap_or(""),
        GW_META_KEY: {
            GW_META_SCHEMA_VERSION_KEY: GW_SCHEMA_VERSION,
            GW_META_TIMESTAMP_KEY: chrono::Utc::now().to_rfc3339(),
            GW_META_CORRELATION_ID_KEY: correlation_id,
            GW_META_ACCOUNT_KEY: account,
        },
    })
}

pub fn error_payload(error: &GwErrorDescriptor, raw: Option<&str>) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "code": error.code,
        "category": error.category,
        "user_facing": error.user_facing,
        "recovery_action": error.recovery_action,
        "retryable": error.retryable,
    });

    if let Some(raw_error) = raw.map(str::trim).filter(|value| !value.is_empty()) {
        let compact = if raw_error.len() > 500 {
            format!("{}…", &raw_error[..500])
        } else {
            raw_error.to_string()
        };
        payload["raw_error"] = serde_json::json!(compact);
    }

    payload
}

pub fn parse_error(raw: &str) -> GwErrorDescriptor {
    let lower = raw.to_ascii_lowercase();

    if raw.contains("accessNotConfigured")
        || raw.contains("has not been used")
        || lower.contains("is disabled")
    {
        let url = raw
            .split_once("https://console")
            .map(|(_, rest)| {
                format!(
                    "https://console{}",
                    rest.split_whitespace().next().unwrap_or("")
                )
            })
            .unwrap_or_default();

        let api_name = if lower.contains("gmail") {
            "Gmail API"
        } else if lower.contains("calendar") {
            "Calendar API"
        } else if lower.contains("drive") {
            "Drive API"
        } else if lower.contains("docs") {
            "Docs API"
        } else if lower.contains("sheets") {
            "Sheets API"
        } else if lower.contains("slides") {
            "Slides API"
        } else {
            "Google API"
        };

        let user_facing = if url.is_empty() {
            format!(
                "{api_name} is disabled in your Google Cloud project. \
                 Enable it at https://console.cloud.google.com/apis/library then restart KRIA."
            )
        } else {
            format!(
                "{api_name} is disabled. Enable it at {url} \
                 (wait ~1 min after enabling, then retry or restart KRIA)."
            )
        };

        return GwErrorDescriptor {
            code: "api_not_enabled",
            category: "configuration",
            recovery_action: "enable_api",
            retryable: false,
            user_facing,
        };
    }

    if raw.contains("invalid_grant")
        || raw.contains("Token has been expired")
        || raw.contains("Token has been revoked")
    {
        return GwErrorDescriptor {
            code: "auth_token_invalid",
            category: "permission",
            recovery_action: "refresh_auth",
            retryable: false,
            user_facing: "Google authentication token expired or revoked. \
                Re-run: bash scripts/setup_google_workspace.sh  then restart KRIA."
                .into(),
        };
    }

    if raw.contains("insufficientPermissions")
        || raw.contains("Request had insufficient authentication scopes")
    {
        return GwErrorDescriptor {
            code: "insufficient_scopes",
            category: "permission",
            recovery_action: "refresh_auth",
            retryable: false,
            user_facing: "Insufficient OAuth scopes. \
                Re-run: bash scripts/setup_google_workspace.sh  to refresh permissions, then restart KRIA."
                .into(),
        };
    }

    if raw.contains("rateLimitExceeded") || raw.contains("quotaExceeded") {
        return GwErrorDescriptor {
            code: "quota_exceeded",
            category: "quota",
            recovery_action: "wait_and_retry",
            retryable: true,
            user_facing: "Google API rate limit or quota exceeded. Wait a minute and try again."
                .into(),
        };
    }

    if raw.contains("Bad Gateway") || raw.contains("status code 502") {
        return GwErrorDescriptor {
            code: "upstream_unavailable",
            category: "transient",
            recovery_action: "retry",
            retryable: true,
            user_facing:
                "Google API temporarily unavailable (502 Bad Gateway). Retry in a few seconds."
                    .into(),
        };
    }

    let user_facing = if raw.len() > 300 {
        format!("{}…", &raw[..300])
    } else {
        raw.to_string()
    };

    GwErrorDescriptor {
        code: "unknown_error",
        category: "transient",
        recovery_action: "retry",
        retryable: false,
        user_facing,
    }
}

pub fn mcp_transport_error(raw: &str) -> GwErrorDescriptor {
    GwErrorDescriptor {
        code: "mcp_call_failed",
        category: "transient",
        recovery_action: "retry",
        retryable: true,
        user_facing: raw.to_string(),
    }
}

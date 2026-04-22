/// Pre-compiled JSON Schema for the `Capability` discriminated union.
///
/// # Security role (Ring 1)
/// When passed to `LocalBackend::chat_with_grammar`, llama.cpp runs this schema
/// through llguidance for constrained decoding — the model's token stream is
/// physically restricted to valid `Capability` JSON.  The schema is computed once
/// at startup and stored in a `OnceLock` so there is zero per-call allocation.
///
/// # Post-generation validation
/// `validate_capability_json` independently re-validates the LLM output to catch
/// any server-side drift (e.g. unconstrained fallback path in `chat_with_grammar`).
use once_cell::sync::OnceCell;
use serde_json::{json, Value};

// ─── Precompiled schema ───────────────────────────────────────────────────────

static CAPABILITY_SCHEMA: OnceCell<Value> = OnceCell::new();

/// Return a reference to the `Capability` JSON Schema.
///
/// Computed exactly once (the first call); all subsequent calls return the cached
/// value with no allocation.
pub fn capability_schema() -> &'static Value {
    CAPABILITY_SCHEMA.get_or_init(build_capability_schema)
}

fn build_capability_schema() -> Value {
    // Reflects the Rust enum:
    //
    //   #[serde(tag = "intent", deny_unknown_fields)]
    //   pub enum Capability {
    //       OpenUrl   { url: Url },
    //       LaunchApp { app_id: CanonicalAppId, args: Vec<SafeArg> },
    //       SendMessage { app: MessagingApp, contact: ContactId, body: MessageBody },
    //       FileWrite { path: SandboxedPath, content: Vec<u8> },
    //       AxInvoke  { app_id: CanonicalAppId, action: AxAction },
    //   }
    //
    // Because serde uses the string "intent" as the discriminator key, each variant
    // is encoded as an object with `"intent": "<VariantName>"` plus the fields.

    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Capability",
        "description": "A typed OS-level action K.R.I.A. may perform. \
                         Use the 'intent' field to select the variant. \
                         No other top-level keys are permitted (deny_unknown_fields).",
        "oneOf": [
            open_url_variant(),
            launch_app_variant(),
            send_message_variant(),
            file_write_variant(),
            ax_invoke_variant(),
        ]
    })
}

fn open_url_variant() -> Value {
    json!({
        "type": "object",
        "required": ["intent", "url"],
        "additionalProperties": false,
        "properties": {
            "intent": { "type": "string", "const": "OpenUrl" },
            "url": {
                "type": "string",
                "format": "uri",
                "description": "Fully-qualified URL. Scheme must be https, http, mailto, tel, or a registered deep-link. \
                                 file://, javascript:, data:, smb:// and similar are BLOCKED."
            }
        }
    })
}

fn launch_app_variant() -> Value {
    json!({
        "type": "object",
        "required": ["intent", "app_id"],
        "additionalProperties": false,
        "properties": {
            "intent": { "type": "string", "const": "LaunchApp" },
            "app_id": {
                "type": "string",
                "description": "Canonical application ID from the installed-app registry (e.g. 'chromium', 'code', 'spotify')."
            },
            "args": {
                "type": "array",
                "items": {
                    "type": "string",
                    "description": "Single launch argument. Must NOT contain shell metacharacters (; & | $ ` < >)."
                },
                "default": []
            }
        }
    })
}

fn send_message_variant() -> Value {
    json!({
        "type": "object",
        "required": ["intent", "app", "contact", "body"],
        "additionalProperties": false,
        "properties": {
            "intent": { "type": "string", "const": "SendMessage" },
            "app": {
                "type": "string",
                "enum": ["WhatsApp", "Gmail", "Telegram", "Signal"],
                "description": "Messaging application."
            },
            "contact": {
                "type": "object",
                "required": ["display_name", "identifier", "app"],
                "additionalProperties": false,
                "properties": {
                    "display_name": { "type": "string" },
                    "identifier": {
                        "type": "string",
                        "description": "E.164 phone number (WhatsApp/Signal) or email address (Gmail). \
                                         Must be pre-resolved from the contacts database; leave empty if unknown."
                    },
                    "app": {
                        "type": "string",
                        "enum": ["WhatsApp", "Gmail", "Telegram", "Signal"]
                    }
                }
            },
            "body": {
                "type": "string",
                "maxLength": 4096,
                "description": "Message body text."
            }
        }
    })
}

fn file_write_variant() -> Value {
    json!({
        "type": "object",
        "required": ["intent", "path", "content"],
        "additionalProperties": false,
        "properties": {
            "intent": { "type": "string", "const": "FileWrite" },
            "path": {
                "type": "string",
                "description": "Absolute path within the user's allowed write roots (~/, /tmp). \
                                 Paths under /etc, /boot, /root, /usr, /var, /proc, /sys are BLOCKED."
            },
            "content": {
                "type": "string",
                "description": "UTF-8 file content to write."
            }
        }
    })
}

fn ax_invoke_variant() -> Value {
    json!({
        "type": "object",
        "required": ["intent", "app_id", "action"],
        "additionalProperties": false,
        "properties": {
            "intent": { "type": "string", "const": "AxInvoke" },
            "app_id": {
                "type": "string",
                "description": "Canonical ID of the target application."
            },
            "action": {
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["Click", "TypeText", "Focus", "SelectItem"],
                        "description": "Accessibility action to perform."
                    },
                    "text": {
                        "type": "string",
                        "description": "Required when type is TypeText."
                    },
                    "item": {
                        "type": "string",
                        "description": "Required when type is SelectItem."
                    }
                }
            }
        }
    })
}

// ─── Post-generation validation ───────────────────────────────────────────────

/// Errors produced by `validate_capability_json`.
#[derive(Debug, thiserror::Error)]
pub enum CapabilitySchemaError {
    #[error("not valid JSON: {0}")]
    MalformedJson(#[from] serde_json::Error),
    #[error("missing required field: '{0}'")]
    MissingField(String),
    #[error("unknown intent: '{0}'")]
    UnknownIntent(String),
    #[error("blocked value: {0}")]
    BlockedValue(String),
}

/// Lightweight structural validation of a raw JSON string against the `Capability`
/// schema.  This is not a full JSON-Schema validator — it checks the fields that
/// matter for security (intent discriminator, blocked schemes/paths).
pub fn validate_capability_json(raw: &str) -> Result<(), CapabilitySchemaError> {
    let v: Value = serde_json::from_str(raw)?;

    let intent = v["intent"]
        .as_str()
        .ok_or_else(|| CapabilitySchemaError::MissingField("intent".into()))?;

    match intent {
        "OpenUrl" => {
            let url = v["url"]
                .as_str()
                .ok_or_else(|| CapabilitySchemaError::MissingField("url".into()))?
                .to_ascii_lowercase();
            // Scheme allow-list (mirrors scheme.rs BLOCKED_SCHEMES).
            let blocked = [
                "file://", "javascript:", "data:", "vbscript:", "smb://", "cifs://",
                "nfs://", "ftp://", "sftp://", "about:", "chrome-extension://",
                "chrome-devtools://", "moz-extension://", "resource:", "view-source:",
                "intent:", "android-app:", "vnc://", "rdp://", "ssh://", "content://",
            ];
            for scheme in blocked {
                if url.starts_with(scheme) {
                    return Err(CapabilitySchemaError::BlockedValue(format!(
                        "scheme '{scheme}' is permanently blocked"
                    )));
                }
            }
        }
        "LaunchApp" => {
            if v["app_id"].as_str().is_none() {
                return Err(CapabilitySchemaError::MissingField("app_id".into()));
            }
            // Validate each arg has no shell metacharacters.
            if let Some(args) = v["args"].as_array() {
                for arg in args {
                    if let Some(s) = arg.as_str() {
                        if s.chars().any(|c| matches!(c, ';' | '&' | '|' | '$' | '`' | '<' | '>')) {
                            return Err(CapabilitySchemaError::BlockedValue(
                                "shell metacharacter in arg".into(),
                            ));
                        }
                    }
                }
            }
        }
        "SendMessage" => {
            if v["app"].as_str().is_none() {
                return Err(CapabilitySchemaError::MissingField("app".into()));
            }
            if v["contact"]["identifier"].as_str().is_none() {
                return Err(CapabilitySchemaError::MissingField("contact.identifier".into()));
            }
            if v["body"].as_str().is_none() {
                return Err(CapabilitySchemaError::MissingField("body".into()));
            }
        }
        "FileWrite" => {
            let path = v["path"]
                .as_str()
                .ok_or_else(|| CapabilitySchemaError::MissingField("path".into()))?;
            let blocked_roots = ["/etc/", "/boot/", "/root/", "/usr/", "/var/", "/proc/", "/sys/"];
            for root in blocked_roots {
                if path.starts_with(root) {
                    return Err(CapabilitySchemaError::BlockedValue(format!(
                        "path '{path}' is under blocked root '{root}'"
                    )));
                }
            }
            if path.contains("../") {
                return Err(CapabilitySchemaError::BlockedValue(
                    "path traversal '../' is not allowed".into(),
                ));
            }
        }
        "AxInvoke" => {
            if v["app_id"].as_str().is_none() {
                return Err(CapabilitySchemaError::MissingField("app_id".into()));
            }
            if v["action"]["type"].as_str().is_none() {
                return Err(CapabilitySchemaError::MissingField("action.type".into()));
            }
        }
        other => return Err(CapabilitySchemaError::UnknownIntent(other.into())),
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_computed_once_and_cached() {
        let a = capability_schema();
        let b = capability_schema();
        assert!(std::ptr::eq(a, b), "should return the same static reference");
    }

    #[test]
    fn schema_has_five_variants() {
        let schema = capability_schema();
        let variants = schema["oneOf"].as_array().unwrap();
        assert_eq!(variants.len(), 5);
    }

    #[test]
    fn validate_open_url_https_ok() {
        assert!(validate_capability_json(r#"{"intent":"OpenUrl","url":"https://example.com"}"#).is_ok());
    }

    #[test]
    fn validate_open_url_file_blocked() {
        let err = validate_capability_json(r#"{"intent":"OpenUrl","url":"file:///etc/passwd"}"#);
        assert!(matches!(err, Err(CapabilitySchemaError::BlockedValue(_))));
    }

    #[test]
    fn validate_open_url_javascript_blocked() {
        let err = validate_capability_json(r#"{"intent":"OpenUrl","url":"javascript:alert(1)"}"#);
        assert!(matches!(err, Err(CapabilitySchemaError::BlockedValue(_))));
    }

    #[test]
    fn validate_launch_app_shell_metachar_blocked() {
        let err = validate_capability_json(
            r#"{"intent":"LaunchApp","app_id":"bash","args":["--rcfile","evil; rm -rf /"]}"#,
        );
        assert!(matches!(err, Err(CapabilitySchemaError::BlockedValue(_))));
    }

    #[test]
    fn validate_file_write_etc_blocked() {
        let err = validate_capability_json(
            r#"{"intent":"FileWrite","path":"/etc/crontab","content":"evil"}"#,
        );
        assert!(matches!(err, Err(CapabilitySchemaError::BlockedValue(_))));
    }

    #[test]
    fn validate_file_write_traversal_blocked() {
        let err = validate_capability_json(
            r#"{"intent":"FileWrite","path":"/home/user/../../../etc/hosts","content":"evil"}"#,
        );
        assert!(matches!(err, Err(CapabilitySchemaError::BlockedValue(_))));
    }

    #[test]
    fn validate_send_message_ok() {
        let ok = validate_capability_json(
            r#"{"intent":"SendMessage","app":"WhatsApp","contact":{"display_name":"Anjali","identifier":"+919876543210","app":"WhatsApp"},"body":"hey!"}"#,
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn validate_unknown_intent_rejected() {
        let err = validate_capability_json(r#"{"intent":"ShellExec","cmd":"rm -rf /"}"#);
        assert!(matches!(err, Err(CapabilitySchemaError::UnknownIntent(_))));
    }

    #[test]
    fn validate_missing_intent_field() {
        let err = validate_capability_json(r#"{"url":"https://example.com"}"#);
        assert!(matches!(err, Err(CapabilitySchemaError::MissingField(_))));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  tests/common/mod.rs — Shared helpers for the KRIA comprehensive test suite
//
//  Provides:
//    • MockLlmServer   – canned /v1/chat/completions responses, request capture
//    • SandboxDir      – RAII temp dir rooted under target/test-sandbox/<uuid>
//    • EnvGuard        – skip helpers for services absent in CI
//    • assert_*        – quality assertion helpers
// ─────────────────────────────────────────────────────────────────────────────

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ═══════════════════════════════════════════════════════════════════════════
//  MockLlmServer
// ═══════════════════════════════════════════════════════════════════════════

/// A lightweight synchronous TCP server that emulates the OpenAI-compatible
/// `/v1/chat/completions` endpoint used by the KRIA agent loop.
///
/// Feed it a queue of `serde_json::Value` bodies; each incoming HTTP request
/// pops the next body from the queue and returns it.  After the queue is
/// exhausted the server thread exits cleanly.
///
/// Captured raw request bodies are available via [`MockLlmServer::captured`].
pub struct MockLlmServer {
    pub base_url: String,
    pub captured: Arc<Mutex<Vec<String>>>,
    _handle: std::thread::JoinHandle<()>,
}

impl MockLlmServer {
    pub fn new(responses: Vec<serde_json::Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock LLM server");
        let addr = listener.local_addr().expect("local addr");
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cap2 = captured.clone();

        let handle = std::thread::spawn(move || {
            for body in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };

                let req_body = read_http_request_body(&mut stream);
                if let Ok(mut v) = cap2.lock() {
                    v.push(req_body);
                }

                let payload = body.to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            base_url: format!("http://{}/v1", addr),
            captured,
            _handle: handle,
        }
    }

    /// Returns the number of requests captured so far.
    pub fn request_count(&self) -> usize {
        self.captured.lock().unwrap().len()
    }

    /// Returns a copy of all captured request bodies.
    pub fn all_requests(&self) -> Vec<String> {
        self.captured.lock().unwrap().clone()
    }
}

// ─── canned response builders ─────────────────────────────────────────────

/// Build a chat-completion response that includes a tool call.
pub fn tool_call_response(tool_name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": arguments.to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

/// Build a plain text chat-completion response (no tool call).
pub fn text_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

/// Build a multi-tool-call response for chained workflow tests.
pub fn multi_tool_call_response(calls: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let tool_calls: Vec<serde_json::Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            serde_json::json!({
                "id": format!("call_{i}"),
                "type": "function",
                "function": { "name": name, "arguments": args.to_string() }
            })
        })
        .collect();

    serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": tool_calls
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

// ─── internal HTTP reader ──────────────────────────────────────────────────

fn read_http_request_body(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::<u8>::new();
    let mut tmp = [0u8; 4096];

    // Read until we see the header/body separator
    loop {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }

    // Extract content-length to read full body
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0) + 4;
    let header_text = String::from_utf8_lossy(&buf[..header_end.min(buf.len())]);
    let mut content_len = 0usize;
    for line in header_text.lines() {
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_len = v.trim().parse().unwrap_or(0);
            break;
        }
    }

    let needed = header_end + content_len;
    while buf.len() < needed {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }

    String::from_utf8_lossy(&buf[header_end..]).into_owned()
}

// ═══════════════════════════════════════════════════════════════════════════
//  SandboxDir
// ═══════════════════════════════════════════════════════════════════════════

/// A RAII temporary directory rooted at `<workspace>/target/test-sandbox/<uuid>`.
///
/// The sandbox is created on construction and deleted on Drop.  Tests that
/// need to create / read / write / delete real files should use this instead
/// of writing to user home directories.
pub struct SandboxDir {
    pub path: PathBuf,
}

impl SandboxDir {
    pub fn new() -> Self {
        let workspace = workspace_root();
        let sandbox_root = workspace.join("target").join("test-sandbox");
        let id = uuid_v4_hex();
        let path = sandbox_root.join(id);
        std::fs::create_dir_all(&path).expect("create sandbox dir");
        Self { path }
    }

    /// Return a path inside the sandbox without creating it.
    pub fn child(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    /// Create a file with content and return its path.
    pub fn write_file(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.child(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&p, content).expect("write sandbox file");
        p
    }

    /// Read a file from the sandbox.
    pub fn read_file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.child(rel)).expect("read sandbox file")
    }

    /// True if the path exists in the sandbox.
    pub fn exists(&self, rel: &str) -> bool {
        self.child(rel).exists()
    }
}

impl Drop for SandboxDir {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl Default for SandboxDir {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  EnvGuard — CI-skip helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Returns true if an internet connection is available.
/// Uses a quick TCP probe to 1.1.1.1:80.
pub fn internet_available() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    TcpStream::connect_timeout(
        &"1.1.1.1:80".parse().unwrap(),
        Duration::from_secs(3),
    )
    .is_ok()
}

/// Returns true if the local LLM server appears to be running.
pub fn llm_available() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    TcpStream::connect_timeout(
        &"127.0.0.1:8080".parse().unwrap(),
        Duration::from_secs(2),
    )
    .is_ok()
}

/// Returns true if a GNOME / X11 display is available.
pub fn gnome_display_available() -> bool {
    std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
}

/// Returns true if the DBUS session bus is accessible.
pub fn dbus_available() -> bool {
    std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok()
        || Path::new(&format!("/run/user/{}/bus", unsafe { libc::getuid() })).exists()
}

/// Returns true if the Python sidecar binary/module is present.
pub fn sidecar_available() -> bool {
    // Check that the kria_modules package is importable
    std::process::Command::new("python3")
        .args(["-c", "import kria_modules"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns true if Google Workspace credentials file exists.
pub fn gworkspace_creds_available() -> bool {
    dirs::home_dir()
        .map(|h| {
            h.join(".config/kria/gworkspace_credentials.json").exists()
                || std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
        })
        .unwrap_or(false)
}

/// Returns true if KRIA_DANGEROUS env var is set to "1".
pub fn dangerous_enabled() -> bool {
    std::env::var("KRIA_DANGEROUS").as_deref() == Ok("1")
}

/// Returns true if KRIA_REAL_LLM env var is set to "1" AND LLM is reachable.
pub fn real_llm_enabled() -> bool {
    std::env::var("KRIA_REAL_LLM").as_deref() == Ok("1") && llm_available()
}

/// Returns true if KRIA_VOICE_LIVE env var is set to "1".
pub fn voice_live_enabled() -> bool {
    std::env::var("KRIA_VOICE_LIVE").as_deref() == Ok("1")
}

// ═══════════════════════════════════════════════════════════════════════════
//  Quality Assertions
// ═══════════════════════════════════════════════════════════════════════════

/// Regex patterns for raw shell commands that Kria must NEVER emit verbatim
/// in a user-facing text response when a structured tool exists.
static BASH_HALLUCINATION_PATTERNS: &[&str] = &[
    r"ps aux",
    r"df -h",
    r"free -m",
    r"nmcli",
    r"iwconfig",
    r"ifconfig",
    r"netstat",
    r"lsblk",
    r"du -sh",
    r"sudo apt",
    r"sudo rm",
    r"chmod \+x",
    r"curl -s",
    r"wget ",
    r"grep -r",
    r"find / -name",
];

/// Assert that a user-visible response text does not contain raw bash commands
/// that should have been handled by a structured tool call instead.
pub fn assert_no_bash_hallucination(text: &str) {
    for pattern in BASH_HALLUCINATION_PATTERNS {
        let re = regex::Regex::new(pattern).unwrap();
        assert!(
            !re.is_match(text),
            "Response contains raw bash command matching `{pattern}` — \
             Kria should call a tool instead of emitting shell commands.\n\
             Response was: {text}"
        );
    }
}

/// Assert that a response text is within a sane length range.
pub fn assert_response_length_sane(text: &str, min_chars: usize, max_chars: usize) {
    assert!(
        text.len() >= min_chars,
        "Response too short ({} chars, min {min_chars}): {text}",
        text.len()
    );
    assert!(
        text.len() <= max_chars,
        "Response too long ({} chars, max {max_chars})",
        text.len()
    );
}

/// Assert that a `ToolResult`-shaped JSON value indicates success.
pub fn assert_tool_success(result: &serde_json::Value) {
    assert!(
        result["success"].as_bool().unwrap_or(false),
        "Tool result indicates failure. Error: {:?}\nFull result: {}",
        result["error"],
        serde_json::to_string_pretty(result).unwrap_or_default()
    );
}

/// Assert that a nested JSON field exists and satisfies a predicate.
pub fn assert_result_field<F: Fn(&serde_json::Value) -> bool>(
    result: &serde_json::Value,
    field_path: &str,
    predicate: F,
    description: &str,
) {
    let parts: Vec<&str> = field_path.split('.').collect();
    let mut current = result;
    for part in &parts {
        current = match current.get(*part) {
            Some(v) => v,
            None => panic!(
                "Field `{field_path}` not found in result. \
                 Missing `{part}`. Full result: {}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            ),
        };
    }
    assert!(
        predicate(current),
        "Field `{field_path}` did not satisfy predicate: {description}. \
         Value was: {current}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Internal utilities
// ═══════════════════════════════════════════════════════════════════════════

/// Returns the workspace root directory (where Cargo.toml lives).
fn workspace_root() -> PathBuf {
    // Walk up from this file's manifest dir
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    // kria-core is 2 levels below workspace root: crates/kria-core
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest)
}

/// Generate a short random hex string for sandbox directory names.
fn uuid_v4_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    // Mix in thread id for uniqueness across parallel tests
    let tid = std::thread::current().id();
    format!("{:08x}-{:?}", ts, tid)
        .replace("ThreadId(", "")
        .replace(')', "")
        .replace(' ', "")
}

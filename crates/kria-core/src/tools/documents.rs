use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

/// Shared sidecar handle for document tools.
#[derive(Clone)]
struct DocSidecar(Option<Arc<Mutex<Arc<SidecarBridge>>>>);

impl DocSidecar {
    async fn try_extract(&self, path: &str, operations: &[&str]) -> Option<serde_json::Value> {
        let bridge = self.0.as_ref()?;
        let bridge = bridge.lock().await;
        let params = serde_json::json!({
            "file": path,
            "operations": operations,
        });
        bridge.request("document.extract", params).await.ok()
    }
}

struct ParseDocument {
    sidecar: DocSidecar,
}

#[async_trait]
impl ToolHandler for ParseDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Try sidecar for rich extraction (PDF, DOCX)
        if matches!(ext.as_str(), "pdf" | "docx" | "xlsx") {
            if let Some(result) = self
                .sidecar
                .try_extract(path, &["text", "tables", "sections"])
                .await
            {
                return ToolResult::ok(serde_json::json!({
                    "path": path, "format": ext, "backend": "sidecar",
                    "result": result,
                }));
            }
        }

        // Fallback to Rust-native extraction
        match ext.as_str() {
            "txt" | "md" | "log" | "json" | "yaml" | "yml" | "toml" | "csv" | "xml" => {
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => {
                        let max = 50000;
                        let truncated = content.len() > max;
                        let text = if truncated { &content[..max] } else { &content };
                        ToolResult::ok(serde_json::json!({
                            "path": path, "format": ext, "content": text,
                            "truncated": truncated, "total_chars": content.len(),
                            "backend": "native",
                        }))
                    }
                    Err(e) => ToolResult::err(format!("read failed: {e}")),
                }
            }
            "pdf" => {
                // Fallback: poppler's pdftotext
                let output = tokio::process::Command::new("pdftotext")
                    .args([path, "-"])
                    .output()
                    .await;
                match output {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        ToolResult::ok(serde_json::json!({
                            "path": path, "format": "pdf", "content": text,
                            "chars": text.len(), "backend": "pdftotext",
                        }))
                    }
                    _ => ToolResult::err("PDF parsing failed (install pdftotext or start sidecar)"),
                }
            }
            "docx" => {
                // Fallback: pandoc
                let output = tokio::process::Command::new("pandoc")
                    .args(["-f", "docx", "-t", "plain", path])
                    .output()
                    .await;
                match output {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        ToolResult::ok(serde_json::json!({
                            "path": path, "format": "docx", "content": text,
                            "backend": "pandoc",
                        }))
                    }
                    _ => ToolResult::err("DOCX parsing failed (install pandoc or start sidecar)"),
                }
            }
            _ => ToolResult::err(format!("unsupported document format: {ext}")),
        }
    }
}

struct ParseCsv {
    sidecar: DocSidecar,
}

#[async_trait]
impl ToolHandler for ParseCsv {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let max_rows = params["max_rows"].as_u64().unwrap_or(100) as usize;

        // Try sidecar for pandas analysis (schema detection, statistics)
        if let Some(result) = self.sidecar.try_extract(path, &["text", "tables"]).await {
            return ToolResult::ok(serde_json::json!({
                "path": path, "format": "csv", "backend": "sidecar",
                "result": result,
            }));
        }

        // Fallback: native CSV parsing with basic analysis
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let header = lines.first().copied().unwrap_or("");
                let columns: Vec<&str> = header.split(',').collect();
                let row_count = lines.len().saturating_sub(1);
                let sample_rows: Vec<Vec<&str>> = lines
                    .iter()
                    .skip(1)
                    .take(max_rows)
                    .map(|l| l.split(',').collect())
                    .collect();

                ToolResult::ok(serde_json::json!({
                    "path": path, "format": "csv", "backend": "native",
                    "columns": columns,
                    "column_count": columns.len(),
                    "row_count": row_count,
                    "sample_rows": sample_rows,
                    "truncated": row_count > max_rows,
                }))
            }
            Err(e) => ToolResult::err(format!("CSV read failed: {e}")),
        }
    }
}

struct SummarizeDocument {
    sidecar: DocSidecar,
}

#[async_trait]
impl ToolHandler for SummarizeDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Try sidecar for structured summary (PDF/DOCX)
        if matches!(ext.as_str(), "pdf" | "docx") {
            if let Some(result) = self.sidecar.try_extract(path, &["text", "sections"]).await {
                // Produce a structured summary from sidecar output
                let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let word_count = text.split_whitespace().count();
                let sections = result
                    .get("sections")
                    .cloned()
                    .unwrap_or(serde_json::json!([]));
                let preview: String = text.chars().take(500).collect();
                return ToolResult::ok(serde_json::json!({
                    "path": path, "format": ext, "backend": "sidecar",
                    "word_count": word_count,
                    "char_count": text.len(),
                    "sections": sections,
                    "preview": preview,
                }));
            }
        }

        // Fallback: basic text analysis
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("read failed: {e}")),
        };
        let word_count = content.split_whitespace().count();
        let line_count = content.lines().count();
        let preview: String = content.chars().take(500).collect();
        ToolResult::ok(serde_json::json!({
            "path": path, "backend": "native",
            "word_count": word_count,
            "line_count": line_count,
            "char_count": content.len(),
            "preview": preview,
        }))
    }
}

pub fn register(reg: &ToolRegistry) {
    register_with_sidecar(reg, None);
}

pub fn register_with_sidecar(reg: &ToolRegistry, sidecar: Option<Arc<SidecarBridge>>) {
    let doc_sc = DocSidecar(sidecar.map(|s| Arc::new(Mutex::new(s))));

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "parse_document".into(),
                description:
                    "Extract text from any document (PDF, DOCX, CSV, TXT, MD, JSON, YAML, etc.)"
                        .into(),
                category: "documents".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![param("path", "string", "Document file path", true)],
            },
            Arc::new(ParseDocument {
                sidecar: doc_sc.clone(),
            }),
        ),
        (
            ToolDef {
                name: "parse_csv".into(),
                description: "Parse CSV file with column detection and sample rows".into(),
                category: "documents".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("path", "string", "CSV file path", true),
                    param(
                        "max_rows",
                        "integer",
                        "Max sample rows (default 100)",
                        false,
                    ),
                ],
            },
            Arc::new(ParseCsv {
                sidecar: doc_sc.clone(),
            }),
        ),
        (
            ToolDef {
                name: "summarize_document".into(),
                description: "Get document statistics, sections, and preview".into(),
                category: "documents".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![param("path", "string", "File path", true)],
            },
            Arc::new(SummarizeDocument { sidecar: doc_sc }),
        ),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}

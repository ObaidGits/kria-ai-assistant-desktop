use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct ParseDocument;
#[async_trait] impl ToolHandler for ParseDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let ext = std::path::Path::new(path).extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

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
                        }))
                    }
                    Err(e) => ToolResult::err(format!("read failed: {e}"))
                }
            }
            "pdf" => {
                // Use poppler's pdftotext for PDF parsing
                let output = tokio::process::Command::new("pdftotext")
                    .args([path, "-"])
                    .output().await;
                match output {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        ToolResult::ok(serde_json::json!({
                            "path": path, "format": "pdf", "content": text,
                            "chars": text.len(),
                        }))
                    }
                    _ => ToolResult::err("PDF parsing failed (pdftotext required)")
                }
            }
            "docx" => {
                // Use pandoc for DOCX
                let output = tokio::process::Command::new("pandoc")
                    .args(["-f", "docx", "-t", "plain", path])
                    .output().await;
                match output {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        ToolResult::ok(serde_json::json!({
                            "path": path, "format": "docx", "content": text,
                        }))
                    }
                    _ => ToolResult::err("DOCX parsing failed (pandoc required)")
                }
            }
            _ => ToolResult::err(format!("unsupported document format: {ext}"))
        }
    }
}

struct SummarizeDocument;
#[async_trait] impl ToolHandler for SummarizeDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("read failed: {e}")),
        };
        let word_count = content.split_whitespace().count();
        let line_count = content.lines().count();
        let preview: String = content.chars().take(500).collect();
        ToolResult::ok(serde_json::json!({
            "path": path,
            "word_count": word_count,
            "line_count": line_count,
            "char_count": content.len(),
            "preview": preview,
            "note": "Full summarization requires LLM pass (delegated to agent layer)",
        }))
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let formats = ["pdf", "docx", "xlsx", "csv", "txt", "md"];
    for fmt in &formats {
        let _name = format!("parse_{fmt}");
        // All parse tools share the same handler
    }

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "parse_pdf".into(), description: "Extract text from a PDF file".into(),
            category: "documents".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "PDF file path", true)],
        }, Arc::new(ParseDocument)),
        (ToolDef {
            name: "parse_docx".into(), description: "Extract text from a DOCX file".into(),
            category: "documents".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "DOCX file path", true)],
        }, Arc::new(ParseDocument)),
        (ToolDef {
            name: "parse_csv".into(), description: "Read and display CSV file contents".into(),
            category: "documents".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("path", "string", "CSV file path", true)],
        }, Arc::new(ParseDocument)),
        (ToolDef {
            name: "summarize_document".into(), description: "Get document statistics and a preview".into(),
            category: "documents".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("path", "string", "File path", true)],
        }, Arc::new(SummarizeDocument)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}

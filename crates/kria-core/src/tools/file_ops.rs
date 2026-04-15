use std::sync::Arc;
use std::path::PathBuf;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct ReadFile;
#[async_trait] impl ToolHandler for ReadFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let max = params["max_chars"].as_u64().unwrap_or(50000) as usize;
                let truncated = if content.len() > max { &content[..max] } else { &content };
                ToolResult::ok(serde_json::json!({
                    "path": path,
                    "content": truncated,
                    "size_bytes": content.len(),
                    "truncated": content.len() > max,
                }))
            }
            Err(e) => ToolResult::err(format!("read_file failed: {e}"))
        }
    }
}

struct SearchFiles;
#[async_trait] impl ToolHandler for SearchFiles {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let dir = params["directory"].as_str().unwrap_or(".");
        let pattern = params["pattern"].as_str().unwrap_or("*");
        let max = params["max_results"].as_u64().unwrap_or(50) as usize;

        let glob = globset::GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map(|g| g.compile_matcher());

        let mut results = Vec::new();
        for entry in walkdir::WalkDir::new(dir).max_depth(10).into_iter().filter_map(|e| e.ok()) {
            if results.len() >= max { break; }
            if let Some(ref m) = glob.as_ref().ok() {
                if m.is_match(entry.file_name().to_string_lossy().as_ref()) {
                    results.push(entry.path().to_string_lossy().to_string());
                }
            }
        }
        ToolResult::ok(serde_json::json!({ "matches": results, "count": results.len() }))
    }
}

struct ListDirectory;
#[async_trait] impl ToolHandler for ListDirectory {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        match tokio::fs::read_dir(path).await {
            Ok(mut entries) => {
                let mut items = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let meta = entry.metadata().await.ok();
                    items.push(serde_json::json!({
                        "name": entry.file_name().to_string_lossy(),
                        "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    }));
                }
                ToolResult::ok(serde_json::json!({ "path": path, "entries": items }))
            }
            Err(e) => ToolResult::err(format!("list_directory failed: {e}"))
        }
    }
}

struct GetFileInfo;
#[async_trait] impl ToolHandler for GetFileInfo {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        match tokio::fs::metadata(path).await {
            Ok(meta) => {
                let modified = meta.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
                });
                ToolResult::ok(serde_json::json!({
                    "path": path,
                    "size_bytes": meta.len(),
                    "is_dir": meta.is_dir(),
                    "is_file": meta.is_file(),
                    "modified_epoch": modified,
                    "readonly": meta.permissions().readonly(),
                }))
            }
            Err(e) => ToolResult::err(format!("get_file_info failed: {e}"))
        }
    }
}

struct CalculateDirSize;
#[async_trait] impl ToolHandler for CalculateDirSize {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let total: u64 = walkdir::WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        ToolResult::ok(serde_json::json!({
            "path": path,
            "total_bytes": total,
            "total_mb": total / (1024 * 1024),
        }))
    }
}

struct WriteFile;
#[async_trait] impl ToolHandler for WriteFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let content = params["content"].as_str().unwrap_or("");
        let max_size = 10 * 1024 * 1024; // 10MB
        if content.len() > max_size {
            return ToolResult::err("content exceeds 10MB limit");
        }
        if let Some(parent) = PathBuf::from(path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(path, content).await {
            Ok(_) => ToolResult::ok(serde_json::json!({
                "path": path,
                "bytes_written": content.len(),
            })),
            Err(e) => ToolResult::err(format!("write_file failed: {e}"))
        }
    }
}

struct CreateDirectory;
#[async_trait] impl ToolHandler for CreateDirectory {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        match tokio::fs::create_dir_all(path).await {
            Ok(_) => ToolResult::ok(serde_json::json!({ "path": path, "created": true })),
            Err(e) => ToolResult::err(format!("create_directory failed: {e}"))
        }
    }
}

struct RenameFile;
#[async_trait] impl ToolHandler for RenameFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let source = params["source"].as_str().unwrap_or("");
        let destination = params["destination"].as_str().unwrap_or("");
        match tokio::fs::rename(source, destination).await {
            Ok(_) => ToolResult::ok(serde_json::json!({ "source": source, "destination": destination })),
            Err(e) => ToolResult::err(format!("rename_file failed: {e}"))
        }
    }
}

struct CopyFile;
#[async_trait] impl ToolHandler for CopyFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let source = params["source"].as_str().unwrap_or("");
        let destination = params["destination"].as_str().unwrap_or("");
        match tokio::fs::copy(source, destination).await {
            Ok(bytes) => ToolResult::ok(serde_json::json!({
                "source": source, "destination": destination, "bytes_copied": bytes,
            })),
            Err(e) => ToolResult::err(format!("copy_file failed: {e}"))
        }
    }
}

struct DeleteFile;
#[async_trait] impl ToolHandler for DeleteFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        match tokio::fs::remove_file(path).await {
            Ok(_) => ToolResult::ok(serde_json::json!({ "path": path, "deleted": true })),
            Err(e) => ToolResult::err(format!("delete_file failed: {e}"))
        }
    }
}

struct DeleteDirectory;
#[async_trait] impl ToolHandler for DeleteDirectory {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        match tokio::fs::remove_dir_all(path).await {
            Ok(_) => ToolResult::ok(serde_json::json!({ "path": path, "deleted": true })),
            Err(e) => ToolResult::err(format!("delete_directory failed: {e}"))
        }
    }
}

struct MoveFile;
#[async_trait] impl ToolHandler for MoveFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let source = params["source"].as_str().unwrap_or("");
        let destination = params["destination"].as_str().unwrap_or("");
        // Try rename first (same filesystem), fallback to copy+delete
        if tokio::fs::rename(source, destination).await.is_ok() {
            return ToolResult::ok(serde_json::json!({
                "source": source, "destination": destination
            }));
        }
        match tokio::fs::copy(source, destination).await {
            Ok(_) => {
                let _ = tokio::fs::remove_file(source).await;
                ToolResult::ok(serde_json::json!({
                    "source": source, "destination": destination
                }))
            }
            Err(e) => ToolResult::err(format!("move_file failed: {e}"))
        }
    }
}

// ─── Registration ───

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // GREEN
        (ToolDef {
            name: "read_file".into(), description: "Read the contents of a file".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("path", "string", "Absolute path to the file", true),
                param("max_chars", "integer", "Max characters to return (default 50000)", false),
            ],
        }, Arc::new(ReadFile)),
        (ToolDef {
            name: "search_files".into(), description: "Search for files matching a glob pattern".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("directory", "string", "Starting directory", true),
                param("pattern", "string", "Glob pattern (e.g. *.txt)", true),
                param("max_results", "integer", "Max results (default 50)", false),
            ],
        }, Arc::new(SearchFiles)),
        (ToolDef {
            name: "list_directory".into(), description: "List contents of a directory".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("path", "string", "Directory path", true)],
        }, Arc::new(ListDirectory)),
        (ToolDef {
            name: "get_file_info".into(), description: "Get file metadata (size, type, modified time)".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("path", "string", "File path", true)],
        }, Arc::new(GetFileInfo)),
        (ToolDef {
            name: "calculate_dir_size".into(), description: "Calculate total size of a directory".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("path", "string", "Directory path", true)],
        }, Arc::new(CalculateDirSize)),
        // YELLOW
        (ToolDef {
            name: "write_file".into(), description: "Write content to a file (max 10MB)".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![
                param("path", "string", "File path", true),
                param("content", "string", "Content to write", true),
            ],
        }, Arc::new(WriteFile)),
        (ToolDef {
            name: "create_directory".into(), description: "Create a directory (with parents)".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![param("path", "string", "Directory path", true)],
        }, Arc::new(CreateDirectory)),
        (ToolDef {
            name: "rename_file".into(), description: "Rename a file or directory".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![
                param("source", "string", "Current path", true),
                param("destination", "string", "New name/path", true),
            ],
        }, Arc::new(RenameFile)),
        (ToolDef {
            name: "copy_file".into(), description: "Copy a file".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![
                param("source", "string", "Source path", true),
                param("destination", "string", "Destination path", true),
            ],
        }, Arc::new(CopyFile)),
        // RED
        (ToolDef {
            name: "delete_file".into(), description: "Delete a file (requires approval)".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![param("path", "string", "File path", true)],
        }, Arc::new(DeleteFile)),
        (ToolDef {
            name: "delete_directory".into(), description: "Delete a directory and all contents (requires approval)".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![param("path", "string", "Directory path", true)],
        }, Arc::new(DeleteDirectory)),
        (ToolDef {
            name: "move_file".into(), description: "Move a file or directory".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![
                param("source", "string", "Source path", true),
                param("destination", "string", "Destination path", true),
            ],
        }, Arc::new(MoveFile)),
    ];

    for (def, handler) in tools {
        reg.register(def, handler);
    }
}

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

// ─── Phase 3: Enhanced file search & code intelligence ───

struct SearchFileContents;
#[async_trait] impl ToolHandler for SearchFileContents {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let dir = params["directory"].as_str().unwrap_or(".");
        let query = params["query"].as_str().unwrap_or("");
        let max = params["max_results"].as_u64().unwrap_or(20) as usize;
        let context_lines = params["context_lines"].as_u64().unwrap_or(2) as usize;
        let case_sensitive = params["case_sensitive"].as_bool().unwrap_or(false);

        if query.is_empty() {
            return ToolResult::err("query parameter is required");
        }

        let search_query = if case_sensitive { query.to_string() } else { query.to_lowercase() };
        let mut results = Vec::new();

        for entry in walkdir::WalkDir::new(dir).max_depth(10).into_iter().filter_map(|e| e.ok()) {
            if results.len() >= max { break; }
            if !entry.file_type().is_file() { continue; }
            let path = entry.path();
            // Skip binary files by extension
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let binary_exts = ["png", "jpg", "jpeg", "gif", "bmp", "ico", "woff", "woff2", "ttf", "otf",
                "mp3", "mp4", "avi", "mov", "zip", "tar", "gz", "rar", "7z", "exe", "dll", "so",
                "o", "a", "dylib", "bin", "dat", "db", "sqlite", "gguf", "onnx", "pdf"];
            if binary_exts.contains(&ext.as_str()) { continue; }
            // Skip files larger than 1MB
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > 1_048_576 { continue; }

            if let Ok(content) = std::fs::read_to_string(path) {
                let lines: Vec<&str> = content.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    if results.len() >= max { break; }
                    let matches = if case_sensitive {
                        line.contains(&search_query)
                    } else {
                        line.to_lowercase().contains(&search_query)
                    };
                    if matches {
                        let start = i.saturating_sub(context_lines);
                        let end = (i + context_lines + 1).min(lines.len());
                        let context: Vec<String> = lines[start..end].iter()
                            .enumerate()
                            .map(|(j, l)| format!("{:>4} {}{}", start + j + 1, if start + j == i { ">" } else { " " }, l))
                            .collect();
                        results.push(serde_json::json!({
                            "file": path.to_string_lossy(),
                            "line": i + 1,
                            "match": line.trim(),
                            "context": context.join("\n"),
                        }));
                    }
                }
            }
        }
        ToolResult::ok(serde_json::json!({ "matches": results, "count": results.len() }))
    }
}

struct FindFilesByPattern;
#[async_trait] impl ToolHandler for FindFilesByPattern {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let dir = params["directory"].as_str().unwrap_or(".");
        let pattern = params["pattern"].as_str().unwrap_or("*");
        let max = params["max_results"].as_u64().unwrap_or(100) as usize;
        let min_size = params["min_size"].as_u64();
        let max_size = params["max_size"].as_u64();
        let file_type = params["type"].as_str().unwrap_or("any"); // "file", "dir", "any"

        let glob = match globset::GlobBuilder::new(pattern).case_insensitive(true).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolResult::err(format!("invalid pattern: {e}")),
        };

        let mut results = Vec::new();
        for entry in walkdir::WalkDir::new(dir).max_depth(15).into_iter().filter_map(|e| e.ok()) {
            if results.len() >= max { break; }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Filter by type
            match file_type {
                "file" if !meta.is_file() => continue,
                "dir" if !meta.is_dir() => continue,
                _ => {}
            }
            // Filter by size
            if let Some(min) = min_size { if meta.len() < min { continue; } }
            if let Some(max_s) = max_size { if meta.len() > max_s { continue; } }

            if glob.is_match(entry.file_name().to_string_lossy().as_ref()) {
                let modified = meta.modified().ok().and_then(|t|
                    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
                results.push(serde_json::json!({
                    "path": entry.path().to_string_lossy(),
                    "size": meta.len(),
                    "is_dir": meta.is_dir(),
                    "modified_epoch": modified,
                }));
            }
        }
        ToolResult::ok(serde_json::json!({ "matches": results, "count": results.len() }))
    }
}

struct GetProjectStructure;
#[async_trait] impl ToolHandler for GetProjectStructure {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let dir = params["path"].as_str().unwrap_or(".");
        let max_depth = params["max_depth"].as_u64().unwrap_or(4) as usize;
        let show_hidden = params["show_hidden"].as_bool().unwrap_or(false);

        fn build_tree(path: &std::path::Path, depth: usize, max_depth: usize, show_hidden: bool) -> Vec<serde_json::Value> {
            if depth >= max_depth { return vec![]; }
            let mut entries: Vec<_> = match std::fs::read_dir(path) {
                Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
                Err(_) => return vec![],
            };
            entries.sort_by_key(|e| e.file_name());
            let mut tree = Vec::new();
            for entry in entries {
                let name = entry.file_name().to_string_lossy().to_string();
                if !show_hidden && name.starts_with('.') { continue; }
                // Skip common non-essential dirs
                if depth == 0 && ["node_modules", "target", ".git", "__pycache__", ".mypy_cache",
                    "dist", "build", ".next", ".venv", "venv"].contains(&name.as_str()) { continue; }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() {
                    let children = build_tree(&entry.path(), depth + 1, max_depth, show_hidden);
                    tree.push(serde_json::json!({
                        "name": name, "type": "dir", "children": children,
                    }));
                } else {
                    tree.push(serde_json::json!({
                        "name": name, "type": "file", "size": meta.len(),
                    }));
                }
            }
            tree
        }

        let tree = build_tree(std::path::Path::new(dir), 0, max_depth, show_hidden);
        ToolResult::ok(serde_json::json!({ "path": dir, "tree": tree }))
    }
}

struct CountLinesOfCode;
#[async_trait] impl ToolHandler for CountLinesOfCode {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let dir = params["directory"].as_str().unwrap_or(".");
        let mut by_lang: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new(); // lang -> (files, lines)
        let mut total_files = 0usize;
        let mut total_lines = 0usize;

        for entry in walkdir::WalkDir::new(dir).max_depth(15).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() { continue; }
            let path = entry.path();
            let lang = crate::preprocessing::code::CodeProcessor::detect_language(path);
            if lang == "unknown" { continue; }
            if let Ok(content) = std::fs::read_to_string(path) {
                let lines = content.lines().count();
                let entry = by_lang.entry(lang).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += lines;
                total_files += 1;
                total_lines += lines;
            }
        }

        let breakdown: Vec<serde_json::Value> = by_lang.iter()
            .map(|(lang, (files, lines))| serde_json::json!({
                "language": lang, "files": files, "lines": lines,
            }))
            .collect();

        ToolResult::ok(serde_json::json!({
            "directory": dir,
            "total_files": total_files,
            "total_lines": total_lines,
            "breakdown": breakdown,
        }))
    }
}

struct DiffFiles;
#[async_trait] impl ToolHandler for DiffFiles {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let file_a = params["file_a"].as_str().unwrap_or("");
        let file_b = params["file_b"].as_str().unwrap_or("");

        let content_a = match tokio::fs::read_to_string(file_a).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("read file_a failed: {e}")),
        };
        let content_b = match tokio::fs::read_to_string(file_b).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("read file_b failed: {e}")),
        };

        let lines_a: Vec<&str> = content_a.lines().collect();
        let lines_b: Vec<&str> = content_b.lines().collect();
        let mut diffs = Vec::new();
        let max_len = lines_a.len().max(lines_b.len());

        for i in 0..max_len {
            let a = lines_a.get(i).copied().unwrap_or("");
            let b = lines_b.get(i).copied().unwrap_or("");
            if a != b {
                diffs.push(serde_json::json!({
                    "line": i + 1,
                    "file_a": a,
                    "file_b": b,
                }));
            }
        }

        ToolResult::ok(serde_json::json!({
            "file_a": file_a,
            "file_b": file_b,
            "lines_a": lines_a.len(),
            "lines_b": lines_b.len(),
            "differences": diffs.len(),
            "diffs": if diffs.len() > 100 { diffs[..100].to_vec() } else { diffs },
        }))
    }
}

struct FindTodos;
#[async_trait] impl ToolHandler for FindTodos {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let dir = params["directory"].as_str().unwrap_or(".");
        let max = params["max_results"].as_u64().unwrap_or(50) as usize;
        let pattern = regex::Regex::new(r"(?i)\b(TODO|FIXME|HACK|XXX|BUG|OPTIMIZE|REFACTOR)\b[:\s]*(.*)").unwrap();

        let binary_exts = ["png", "jpg", "jpeg", "gif", "bmp", "ico", "woff", "woff2", "ttf", "otf",
            "mp3", "mp4", "zip", "tar", "gz", "rar", "exe", "dll", "so", "o", "a",
            "bin", "dat", "db", "sqlite", "gguf", "onnx", "pdf"];

        let mut results = Vec::new();
        for entry in walkdir::WalkDir::new(dir).max_depth(10).into_iter().filter_map(|e| e.ok()) {
            if results.len() >= max { break; }
            if !entry.file_type().is_file() { continue; }
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if binary_exts.contains(&ext.as_str()) { continue; }
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > 1_048_576 { continue; }

            if let Ok(content) = std::fs::read_to_string(path) {
                for (i, line) in content.lines().enumerate() {
                    if results.len() >= max { break; }
                    if let Some(cap) = pattern.captures(line) {
                        let tag = cap.get(1).map(|m| m.as_str()).unwrap_or("TODO");
                        let message = cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");
                        results.push(serde_json::json!({
                            "file": path.to_string_lossy(),
                            "line": i + 1,
                            "tag": tag.to_uppercase(),
                            "message": message,
                            "context": line.trim(),
                        }));
                    }
                }
            }
        }

        ToolResult::ok(serde_json::json!({
            "directory": dir,
            "count": results.len(),
            "items": results,
        }))
    }
}

struct AnalyzeCode;
#[async_trait] impl ToolHandler for AnalyzeCode {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let p = std::path::Path::new(path);
        match crate::preprocessing::code::CodeProcessor::analyze(p) {
            Ok(info) => ToolResult::ok(serde_json::json!({
                "path": path,
                "language": info.language,
                "line_count": info.line_count,
                "functions": info.functions,
                "imports": info.imports,
                "function_count": info.functions.len(),
                "import_count": info.imports.len(),
            })),
            Err(e) => ToolResult::err(format!("analyze failed: {e}")),
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
            name: "delete_file".into(), description: "Delete a file permanently".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![param("path", "string", "File path", true)],
        }, Arc::new(DeleteFile)),
        (ToolDef {
            name: "delete_directory".into(), description: "Delete a directory and all its contents".into(),
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
        // Phase 3: Enhanced file search
        (ToolDef {
            name: "search_file_contents".into(), description: "Search inside files for text (grep-like) with context lines".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("directory", "string", "Starting directory", true),
                param("query", "string", "Text to search for", true),
                param("max_results", "integer", "Max matches (default 20)", false),
                param("context_lines", "integer", "Context lines before/after (default 2)", false),
                param("case_sensitive", "boolean", "Case-sensitive search (default false)", false),
            ],
        }, Arc::new(SearchFileContents)),
        (ToolDef {
            name: "find_files_by_pattern".into(), description: "Find files/dirs by glob pattern with size/date/type filters".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("directory", "string", "Starting directory", true),
                param("pattern", "string", "Glob pattern (e.g. *.rs, *.py)", true),
                param("max_results", "integer", "Max results (default 100)", false),
                param("min_size", "integer", "Minimum file size in bytes", false),
                param("max_size", "integer", "Maximum file size in bytes", false),
                param("type", "string", "Filter: file, dir, or any (default any)", false),
            ],
        }, Arc::new(FindFilesByPattern)),
        (ToolDef {
            name: "get_project_structure".into(), description: "Get a tree-like directory structure for a project".into(),
            category: "file_ops".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("path", "string", "Project root directory", true),
                param("max_depth", "integer", "Max depth to traverse (default 4)", false),
                param("show_hidden", "boolean", "Include hidden files/dirs (default false)", false),
            ],
        }, Arc::new(GetProjectStructure)),
        // Phase 3: Code intelligence
        (ToolDef {
            name: "count_lines_of_code".into(), description: "Count lines of code by language in a directory".into(),
            category: "code_intelligence".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("directory", "string", "Directory to analyze", true),
            ],
        }, Arc::new(CountLinesOfCode)),
        (ToolDef {
            name: "diff_files".into(), description: "Compare two files and show line-by-line differences".into(),
            category: "code_intelligence".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("file_a", "string", "First file path", true),
                param("file_b", "string", "Second file path", true),
            ],
        }, Arc::new(DiffFiles)),
        (ToolDef {
            name: "find_todos".into(), description: "Scan codebase for TODO/FIXME/HACK/BUG comments".into(),
            category: "code_intelligence".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("directory", "string", "Directory to scan", true),
                param("max_results", "integer", "Max results (default 50)", false),
            ],
        }, Arc::new(FindTodos)),
        (ToolDef {
            name: "analyze_code".into(), description: "Analyze a source file: detect language, extract functions and imports".into(),
            category: "code_intelligence".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("path", "string", "Source file path", true),
            ],
        }, Arc::new(AnalyzeCode)),
    ];

    for (def, handler) in tools {
        reg.register(def, handler);
    }
}

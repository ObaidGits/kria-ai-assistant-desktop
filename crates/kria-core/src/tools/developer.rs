use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

// Helper to run git commands
async fn run_git(args: &[&str], cwd: Option<&str>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd.output().await.map_err(|e| format!("git not available: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git error: {}", stderr.trim()))
    }
}

// ── Git Tools ──

struct GitStatus;
#[async_trait] impl ToolHandler for GitStatus {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        match run_git(&["status", "--porcelain", "-b"], Some(path)).await {
            Ok(output) => {
                let lines: Vec<&str> = output.lines().collect();
                let branch = lines.first()
                    .map(|l| l.trim_start_matches("## ").to_string())
                    .unwrap_or_default();
                let changes: Vec<serde_json::Value> = lines.iter().skip(1)
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let status = l.chars().take(2).collect::<String>();
                        let file = l[3..].trim().to_string();
                        serde_json::json!({ "status": status.trim(), "file": file })
                    })
                    .collect();
                ToolResult::ok(serde_json::json!({
                    "branch": branch,
                    "changes": changes,
                    "clean": changes.is_empty(),
                }))
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

struct GitLog;
#[async_trait] impl ToolHandler for GitLog {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let count = params["count"].as_u64().unwrap_or(10);
        match run_git(&["log", &format!("-{count}"), "--oneline", "--decorate"], Some(path)).await {
            Ok(output) => {
                let commits: Vec<serde_json::Value> = output.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let parts: Vec<&str> = l.splitn(2, ' ').collect();
                        serde_json::json!({
                            "hash": parts.first().unwrap_or(&""),
                            "message": parts.get(1).unwrap_or(&""),
                        })
                    })
                    .collect();
                ToolResult::ok(serde_json::json!({ "commits": commits }))
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

struct GitDiff;
#[async_trait] impl ToolHandler for GitDiff {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let file = params["file"].as_str();
        let mut args = vec!["diff", "--stat"];
        if let Some(f) = file { args.push(f); }
        match run_git(&args, Some(path)).await {
            Ok(stat) => {
                // Also get full diff if not too large
                let mut full_args = vec!["diff"];
                if let Some(f) = file { full_args.push(f); }
                let full = run_git(&full_args, Some(path)).await.unwrap_or_default();
                let max = 5000;
                let diff_text = if full.len() > max {
                    format!("{}...\n[truncated, {} more chars]", &full[..max], full.len() - max)
                } else {
                    full
                };
                ToolResult::ok(serde_json::json!({
                    "stat": stat.trim(),
                    "diff": diff_text,
                }))
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

struct GitCommit;
#[async_trait] impl ToolHandler for GitCommit {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let message = params["message"].as_str().unwrap_or("Update");
        let all = params["all"].as_bool().unwrap_or(true);

        if all {
            if let Err(e) = run_git(&["add", "-A"], Some(path)).await {
                return ToolResult::err(format!("git add failed: {e}"));
            }
        }
        match run_git(&["commit", "-m", message], Some(path)).await {
            Ok(output) => ToolResult::ok(serde_json::json!({ "committed": true, "output": output.trim() })),
            Err(e) => ToolResult::err(e),
        }
    }
}

struct GitBranchList;
#[async_trait] impl ToolHandler for GitBranchList {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        match run_git(&["branch", "-a", "--no-color"], Some(path)).await {
            Ok(output) => {
                let branches: Vec<serde_json::Value> = output.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let current = l.starts_with('*');
                        let name = l.trim_start_matches('*').trim().to_string();
                        serde_json::json!({ "name": name, "current": current })
                    })
                    .collect();
                ToolResult::ok(serde_json::json!({ "branches": branches }))
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

struct GitCheckout;
#[async_trait] impl ToolHandler for GitCheckout {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let branch = params["branch"].as_str().unwrap_or("main");
        let create = params["create"].as_bool().unwrap_or(false);
        let mut args = vec!["checkout"];
        if create { args.push("-b"); }
        args.push(branch);
        match run_git(&args, Some(path)).await {
            Ok(output) => ToolResult::ok(serde_json::json!({ "branch": branch, "output": output.trim() })),
            Err(e) => ToolResult::err(e),
        }
    }
}

struct GitStash;
#[async_trait] impl ToolHandler for GitStash {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let pop = params["pop"].as_bool().unwrap_or(false);
        let args = if pop { vec!["stash", "pop"] } else { vec!["stash"] };
        match run_git(&args, Some(path)).await {
            Ok(output) => ToolResult::ok(serde_json::json!({ "action": if pop {"pop"} else {"stash"}, "output": output.trim() })),
            Err(e) => ToolResult::err(e),
        }
    }
}

// ── Project Analysis ──

struct AnalyzeProject;
#[async_trait] impl ToolHandler for AnalyzeProject {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or(".");
        let root = std::path::Path::new(path);
        if !root.exists() {
            return ToolResult::err(format!("path not found: {path}"));
        }

        let mut languages: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut total_files = 0usize;
        let mut total_lines = 0usize;
        let mut framework = String::new();
        let mut build_system = String::new();

        // Detect build system and framework from root files
        let markers = [
            ("Cargo.toml", "Rust", "Cargo"),
            ("package.json", "JavaScript/TypeScript", "npm"),
            ("pyproject.toml", "Python", "uv/pip"),
            ("go.mod", "Go", "go modules"),
            ("pom.xml", "Java", "Maven"),
            ("build.gradle", "Java/Kotlin", "Gradle"),
            ("CMakeLists.txt", "C/C++", "CMake"),
            ("Makefile", "Various", "Make"),
        ];
        for (file, lang, bs) in &markers {
            if root.join(file).exists() {
                if framework.is_empty() { framework = lang.to_string(); }
                if build_system.is_empty() { build_system = bs.to_string(); }
            }
        }

        // Walk directory and count files by extension
        if let Ok(entries) = walkdir::WalkDir::new(root)
            .max_depth(5)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
        {
            for entry in entries {
                if entry.file_type().is_file() {
                    total_files += 1;
                    if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                        let lang = match ext {
                            "rs" => "Rust",
                            "py" => "Python",
                            "js" => "JavaScript",
                            "ts" | "tsx" => "TypeScript",
                            "java" => "Java",
                            "go" => "Go",
                            "c" | "h" => "C",
                            "cpp" | "hpp" | "cc" => "C++",
                            "rb" => "Ruby",
                            "php" => "PHP",
                            "swift" => "Swift",
                            "kt" => "Kotlin",
                            "cs" => "C#",
                            "html" => "HTML",
                            "css" | "scss" | "less" => "CSS",
                            "json" => "JSON",
                            "toml" | "yaml" | "yml" => "Config",
                            "md" | "txt" | "rst" => "Docs",
                            "sh" | "bash" => "Shell",
                            "sql" => "SQL",
                            _ => "Other",
                        };
                        *languages.entry(lang.to_string()).or_insert(0) += 1;
                    }
                    // Count lines (quick estimate)
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        total_lines += content.lines().count();
                    }
                }
            }
        }

        // Sort languages by count
        let mut lang_vec: Vec<(String, usize)> = languages.into_iter().collect();
        lang_vec.sort_by(|a, b| b.1.cmp(&a.1));
        let lang_summary: Vec<serde_json::Value> = lang_vec.iter()
            .take(10)
            .map(|(l, c)| serde_json::json!({ "language": l, "files": c }))
            .collect();

        // Check for common framework indicators
        if root.join("tauri.conf.json").exists() { framework = "Tauri + Rust".into(); }
        else if root.join("next.config.js").exists() || root.join("next.config.ts").exists() { framework = "Next.js".into(); }
        else if root.join("vite.config.ts").exists() || root.join("vite.config.js").exists() { framework = format!("{} + Vite", framework); }
        else if root.join("django").exists() || root.join("manage.py").exists() { framework = "Django".into(); }
        else if root.join("Gemfile").exists() { framework = "Ruby on Rails".into(); }

        // Git info
        let git_branch = run_git(&["branch", "--show-current"], Some(path)).await.ok()
            .map(|s| s.trim().to_string());

        ToolResult::ok(serde_json::json!({
            "path": path,
            "primary_language": lang_vec.first().map(|(l, _)| l.as_str()).unwrap_or("Unknown"),
            "framework": if framework.is_empty() { "Unknown" } else { &framework },
            "build_system": if build_system.is_empty() { "Unknown" } else { &build_system },
            "total_files": total_files,
            "total_lines": total_lines,
            "languages": lang_summary,
            "git_branch": git_branch,
        }))
    }
}

// ── Diff Tools ──

struct DiffFiles;
#[async_trait] impl ToolHandler for DiffFiles {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let file_a = params["file_a"].as_str().unwrap_or("");
        let file_b = params["file_b"].as_str().unwrap_or("");
        if file_a.is_empty() || file_b.is_empty() {
            return ToolResult::err("both file_a and file_b are required");
        }
        let output = tokio::process::Command::new("diff")
            .args(["-u", file_a, file_b])
            .output().await;
        match output {
            Ok(o) => {
                let diff = String::from_utf8_lossy(&o.stdout);
                let max = 5000;
                ToolResult::ok(serde_json::json!({
                    "diff": if diff.len() > max { &diff[..max] } else { &diff },
                    "identical": o.status.code() == Some(0),
                    "truncated": diff.len() > max,
                }))
            }
            Err(e) => ToolResult::err(format!("diff failed: {e}"))
        }
    }
}

// ── Database Tools ──

struct QuerySqlite;
#[async_trait] impl ToolHandler for QuerySqlite {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let db_path = params["db_path"].as_str().unwrap_or("");
        let sql = params["sql"].as_str().unwrap_or("");
        if db_path.is_empty() || sql.is_empty() {
            return ToolResult::err("db_path and sql are required");
        }
        // Safety: only allow read-only queries by default
        let sql_lower = sql.to_lowercase().trim().to_string();
        if !sql_lower.starts_with("select") && !sql_lower.starts_with("pragma") && !sql_lower.starts_with("explain") {
            return ToolResult::err("only SELECT, PRAGMA, and EXPLAIN queries are allowed (read-only)");
        }
        match rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => {
                match conn.prepare(sql) {
                    Ok(mut stmt) => {
                        let col_count = stmt.column_count();
                        let col_names: Vec<String> = (0..col_count)
                            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
                            .collect();
                        let mut rows = Vec::new();
                        let mut query_rows = stmt.query([]).map_err(|e| e.to_string()).unwrap();
                        let max_rows = 100;
                        let mut count = 0;
                        while let Ok(Some(row)) = query_rows.next() {
                            if count >= max_rows { break; }
                            let mut obj = serde_json::Map::new();
                            for (i, name) in col_names.iter().enumerate() {
                                let val: serde_json::Value = row.get::<_, rusqlite::types::Value>(i)
                                    .map(|v| match v {
                                        rusqlite::types::Value::Null => serde_json::Value::Null,
                                        rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                                        rusqlite::types::Value::Real(f) => serde_json::json!(f),
                                        rusqlite::types::Value::Text(s) => serde_json::json!(s),
                                        rusqlite::types::Value::Blob(b) => serde_json::json!(format!("<blob {} bytes>", b.len())),
                                    })
                                    .unwrap_or(serde_json::Value::Null);
                                obj.insert(name.clone(), val);
                            }
                            rows.push(serde_json::Value::Object(obj));
                            count += 1;
                        }
                        ToolResult::ok(serde_json::json!({
                            "columns": col_names,
                            "rows": rows,
                            "row_count": count,
                            "truncated": count >= max_rows,
                        }))
                    }
                    Err(e) => ToolResult::err(format!("SQL error: {e}"))
                }
            }
            Err(e) => ToolResult::err(format!("failed to open database: {e}"))
        }
    }
}

struct DescribeDatabase;
#[async_trait] impl ToolHandler for DescribeDatabase {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let db_path = params["db_path"].as_str().unwrap_or("");
        if db_path.is_empty() {
            return ToolResult::err("db_path is required");
        }
        match rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => {
                let mut stmt = conn.prepare(
                    "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name"
                ).map_err(|e| format!("SQL error: {e}")).unwrap();
                let tables: Vec<serde_json::Value> = stmt.query_map([], |row| {
                    Ok(serde_json::json!({
                        "name": row.get::<_, String>(0)?,
                        "type": row.get::<_, String>(1)?,
                    }))
                }).map_err(|e| format!("query error: {e}")).unwrap()
                    .filter_map(|r| r.ok())
                    .collect();
                ToolResult::ok(serde_json::json!({
                    "db_path": db_path,
                    "tables": tables,
                    "table_count": tables.len(),
                }))
            }
            Err(e) => ToolResult::err(format!("failed to open database: {e}"))
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // Git tools
        (ToolDef {
            name: "git_status".into(), description: "Show git status: current branch and changed files".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "Repository path (default: current dir)", false)],
        }, Arc::new(GitStatus)),
        (ToolDef {
            name: "git_log".into(), description: "Show recent git commit history".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Repository path", false),
                param("count", "integer", "Number of commits to show (default: 10)", false),
            ],
        }, Arc::new(GitLog)),
        (ToolDef {
            name: "git_diff".into(), description: "Show git diff for working directory or a specific file".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Repository path", false),
                param("file", "string", "Specific file to diff (optional)", false),
            ],
        }, Arc::new(GitDiff)),
        (ToolDef {
            name: "git_commit".into(), description: "Stage all changes and commit with a message".into(),
            category: "developer".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Repository path", false),
                param("message", "string", "Commit message", true),
                param("all", "boolean", "Stage all changes before committing (default: true)", false),
            ],
        }, Arc::new(GitCommit)),
        (ToolDef {
            name: "git_branch_list".into(), description: "List all local and remote branches".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "Repository path", false)],
        }, Arc::new(GitBranchList)),
        (ToolDef {
            name: "git_checkout".into(), description: "Switch to a branch or create a new branch".into(),
            category: "developer".into(), default_tier: RiskLevel::Yellow, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Repository path", false),
                param("branch", "string", "Branch name", true),
                param("create", "boolean", "Create new branch (-b)", false),
            ],
        }, Arc::new(GitCheckout)),
        (ToolDef {
            name: "git_stash".into(), description: "Stash or pop stashed changes".into(),
            category: "developer".into(), default_tier: RiskLevel::Yellow, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Repository path", false),
                param("pop", "boolean", "Pop the stash instead of creating one", false),
            ],
        }, Arc::new(GitStash)),
        // Project analysis
        (ToolDef {
            name: "analyze_project".into(), description: "Analyze a project: detect languages, framework, file counts, and structure".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "Project root path", true)],
        }, Arc::new(AnalyzeProject)),
        // Diff tools
        (ToolDef {
            name: "diff_files_unified".into(), description: "Show unified diff between two files using system diff".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("file_a", "string", "First file path", true),
                param("file_b", "string", "Second file path", true),
            ],
        }, Arc::new(DiffFiles)),
        // Database tools
        (ToolDef {
            name: "query_sqlite".into(), description: "Run a read-only SQL query on a SQLite database".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("db_path", "string", "Path to the SQLite database file", true),
                param("sql", "string", "SQL query (SELECT/PRAGMA/EXPLAIN only)", true),
            ],
        }, Arc::new(QuerySqlite)),
        (ToolDef {
            name: "describe_database".into(), description: "List all tables and views in a SQLite database".into(),
            category: "developer".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("db_path", "string", "Path to the SQLite database file", true)],
        }, Arc::new(DescribeDatabase)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}

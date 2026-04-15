use std::path::Path;

/// Code file preprocessing: language detection, structure extraction.
pub struct CodeProcessor;

#[derive(Debug, Clone)]
pub struct CodeInfo {
    pub language: String,
    pub line_count: usize,
    pub functions: Vec<String>,
    pub imports: Vec<String>,
}

impl CodeProcessor {
    /// Detect programming language from file extension.
    pub fn detect_language(path: &Path) -> String {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "rs" => "rust",
            "py" => "python",
            "js" | "mjs" => "javascript",
            "ts" | "mts" => "typescript",
            "tsx" | "jsx" => "react",
            "go" => "go",
            "java" => "java",
            "c" | "h" => "c",
            "cpp" | "cc" | "cxx" | "hpp" => "cpp",
            "cs" => "csharp",
            "rb" => "ruby",
            "php" => "php",
            "swift" => "swift",
            "kt" | "kts" => "kotlin",
            "sh" | "bash" => "shell",
            "sql" => "sql",
            "html" | "htm" => "html",
            "css" | "scss" | "sass" => "css",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            "json" => "json",
            "xml" => "xml",
            "md" | "markdown" => "markdown",
            _ => "unknown",
        }.to_string()
    }

    /// Extract basic structure info from a source file.
    pub fn analyze(path: &Path) -> anyhow::Result<CodeInfo> {
        let content = std::fs::read_to_string(path)?;
        let lang = Self::detect_language(path);
        let line_count = content.lines().count();

        let fn_pattern = match lang.as_str() {
            "rust" => r"(?m)^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)",
            "python" => r"(?m)^\s*def\s+(\w+)",
            "javascript" | "typescript" | "react" => r"(?m)(?:function\s+(\w+)|(?:const|let)\s+(\w+)\s*=\s*(?:async\s+)?\()",
            "go" => r"(?m)^func\s+(\w+)",
            _ => r"$^", // match nothing
        };

        let import_pattern = match lang.as_str() {
            "rust" => r"(?m)^use\s+(.+);",
            "python" => r"(?m)^(?:from\s+\S+\s+)?import\s+(.+)",
            "javascript" | "typescript" | "react" => r#"(?m)^import\s+.+from\s+['"](.+?)['"]"#,
            "go" => r#"(?m)^\s+"(.+)""#,
            _ => r"$^",
        };

        let fn_re = regex::Regex::new(fn_pattern)?;
        let import_re = regex::Regex::new(import_pattern)?;

        let functions: Vec<String> = fn_re.captures_iter(&content)
            .filter_map(|cap| {
                cap.get(1).or(cap.get(2)).map(|m| m.as_str().to_string())
            })
            .collect();

        let imports: Vec<String> = import_re.captures_iter(&content)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
            .collect();

        Ok(CodeInfo {
            language: lang,
            line_count,
            functions,
            imports,
        })
    }
}

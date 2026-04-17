use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct CleanTempFiles;
#[async_trait] impl ToolHandler for CleanTempFiles {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let older_than_days = params["older_than_days"].as_u64().unwrap_or(7);
        let temp_dir = std::env::temp_dir();
        let threshold = std::time::SystemTime::now() -
            std::time::Duration::from_secs(older_than_days * 86400);

        let mut deleted = 0u64;
        let mut freed_bytes = 0u64;

        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if modified < threshold {
                            freed_bytes += meta.len();
                            if meta.is_dir() {
                                let _ = std::fs::remove_dir_all(entry.path());
                            } else {
                                let _ = std::fs::remove_file(entry.path());
                            }
                            deleted += 1;
                        }
                    }
                }
            }
        }

        ToolResult::ok(serde_json::json!({
            "temp_dir": temp_dir.to_string_lossy(),
            "files_deleted": deleted,
            "freed_mb": freed_bytes / (1024 * 1024),
            "older_than_days": older_than_days,
        }))
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // RED
        (ToolDef {
            name: "clean_temp_files".into(), description: "Delete old temporary files".into(),
            category: "disk".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![param("older_than_days", "integer", "Only delete files older than N days (default 7)", false)],
        }, Arc::new(CleanTempFiles)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}

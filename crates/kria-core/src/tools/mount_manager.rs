//! Tool Mount Manager — controls which tools are visible to the LLM each turn.
//!
//! Tools are organized into "mount groups":
//! - `ambient`: always mounted (~8 high-frequency tools like Gmail inbox, Calendar today, Drive search)
//! - `docs`: on-demand (~12 Docs/Sheets/Slides manipulation tools)
//! - `admin`: on-demand (~6 send/delete/share tools requiring higher trust)
//!
//! Tools without a mount group are always visible (the default for all existing tools).

use std::collections::{HashMap, HashSet};

/// Manages which tool groups are currently active (mounted).
pub struct ToolMountManager {
    /// Groups and their member tool names.
    groups: HashMap<String, Vec<String>>,
    /// Currently active (mounted) groups.
    active: HashSet<String>,
    /// Groups that are always active.
    always_on: HashSet<String>,
}

impl ToolMountManager {
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
            active: HashSet::new(),
            always_on: HashSet::new(),
        }
    }

    /// Define a mount group with its tool names.
    /// If `always_on` is true, this group is mounted at startup and cannot be unmounted.
    pub fn define_group(&mut self, name: &str, tools: Vec<String>, always_on: bool) {
        self.groups.insert(name.to_string(), tools);
        if always_on {
            self.always_on.insert(name.to_string());
            self.active.insert(name.to_string());
        }
    }

    /// Mount a group (make its tools visible to the LLM).
    pub fn mount(&mut self, group: &str) -> bool {
        if self.groups.contains_key(group) {
            self.active.insert(group.to_string());
            tracing::info!(group = group, "tool group mounted");
            true
        } else {
            false
        }
    }

    /// Unmount a group (hide its tools from the LLM).
    /// Always-on groups cannot be unmounted.
    pub fn unmount(&mut self, group: &str) -> bool {
        if self.always_on.contains(group) {
            tracing::warn!(group = group, "cannot unmount always-on group");
            return false;
        }
        let removed = self.active.remove(group);
        if removed {
            tracing::info!(group = group, "tool group unmounted");
        }
        removed
    }

    /// Check if a specific tool is currently mounted.
    /// Tools not in any group are always considered mounted.
    pub fn is_mounted(&self, tool_name: &str) -> bool {
        // Check if tool belongs to any group
        for (group_name, tools) in &self.groups {
            if tools.iter().any(|t| t == tool_name) {
                return self.active.contains(group_name);
            }
        }
        // Tool not in any group → always mounted
        true
    }

    /// Get all currently mounted tool names from managed groups.
    pub fn mounted_tools(&self) -> HashSet<String> {
        let mut result = HashSet::new();
        for group in &self.active {
            if let Some(tools) = self.groups.get(group) {
                for t in tools {
                    result.insert(t.clone());
                }
            }
        }
        result
    }

    /// Get the list of active group names.
    pub fn active_groups(&self) -> Vec<String> {
        self.active.iter().cloned().collect()
    }

    /// Get all defined group names.
    pub fn all_groups(&self) -> Vec<String> {
        self.groups.keys().cloned().collect()
    }

    /// Keyword heuristic: scan user message and auto-mount relevant groups.
    /// Returns list of newly mounted groups.
    pub fn auto_mount_from_message(&mut self, message: &str) -> Vec<String> {
        let lower = message.to_lowercase();
        let mut newly_mounted = Vec::new();

        // Keyword → group mapping
        let heuristics: &[(&[&str], &str)] = &[
            (&["email", "gmail", "inbox", "mail", "send email", "draft"], "gworkspace_ambient"),
            (&["calendar", "schedule", "meeting", "event", "appointment"], "gworkspace_ambient"),
            (&["drive", "google drive", "shared drive"], "gworkspace_ambient"),
            (&["document", "google doc", "gdoc", "write doc", "edit doc"], "gworkspace_docs"),
            (&["spreadsheet", "google sheet", "gsheet", "excel"], "gworkspace_docs"),
            (&["slides", "presentation", "google slides", "gslides"], "gworkspace_docs"),
            (&["share file", "share document", "send mail", "delete email", "delete file"], "gworkspace_admin"),
        ];

        for (keywords, group) in heuristics {
            if !self.active.contains(*group) && self.groups.contains_key(*group) {
                if keywords.iter().any(|kw| lower.contains(kw)) {
                    self.active.insert(group.to_string());
                    newly_mounted.push(group.to_string());
                    tracing::info!(group = group, "auto-mounted tool group from message keywords");
                }
            }
        }

        newly_mounted
    }

    /// Check if a tool name belongs to any defined group (managed tool).
    pub fn is_managed(&self, tool_name: &str) -> bool {
        self.groups.values().any(|tools| tools.iter().any(|t| t == tool_name))
    }
}

impl Default for ToolMountManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the default mount manager with Google Workspace groups pre-defined.
pub fn build_default_mount_manager() -> ToolMountManager {
    let mut mm = ToolMountManager::new();

    // Ambient group: always on — lightweight read/search tools
    mm.define_group("gworkspace_ambient", vec![
        "gw_gmail_inbox".into(),
        "gw_gmail_search".into(),
        "gw_gmail_read".into(),
        "gw_calendar_today".into(),
        "gw_calendar_search".into(),
        "gw_drive_search".into(),
        "gw_drive_list".into(),
        "gw_drive_read".into(),
    ], true);

    // Docs group: on-demand — document manipulation
    mm.define_group("gworkspace_docs", vec![
        "gw_docs_read".into(),
        "gw_docs_create".into(),
        "gw_docs_edit".into(),
        "gw_sheets_read".into(),
        "gw_sheets_create".into(),
        "gw_sheets_edit".into(),
        "gw_slides_read".into(),
        "gw_slides_create".into(),
    ], false);

    // Admin group: on-demand — destructive/send operations
    mm.define_group("gworkspace_admin", vec![
        "gw_gmail_send".into(),
        "gw_gmail_delete".into(),
        "gw_drive_delete".into(),
        "gw_calendar_create".into(),
        "gw_calendar_delete".into(),
    ], false);

    mm
}

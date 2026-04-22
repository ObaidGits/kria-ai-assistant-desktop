//! Tool Mount Manager — controls which tools are visible to the LLM each turn.
//!
//! Tools are organized into "mount groups":
//! - `ambient`: always mounted (~8 high-frequency tools like Gmail inbox, Calendar today, Drive search)
//! - `docs`: on-demand (~12 Docs/Sheets/Slides manipulation tools)
//! - `admin`: on-demand (~6 send/delete/share tools requiring higher trust)
//!
//! Tools without a mount group are always visible (the default for all existing tools).

use std::collections::{HashMap, HashSet};

const GOOGLE_MEET_FALLBACK_MODE: &str = "calendar_conference_link";

const GOOGLE_MEET_KEYWORDS: &[&str] = &[
    "google meet",
    "gmeet",
    "meet link",
    "meeting link",
    "video call",
    "conference link",
    "meet invite",
];

fn has_any_keyword(haystack: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|kw| haystack.contains(kw))
}

fn looks_like_calendar_write_request(lower: &str) -> bool {
    let has_calendar_context = [
        "calendar",
        "event",
        "meeting",
        "appointment",
        "invite",
        "google meet",
        "gmeet",
        "meet link",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    let has_write_intent = ["schedule", "create", "book", "add", "plan"]
        .iter()
        .any(|kw| lower.contains(kw));

    has_calendar_context && has_write_intent
}

/// Detect requests that should use Google Meet fallback via Calendar.
pub fn google_meet_fallback_metadata(message: &str) -> Option<serde_json::Value> {
    let lower = message.to_lowercase();
    if !has_any_keyword(&lower, GOOGLE_MEET_KEYWORDS) {
        return None;
    }

    Some(serde_json::json!({
        "type": "google_meet_fallback",
        "meet_support_mode": GOOGLE_MEET_FALLBACK_MODE,
        "primary_tool": "gw_calendar_create",
        "secondary_tool": "gw_calendar_search",
        "notes": [
            "No direct Google Meet API tool is exposed.",
            "Create meeting links through Calendar conference-link mode.",
            "If scheduling details are incomplete, ask a follow-up question before creating the event."
        ]
    }))
}

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
        // A tool can belong to multiple groups. Treat it as mounted if ANY of
        // those groups are currently active.
        let mut managed = false;
        for (group_name, tools) in &self.groups {
            if tools.iter().any(|t| t == tool_name) {
                managed = true;
                if self.active.contains(group_name) {
                    return true;
                }
            }
        }
        if managed {
            return false;
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
            (
                &["email", "gmail", "inbox", "mail", "send email", "draft"],
                "gworkspace_ambient",
            ),
            (GOOGLE_MEET_KEYWORDS, "gworkspace_meet_fallback"),
            (
                &["calendar", "schedule", "meeting", "event", "appointment"],
                "gworkspace_ambient",
            ),
            (
                &["drive", "google drive", "shared drive"],
                "gworkspace_ambient",
            ),
            (
                &["document", "google doc", "gdoc", "write doc", "edit doc"],
                "gworkspace_docs",
            ),
            (
                &["spreadsheet", "google sheet", "gsheet", "excel"],
                "gworkspace_docs",
            ),
            (
                &["slides", "presentation", "google slides", "gslides"],
                "gworkspace_docs",
            ),
            (
                &["form", "forms", "google form", "google forms"],
                "gworkspace_docs",
            ),
            (
                &[
                    "share file",
                    "share document",
                    "send mail",
                    "send email",
                    "send gmail",
                    "delete email",
                    "delete mail",
                    "delete gmail",
                    "delete file",
                    "delete document",
                    "delete doc",
                    "delete spreadsheet",
                    "delete sheet",
                    "delete presentation",
                    "delete slide",
                    "delete meeting",
                    "cancel meeting",
                    "delete event",
                ],
                "gworkspace_admin",
            ),
        ];

        for (keywords, group) in heuristics {
            if !self.active.contains(*group)
                && self.groups.contains_key(*group)
                && keywords.iter().any(|kw| lower.contains(kw))
            {
                self.active.insert(group.to_string());
                newly_mounted.push(group.to_string());
                tracing::info!(
                    group = group,
                    "auto-mounted tool group from message keywords"
                );
            }
        }

        // Calendar write operations (create/schedule) should expose the
        // create-event tool without mounting the full admin set.
        if !self.active.contains("gworkspace_calendar_write")
            && self.groups.contains_key("gworkspace_calendar_write")
            && looks_like_calendar_write_request(&lower)
        {
            self.active.insert("gworkspace_calendar_write".to_string());
            newly_mounted.push("gworkspace_calendar_write".to_string());
            tracing::info!(
                group = "gworkspace_calendar_write",
                "auto-mounted calendar write group from scheduling intent"
            );
        }

        newly_mounted
    }

    /// Check if a tool name belongs to any defined group (managed tool).
    pub fn is_managed(&self, tool_name: &str) -> bool {
        self.groups
            .values()
            .any(|tools| tools.iter().any(|t| t == tool_name))
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
    mm.define_group(
        "gworkspace_ambient",
        vec![
            "gw_gmail_inbox".into(),
            "gw_gmail_search".into(),
            "gw_gmail_read".into(),
            "gw_calendar_today".into(),
            "gw_calendar_search".into(),
            "gw_drive_search".into(),
            "gw_drive_list".into(),
            "gw_drive_read".into(),
        ],
        true,
    );

    // Docs group: on-demand — document manipulation
    mm.define_group(
        "gworkspace_docs",
        vec![
            "gw_docs_read".into(),
            "gw_docs_create".into(),
            "gw_docs_edit".into(),
            "gw_sheets_read".into(),
            "gw_sheets_create".into(),
            "gw_sheets_edit".into(),
            "gw_slides_read".into(),
            "gw_slides_create".into(),
            "gw_forms_list".into(),
            "gw_forms_create".into(),
        ],
        false,
    );

    // Admin group: on-demand — destructive/send operations
    mm.define_group(
        "gworkspace_admin",
        vec![
            "gw_gmail_send".into(),
            "gw_gmail_delete".into(),
            "gw_drive_delete".into(),
            "gw_calendar_create".into(),
            "gw_calendar_delete".into(),
        ],
        false,
    );

    // Meet fallback group: enable calendar scheduling tools for Meet-link requests.
    mm.define_group(
        "gworkspace_meet_fallback",
        vec!["gw_calendar_search".into(), "gw_calendar_create".into()],
        false,
    );

    // Calendar write group: expose calendar create for schedule/create intents
    // without mounting the entire admin/destructive group.
    mm.define_group(
        "gworkspace_calendar_write",
        vec!["gw_calendar_create".into()],
        false,
    );

    mm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_google_meet_fallback_metadata() {
        let metadata =
            google_meet_fallback_metadata("Please create a Google Meet link for tomorrow")
                .expect("meet fallback metadata should be present");

        assert_eq!(
            metadata["meet_support_mode"],
            serde_json::json!("calendar_conference_link")
        );
        assert_eq!(
            metadata["primary_tool"],
            serde_json::json!("gw_calendar_create")
        );
    }

    #[test]
    fn auto_mounts_meet_fallback_group_for_video_call_requests() {
        let mut mm = build_default_mount_manager();
        let mounted = mm.auto_mount_from_message("Set up a video call and share a meet link");

        assert!(mounted.iter().any(|g| g == "gworkspace_meet_fallback"));
        assert!(mm
            .active_groups()
            .iter()
            .any(|g| g == "gworkspace_meet_fallback"));
    }

    #[test]
    fn auto_mounts_calendar_write_for_schedule_requests() {
        let mut mm = build_default_mount_manager();
        let mounted = mm.auto_mount_from_message("Schedule a calendar meeting for tomorrow");

        assert!(mounted.iter().any(|g| g == "gworkspace_calendar_write"));
        assert!(mm.is_mounted("gw_calendar_create"));
    }

    #[test]
    fn tool_shared_across_groups_is_mounted_when_any_group_active() {
        let mut mm = build_default_mount_manager();
        // gworkspace_meet_fallback includes gw_calendar_create.
        mm.mount("gworkspace_meet_fallback");

        assert!(mm.is_mounted("gw_calendar_create"));
    }
}

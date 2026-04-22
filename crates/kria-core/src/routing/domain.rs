//! Domain taxonomy — maps the 28+ tool categories to ~10 coarse domains.
//! Domains are the unit of routing; tools are selected inside a domain by the LLM.

use serde::{Deserialize, Serialize};

/// Coarse routing domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Domain {
    /// Pure conversation: explanations, jokes, greetings, OOD chat.
    Conversation,
    /// System status, CPU/RAM/GPU/battery/uptime/network read-only.
    SystemInfo,
    /// File read/write/delete/search/parse operations.
    FileOps,
    /// App lifecycle: open/close/focus/list running apps.
    AppLifecycle,
    /// Communications: email, calendar, contacts, Telegram, notifications.
    Comms,
    /// Google Workspace: Drive, Docs, Sheets, Slides, Forms.
    Workspace,
    /// Knowledge & memory: recall, snippets, RAG, web search, news.
    Knowledge,
    /// Power & hardware control: shutdown, reboot, volume, brightness, mute.
    Power,
    /// Vision: screenshot, image analysis, describe screen.
    Vision,
    /// Package/software management: install, uninstall, update.
    Packages,
    /// Shell/process/developer: run commands, git, lint, build.
    Developer,
    /// Planner/complex multi-step — fallback when multi-domain or ambiguous.
    Planner,
}

impl Domain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::SystemInfo => "system_info",
            Self::FileOps => "file_ops",
            Self::AppLifecycle => "app_lifecycle",
            Self::Comms => "comms",
            Self::Workspace => "workspace",
            Self::Knowledge => "knowledge",
            Self::Power => "power",
            Self::Vision => "vision",
            Self::Packages => "packages",
            Self::Developer => "developer",
            Self::Planner => "planner",
        }
    }

    /// All tool-bearing domains (excludes Conversation and Planner which
    /// do not have direct tool mappings in the semantic layer).
    pub fn tool_domains() -> &'static [Domain] {
        &[
            Self::SystemInfo,
            Self::FileOps,
            Self::AppLifecycle,
            Self::Comms,
            Self::Workspace,
            Self::Knowledge,
            Self::Power,
            Self::Vision,
            Self::Packages,
            Self::Developer,
        ]
    }

    /// Hand-authored anchor sentences stabilise centroids when a domain is sparse.
    /// Each anchor sentence captures the *intent* of the domain, not tool names.
    pub fn anchor_sentences(self) -> &'static [&'static str] {
        match self {
            Self::Conversation => &[
                "just chat and talk with me",
                "explain how something works",
                "tell me a story or a joke",
                "describe a concept in simple terms",
                "answer a general knowledge question",
                "ek chhoti si baat karo",
            ],
            Self::SystemInfo => &[
                "check system status and hardware information",
                "report CPU memory GPU battery usage",
                "show network connection and internet status",
                "how long has the system been running",
                "system ki info dikhao",
            ],
            Self::FileOps => &[
                "read write copy move delete rename files and folders",
                "search for a file or look inside documents",
                "parse a PDF spreadsheet or Word document",
                "create or modify a file on disk",
                "file dhundo ya padhao",
            ],
            Self::AppLifecycle => &[
                "open launch start or close an application",
                "focus a window or list running programs",
                "switch between open apps",
                "app band karo ya kholo",
            ],
            Self::Comms => &[
                "send receive read or draft an email",
                "schedule or check calendar events and meetings",
                "look up a contact or send a message",
                "set a reminder or notification",
                "email bhejo ya calendar check karo",
            ],
            Self::Workspace => &[
                "create or edit a Google Doc spreadsheet or presentation",
                "upload download or search Google Drive files",
                "share a document or manage permissions",
                "Google Drive mein kuch dhundho ya banao",
            ],
            Self::Knowledge => &[
                "search the web for information",
                "remember or recall a fact I told you",
                "get latest news or weather",
                "look up a stock price or currency rate",
                "internet pe search karo ya kuch yaad karo",
            ],
            Self::Power => &[
                "shutdown reboot sleep or lock the computer",
                "mute unmute adjust volume or brightness",
                "control screen or audio output",
                "system band karo ya volume badlo",
            ],
            Self::Vision => &[
                "take a screenshot or capture the screen",
                "describe or analyze what is on screen",
                "read text from an image",
                "screen ka screenshot lo ya describe karo",
            ],
            Self::Packages => &[
                "install uninstall or update a software package",
                "manage apt pip cargo npm packages",
                "software install karo ya hatao",
            ],
            Self::Developer => &[
                "run a shell command or terminal script",
                "git commit push pull or check status",
                "run tests lint or build the project",
                "check running processes or kill a process",
                "terminal mein kuch chalao ya git karo",
            ],
            Self::Planner => &[
                "plan and execute multiple steps in sequence",
                "complex multi-step task requiring tool coordination",
            ],
        }
    }
}

/// Maps a `ToolDef.category` string to a `Domain`.
pub fn category_to_domain(category: &str) -> Domain {
    // MCP tool categories are prefixed "mcp_<server_name>"
    let cat = category.to_lowercase();

    if cat.starts_with("mcp_gworkspace") || cat.contains("google_workspace") || cat.contains("workspace") {
        return Domain::Workspace;
    }
    if cat.starts_with("mcp_telegram") || cat.contains("telegram") {
        return Domain::Comms;
    }

    match cat.as_str() {
        "system_info" | "system_config" => Domain::SystemInfo,
        "file_ops" | "documents" | "disk" => Domain::FileOps,
        "app_lifecycle" => Domain::AppLifecycle,
        "communication" => Domain::Comms,
        "knowledge" | "rag" | "internet" | "news" | "i18n" | "precognitive" => Domain::Knowledge,
        "power" => Domain::Power,
        "vision" => Domain::Vision,
        "packages" => Domain::Packages,
        "shell" | "process" | "developer" | "mount_manager" => Domain::Developer,
        "scheduler" | "proactive" | "interaction" => Domain::Comms,
        _ if cat.starts_with("mcp_") => Domain::Knowledge, // unknown MCP servers: safe default
        _ => Domain::Knowledge,
    }
}

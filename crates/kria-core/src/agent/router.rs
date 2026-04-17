use regex::Regex;
use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Intent classification result.
#[derive(Debug, Clone)]
pub struct IntentResult {
    pub intent: Intent,
    pub tool_hint: Option<String>,
    pub category: Option<String>,
    pub confidence: f32,
}

/// High-level intent categories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Conversational — no tool needed, direct LLM response.
    Conversation,
    /// Direct tool call — a single tool mapped from user text.
    DirectTool(String),
    /// Complex task — requires planning + multi-step tool use.
    ComplexTask,
}

// ─── Conversation patterns (no tool needed) ───
static CONVERSATION_RE: Lazy<Vec<Regex>> = Lazy::new(|| {
    let patterns = [
        r"^(hi|hello|hey|good\s*(morning|afternoon|evening)|howdy|greetings)\b",
        r"^(who|what)\s+(are|is)\s+you",
        r"^(thank|thanks|thx)\b",
        r"^(bye|goodbye|see\s+you|goodnight)\b",
        r"^(tell\s+me\s+a\s+joke|joke\b)",
        r"^(how\s+are\s+you|what'?s\s+up)\b",
        r"^(explain|describe|what\s+is|what\s+are|define)\b",
        r"\?$",
    ];
    patterns.iter().filter_map(|p| Regex::new(&format!("(?i){p}")).ok()).collect()
});

// ─── Direct tool patterns (trigger specific tools) ───
static DIRECT_TOOL_RE: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    let mappings: Vec<(&str, &str)> = vec![
        // System info
        (r"(?i)\b(cpu|processor)\s*(usage|load|info)\b", "get_cpu_usage"),
        (r"(?i)\b(ram|memory)\s*(usage|info|status)\b", "get_memory_info"),
        (r"(?i)\b(disk|storage)\s*(space|usage|info)\b", "get_disk_space"),
        (r"(?i)\b(battery)\s*(status|level|info)\b", "get_battery_status"),
        (r"(?i)\b(gpu|graphics)\s*(info|status|usage)\b", "get_gpu_info"),
        (r"(?i)\b(uptime|how\s+long.*running)\b", "get_system_uptime"),
        (r"(?i)\b(network|internet)\s*(status|info|connection)\b", "get_network_status"),
        // App lifecycle
        (r"(?i)\b(open|launch|start|run)\s+(\w+)\b", "open_application"),
        (r"(?i)\b(close|quit|exit)\s+(\w+)\b", "close_application"),
        (r"(?i)\b(running|active)\s*(apps|applications|processes)\b", "list_running_apps"),
        (r"(?i)\b(kill|terminate)\s*(process|pid)\b", "kill_process"),
        // File ops
        (r"(?i)\b(read|show|cat|display)\s+(the\s+)?file\b", "read_file"),
        (r"(?i)\b(list|ls|dir)\s+(the\s+)?(directory|folder|files)\b", "list_directory"),
        (r"(?i)\b(search|find)\s+(for\s+)?files?\b", "search_files"),
        (r"(?i)\b(write|create|save)\s+(a\s+)?file\b", "write_file"),
        (r"(?i)\b(delete|remove|rm)\s+(the\s+)?file\b", "delete_file"),
        // Clipboard
        (r"(?i)\b(clipboard|paste|what.*copied)\b", "get_clipboard"),
        (r"(?i)\b(copy|set\s+clipboard)\b", "set_clipboard"),
        (r"(?i)\bscreenshot\b", "screenshot"),
        // Power
        (r"(?i)\b(shutdown|shut\s+down|power\s+off)\b", "shutdown_system"),
        (r"(?i)\b(reboot|restart)\s*(system|computer|pc)?\b", "reboot_system"),
        (r"(?i)\block\s*(screen|computer)\b", "lock_screen"),
        (r"(?i)\b(sleep|suspend)\s*(mode|computer)?\b", "sleep"),
        // System config
        (r"(?i)\b(volume|sound)\s*(set|to|at)\s*(\d+)\b", "set_volume"),
        (r"(?i)\b(brightness)\s*(set|to|at)\s*(\d+)\b", "set_brightness"),
        (r"(?i)\b(wifi)\s*(on|off|enable|disable|toggle)\b", "toggle_wifi"),
        // Internet
        (r"(?i)\b(latest|breaking|today|current|recent)\b.*\b(news|headlines|updates?)\b", "search_news"),
        (r"(?i)\b(news|headlines|updates?)\b.*\b(india|indian|pakistan|bangladesh|sri\s*lanka|us|uk|europe|asia|middle\s*east)\b", "search_news"),
        (r"(?i)\b(news|headlines|updates?)\b.*\b(authentic|trusted|reliable|verified)\b", "search_news"),
        (r"(?i)\b(search|google|look\s+up|find\s+online)\b.*\b(web|online|internet)\b", "web_search"),
        (r"(?i)\b(search|google|look\s+up)\s+(for|the|about)\b", "web_search"),
        (r"(?i)\b(ping)\s+\w+", "ping_host"),
        (r"(?i)\b(download)\s+", "download_file"),
        (r"(?i)\bspeed\s*test\b", "speed_test"),
        (r"(?i)\b(my|public)\s*ip\b", "get_public_ip"),
        // Knowledge
        (r"(?i)\bremember\s+(that|this)\b", "remember_fact"),
        (r"(?i)\b(recall|what\s+did\s+I|do\s+you\s+remember)\b", "recall_fact"),
        // Notifications
        (r"(?i)\b(notify|notification|alert)\b", "send_notification"),
        (r"(?i)\b(remind|reminder)\s+me\b", "schedule_reminder"),
        (r"(?i)\b(email|compose|draft)\s*(an?\s+)?email\b", "compose_email"),
        // Code execution
        (r"(?i)\b(run|execute)\s+(this\s+)?(bash|shell|command)\b", "execute_bash"),
        (r"(?i)\b(run|execute)\s+(this\s+)?python\b", "execute_python"),
        // Package
        (r"(?i)\binstall\s+\w+\b", "install_application"),
        (r"(?i)\buninstall\s+\w+\b", "uninstall_application"),
    ];

    mappings.into_iter()
        .filter_map(|(pat, tool)| Regex::new(pat).ok().map(|r| (r, tool)))
        .collect()
});

// ─── Verb → category mapping for complex tasks ───
static VERB_TO_CATEGORY: Lazy<HashMap<&str, &str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    for (verbs, cat) in [
        (&["open", "launch", "start", "run", "focus", "close", "quit", "kill"][..], "app_lifecycle"),
        (&["read", "write", "create", "delete", "move", "copy", "rename", "search", "find", "list"][..], "file_ops"),
        (&["install", "uninstall", "update"][..], "disk"),
        (&["volume", "brightness", "wifi", "bluetooth"][..], "system_config"),
        (&["shutdown", "reboot", "sleep", "hibernate", "lock"][..], "power"),
        (&["search", "google", "download", "fetch", "ping"][..], "internet"),
        (&["remember", "recall", "forget", "save"][..], "knowledge"),
        (&["notify", "remind", "email"][..], "communication"),
        (&["screenshot", "clipboard", "type"][..], "interaction"),
        (&["schedule", "cron"][..], "scheduler"),
    ] {
        for verb in verbs { m.insert(*verb, cat); }
    }
    m
});

/// Intent router — classifies user text into an intent.
pub struct IntentRouter;

impl IntentRouter {
    /// Classify user input text.
    pub fn classify(text: &str) -> IntentResult {
        let trimmed = text.trim();

        // 1. Check direct tool patterns first (highest confidence)
        for (re, tool) in DIRECT_TOOL_RE.iter() {
            if re.is_match(trimmed) {
                return IntentResult {
                    intent: Intent::DirectTool(tool.to_string()),
                    tool_hint: Some(tool.to_string()),
                    category: None,
                    confidence: 0.85,
                };
            }
        }

        // 2. Check conversation patterns
        for re in CONVERSATION_RE.iter() {
            if re.is_match(trimmed) {
                return IntentResult {
                    intent: Intent::Conversation,
                    tool_hint: None,
                    category: None,
                    confidence: 0.75,
                };
            }
        }

        // 3. Check verb-based category mapping
        let first_word = trimmed.split_whitespace().next().unwrap_or("").to_lowercase();
        if let Some(category) = VERB_TO_CATEGORY.get(first_word.as_str()) {
            return IntentResult {
                intent: Intent::ComplexTask,
                tool_hint: None,
                category: Some(category.to_string()),
                confidence: 0.6,
            };
        }

        // 4. Default: complex task (let LLM decide)
        IntentResult {
            intent: Intent::ComplexTask,
            tool_hint: None,
            category: None,
            confidence: 0.3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Intent, IntentRouter};

    #[test]
    fn routes_latest_news_prompts_to_search_news() {
        let result = IntentRouter::classify("Give me latest breaking news updates");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("search_news"));
    }

    #[test]
    fn routes_region_news_prompts_to_search_news() {
        let result = IntentRouter::classify("Show trusted news from India about economy");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("search_news"));
    }

    #[test]
    fn keeps_general_web_lookup_on_web_search() {
        let result = IntentRouter::classify("Search online for rust ownership examples");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("web_search"));
    }
}

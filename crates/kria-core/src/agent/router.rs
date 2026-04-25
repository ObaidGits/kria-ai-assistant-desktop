use once_cell::sync::Lazy;
use regex::Regex;
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
    ];
    patterns
        .iter()
        .filter_map(|p| Regex::new(&format!("(?i){p}")).ok())
        .collect()
});

// ─── Direct tool patterns (trigger specific tools) ───
static DIRECT_TOOL_RE: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    let mappings: Vec<(&str, &str)> = vec![
        // System stats / health (multi-metric — maps to check_system_health as entry point)
        (
            r"(?i)\b(system\s+stat(s|us)|my\s+system\s+stat|mera\s+system|system\s+vitals?)\b",
            "check_system_health",
        ),
        (
            r"(?i)\b(system\s+health|health\s+check)\b",
            "check_system_health",
        ),
        // Alerts
        (
            r"(?i)\b(show|list|get|check|current|active)\b.{0,30}\balerts?\b",
            "get_alerts",
        ),
        (
            r"(?i)\balerts?\b.{0,20}\b(show|list|active|current)\b",
            "get_alerts",
        ),
        (
            r"(?i)\bdismiss\b.{0,20}\balert\b",
            "dismiss_alert",
        ),
        // Power plan
        (
            r"(?i)\bset\b.{0,20}\bpower\s+plan\b",
            "set_power_plan",
        ),
        (
            r"(?i)\bpower\s+plan\b.{0,20}\b(set|change|switch|to)\b",
            "set_power_plan",
        ),
        (
            r"(?i)\b(current|get|what|show).{0,20}\bpower\s+plan\b",
            "get_power_plan",
        ),
        (
            r"(?i)\bpower\s+plan\b",
            "get_power_plan",
        ),
        // WiFi networks list
        (
            r"(?i)\b(list|show|available|nearby|scan)\b.{0,20}\b(wifi|wi-fi|wireless)\s*(networks?|ssid|connections?)\b",
            "get_wifi_networks",
        ),
        // Active window / window management
        (
            r"(?i)\b(active|current|focused)\b.{0,15}\bwindow\b|\bwindow.{0,15}\b(active|current|focused)\b",
            "get_active_window",
        ),
        (
            r"(?i)\b(list|show|all)\b.{0,15}\b(open\s+windows?|windows?)\b",
            "list_windows",
        ),
        // Active network connections
        (
            r"(?i)\b(active|open|current)\b.{0,20}\b(network\s+connections?|connections?|sockets?)\b",
            "get_active_connections",
        ),
        // Service management
        (
            r"(?i)\b(start|stop|restart|status|check)\b.{0,20}\b(service|daemon|systemd)\b",
            "manage_service",
        ),
        // Scheduled tasks
        (
            r"(?i)\b(list|show|my)\b.{0,20}\b(scheduled\s+tasks?|cron\s+jobs?|timers?)\b",
            "list_scheduled_tasks",
        ),
        // System info
        (
            r"(?i)\b(cpu|processor)\s*(usage|load|info|stats?|stat)\b",
            "get_cpu_usage",
        ),
        (
            r"(?i)\bmy\s+cpu\b|\bcpu\s+ka\s+(use|usage|haal)\b",
            "get_cpu_usage",
        ),
        (
            r"(?i)\b(ram|memory)\s*(usage|info|status|stats?|stat)\b",
            "get_memory_info",
        ),
        (
            r"(?i)\b(disk|storage)\s*(space|usage|info)\b",
            "get_disk_space",
        ),
        (
            r"(?i)\bcheck\s+(my\s+)?battery\b|\bbattery\s+(check|level|percent|info|status|kya|hai)\b",
            "get_battery_status",
        ),
        (
            r"(?i)\b(battery)\s*(status|level|info)\b",
            "get_battery_status",
        ),
        (
            r"(?i)\b(gpu|graphics)\s*(info|status|usage)\b",
            "get_gpu_info",
        ),
        (r"(?i)\b(uptime|how\s+long.*running)\b", "get_system_uptime"),
        (
            r"(?i)\b(network|internet)\s*(status|info|connection)\b",
            "get_network_status",
        ),
        // App lifecycle — specific patterns first, generic fallback last
        //
        // browser_search: "open Chrome and search X", "search for X on YouTube",
        //                 "play X on YouTube", "google X", "youtube search X"
        (
            r"(?i)\b(open|launch)\s+\w+\s+(and\s+)?(search|google|look\s*up|find)\b",
            "browser_search",
        ),
        (
            r"(?i)\b(search|google|look\s*up)\b.*\b(on\s+)?(youtube|chrome|firefox|browser|web)\b",
            "browser_search",
        ),
        (
            r"(?i)\b(youtube|yt)\s+(search|play|find|look\s*up)\b",
            "browser_search",
        ),
        (
            r"(?i)\b(play|search)\b.{0,40}\b(on|in|via)\s+(youtube|yt)\b",
            "browser_search",
        ),
        // Embeddings (sidecar) — MUST come before send_message so "make text embeddings"
        // is not misclassified as "text <recipient>".
        (
            r"(?i)\b(generate|create|make|compute|get)\s+(text\s+)?embeddings?\b|\bembedding\s+for\b",
            "embeddings_generate",
        ),
        // send_message: "text/message/WhatsApp/signal Anjali", "send a WhatsApp to X"
        // Excludes "text embeddings" / "text message" via the embeddings rule above.
        (
            r"(?i)\b(text|message|msg)\s+\w+\b",
            "send_message",
        ),
        (
            r"(?i)\b(send|open)\s+(a\s+)?(whatsapp|telegram|signal)\b",
            "send_message",
        ),
        (
            r"(?i)\b(whatsapp|telegram|signal)\s+(message|msg|text)?\s*(to\s+)?\w+\b",
            "send_message",
        ),
        (
            r"(?i)\bsend\s+(a\s+)?message\s+(to\s+)?\w+\b",
            "send_message",
        ),
        // open_url: "open https://...", "go to <url>"
        (
            r"(?i)\b(open|go\s+to|navigate\s+to|visit)\s+https?://\S+",
            "open_url",
        ),
        // open_application: generic — last resort for "open/launch/start <app>"
        (
            r"(?i)\b(open|launch|start|run)\s+(\w+)\b",
            "open_application",
        ),
        (r"(?i)\b(close|quit|exit)\s+(\w+)\b", "close_application"),
        (
            r"(?i)\b(running|active)\s*(apps|applications|processes)\b",
            "list_running_apps",
        ),
        (r"(?i)\b(kill|terminate)\s*(process|pid)\b", "kill_process"),
        // Google Workspace (Drive)
        (
            r"(?i)\b(list|show|browse|what'?s\s+in|what\s+is\s+in|contents?)\b.*\b(google\s+drive|drive\s+files?|drive)\b",
            "gw_drive_list",
        ),
        (
            r"(?i)\b(search|find|look\s*for|locate)\b.*\b(google\s+drive|drive\s+files?|drive)\b",
            "gw_drive_search",
        ),
        (
            r"(?i)\b(read|open|view|download|fetch)\b.*\b(google\s+drive|drive)\b.*\b(file|document|doc|spreadsheet|sheet|slides?|presentation)\b",
            "gw_drive_read",
        ),
        (
            r"(?i)\b(read|open|view|download|fetch)\b.*\b(file|document|doc|spreadsheet|sheet|slides?|presentation)\b.*\b(google\s+drive|drive)\b",
            "gw_drive_read",
        ),
        (
            r"(?i)\b(delete|remove|trash)\b.*\b(google\s+drive|drive)\b.*\b(file|document|doc|spreadsheet|sheet|slides?|presentation)\b",
            "gw_drive_delete",
        ),
        (
            r"(?i)\b(delete|remove|trash)\b.*\b(file|document|doc|spreadsheet|sheet|slides?|presentation)\b.*\b(google\s+drive|drive)\b",
            "gw_drive_delete",
        ),
        (
            r"(?i)\b(latest|recent|today|current|updates?)\b.*\b(google\s+calendar|calendar|schedule|events?)\b",
            "gw_calendar_search",
        ),
        // File ops
        (
            r"(?i)\b(read|show|cat|display)\s+(the\s+)?file\b",
            "read_file",
        ),
        (
            r"(?i)\b(list|ls|dir)\s+(the\s+)?(directory|folder|files)\b",
            "list_directory",
        ),
        (r"(?i)\b(search|find)\s+(for\s+)?files?\b", "search_files"),
        (
            r"(?i)\b(search|find|locate|look\s*for)\b.*\b(file|folder|directory)\b",
            "search_files",
        ),
        (
            r"(?i)\b(file|folder|directory)\b.*\b(named|called|name)\b",
            "search_files",
        ),
        // "search for foo.txt" / "find bar.pdf" — filename with extension implies file search
        (
            r#"(?i)\b(search|find|locate|look\s*for)\b\s+(for\s+)?["']?[\w\-./]+\.(txt|md|pdf|docx?|xlsx?|csv|json|ya?ml|toml|rs|py|js|ts|tsx|jsx|html|css|png|jpg|jpeg|gif|svg|mp3|mp4|wav|zip|tar|gz)["']?"#,
            "search_files",
        ),
        (r"(?i)\b(write|create|save)\s+(a\s+)?file\b", "write_file"),
        (r"(?i)\b(delete|remove|rm)\s+(the\s+)?file\b", "delete_file"),
        // Clipboard
        (r"(?i)\b(clipboard|paste|what.*copied)\b", "get_clipboard"),
        (r"(?i)\b(copy|set\s+clipboard)\b", "set_clipboard"),
        (r"(?i)\bscreenshot\b", "screenshot"),
        // Power
        (
            r"(?i)\b(shutdown|shut\s+down|power\s+off)\b",
            "shutdown_system",
        ),
        (
            r"(?i)\b(reboot|restart)\s*(system|computer|pc)?\b",
            "reboot_system",
        ),
        (r"(?i)\block\s*(screen|computer)\b", "lock_screen"),
        (r"(?i)\b(sleep|suspend)\s*(mode|computer)?\b", "sleep"),
        // System config — volume
        (
            r"(?i)\b(volume|sound)\s*(set|to|at)\s*(\d+)\b",
            "set_volume",
        ),
        (
            r"(?i)\b(set|change|put|increase|decrease|raise|lower|turn\s+up|turn\s+down)\b.{0,20}\b(volume|sound|speaker)\b",
            "set_volume",
        ),
        (
            r"(?i)\b(volume|sound|speaker|awaaz)\s+(ko|set|badhao|ghataao|ghatao|badha|ghata|barhao|badhaao)\b|\b(volume|sound|speaker|awaaz)\s+\d+",
            "set_volume",
        ),
        // System config — brightness
        (
            r"(?i)\b(brightness)\s*(set|to|at)\s*(\d+)\b",
            "set_brightness",
        ),
        (
            r"(?i)\b(set|change|increase|decrease|raise|lower|turn\s+up|turn\s+down)\b.{0,20}\bbrightness\b",
            "set_brightness",
        ),
        (
            r"(?i)\bbrightness\s+(ko|set|badhao|ghataao|ghatao|badha|ghata|barhao|badhaao)\b|\bbrightness\s+\d+",
            "set_brightness",
        ),
        (
            r"(?i)\b(wifi)\s*(on|off|enable|disable|toggle)\b",
            "toggle_wifi",
        ),
        // Internet
        (
            r"(?i)\b(latest|breaking|today|current|recent)\b.*\b(news|headlines|updates?)\b",
            "search_news",
        ),
        (
            r"(?i)\b(news|headlines|updates?)\b.*\b(india|indian|pakistan|bangladesh|sri\s*lanka|us|uk|europe|asia|middle\s*east)\b",
            "search_news",
        ),
        (
            r"(?i)\b(news|headlines|updates?)\b.*\b(authentic|trusted|reliable|verified)\b",
            "search_news",
        ),
        (
            r"(?i)\b(search|google|look\s+up|find\s+online)\b.*\b(web|online|internet)\b",
            "web_search",
        ),
        (
            r"(?i)\b(search|google|look\s+up)\s+(for|the|about)\b",
            "web_search",
        ),
        // Google Workspace (Gmail)
        (
            r"(?i)\b(read|open|view|show)\b.*\b(gmail|gmails|email|emails?|mail)\b.*\b(message\s*id|message_id|id)\b",
            "gw_gmail_read",
        ),
        (
            r"(?i)\b(delete|remove|trash)\b.*\b(gmail|gmails|email|emails?|mail)\b",
            "gw_gmail_delete",
        ),
        (
            r"(?i)\b(check|show|list|get|read|fetch)\b.*\b(gmail|gmails|inbox|emails?|mailbox)\b",
            "gw_gmail_inbox",
        ),
        (
            r"(?i)\b(gmail|gmails|inbox|emails?|mailbox)\b.*\b(check|show|list|get|read|fetch|recent|latest|unread)\b",
            "gw_gmail_inbox",
        ),
        (
            r"(?i)\b(unread|recent|latest)\s+(gmail|gmails|emails?)\b",
            "gw_gmail_inbox",
        ),
        (
            r"(?i)\b(search|find|look\s*for)\b.*\b(gmail|gmails|emails?|inbox)\b",
            "gw_gmail_search",
        ),
        (
            r#"(?i)\b(send|write|compose|draft)\b\s+(?:an?\s+|the\s+)?(.+?)\s+\b(mail|email|gmail)\b.*\bto\s+["']?[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}["']?"#,
            "gw_gmail_send",
        ),
        // Google Workspace (Calendar / Meet fallback via Calendar)
        (
            r"(?i)\b(what'?s|show|list|check|get|view)\b.*\b(calendar|schedule|events?)\b",
            "gw_calendar_search",
        ),
        (
            r"(?i)\b(today)\b.*\b(meetings?|events?)\b|\b(meetings?|events?)\b.*\b(today)\b",
            "gw_calendar_today",
        ),
        (
            r"(?i)\b(schedule|create|book|add|plan)\b.*\b(calendar\s+event|event|meeting|appointment|meet|call|invite)\b",
            "gw_calendar_create",
        ),
        (
            r"(?i)\b(delete|remove|cancel)\b.*\b(calendar\s+event|event|meeting|appointment)\b",
            "gw_calendar_delete",
        ),
        // Google Workspace (Docs)
        (
            r"(?i)\b(create|new|start|draft|write)\b.*\b(google\s+docs?|gdocs?|gdoc|document)\b",
            "gw_docs_create",
        ),
        (
            r"(?i)\b(read|open|show|view|summarize|extract)\b.*\b(google\s+docs?|gdocs?|gdoc|document)\b",
            "gw_docs_read",
        ),
        (
            r"(?i)\b(edit|update|append|modify)\b.*\b(google\s+docs?|gdocs?|gdoc|document)\b",
            "gw_docs_edit",
        ),
        (
            r"(?i)\b(delete|remove|trash)\b.*\b(google\s+docs?|gdocs?|gdoc|document)\b",
            "gw_drive_delete",
        ),
        // Google Workspace (Sheets)
        (
            r"(?i)\b(create|new|start|make)\b.*\b(google\s+sheets?|gsheets?|spreadsheet|sheet)\b",
            "gw_sheets_create",
        ),
        (
            r"(?i)\b(read|open|show|view|analyze)\b.*\b(google\s+sheets?|gsheets?|spreadsheet|sheet)\b",
            "gw_sheets_read",
        ),
        (
            r"(?i)\b(edit|update|write|append|modify)\b.*\b(google\s+sheets?|gsheets?|spreadsheet|sheet)\b",
            "gw_sheets_edit",
        ),
        (
            r"(?i)\b(delete|remove|trash)\b.*\b(google\s+sheets?|gsheets?|spreadsheet|sheet)\b",
            "gw_drive_delete",
        ),
        // Google Workspace (Slides)
        (
            r"(?i)\b(create|new|start|make)\b.*\b(google\s+slides?|gslides?|presentation|deck)\b",
            "gw_slides_create",
        ),
        (
            r"(?i)\b(read|open|show|view)\b.*\b(google\s+slides?|gslides?|presentation|deck)\b",
            "gw_slides_read",
        ),
        (
            r"(?i)\b(delete|remove|trash)\b.*\b(google\s+slides?|gslides?|presentation|deck)\b",
            "gw_drive_delete",
        ),
        // Google Workspace (Forms)
        (
            r"(?i)\b(list|show|read|open|find|search)\b.*\b(google\s+forms?|forms?)\b",
            "gw_forms_list",
        ),
        (
            r"(?i)\b(create|new|make|build)\b.*\b(google\s+forms?|forms?)\b",
            "gw_forms_create",
        ),
        (r"(?i)\b(ping)\s+\w+", "ping_host"),
        (r"(?i)\b(download)\s+", "download_file"),
        (r"(?i)\bspeed\s*test\b", "speed_test"),
        (r"(?i)\b(my|public)\s*ip\b", "get_public_ip"),
        (r"(?i)\bdns\s+(lookup|resolve|query)\b", "dns_lookup"),
        (r"(?i)\bcheck.{0,20}url\b|\burl.{0,20}(status|reachable|accessible)\b", "check_url_status"),
        // Internet connectivity check (must come AFTER specific internet patterns)
        (
            r"(?i)\b(connected|connection).{0,20}\b(internet|online|network)\b",
            "ping_host",
        ),
        (
            r"(?i)\b(internet|online)\b.{0,20}\b(connected|working|up|available|check)\b",
            "ping_host",
        ),
        (
            r"(?i)\bare\s+you\s+connected\b|\bam\s+i\s+online\b|\binternet\s+check\b",
            "ping_host",
        ),
        // Knowledge
        (r"(?i)\bremember\s+(that|this)\b", "remember_fact"),
        (
            r"(?i)\b(recall|what\s+did\s+I|do\s+you\s+remember)\b",
            "recall_fact",
        ),
        (r"(?i)\bsearch.{0,15}(my\s+)?(memory|knowledge)\b", "search_knowledge"),
        (r"(?i)\blist.{0,20}(remember|snippets?|knowledge)\b", "list_remembered"),
        // Notifications — keep general alert after dismiss_alert above
        (
            r"(?i)\b(notify|notification)\b|\bsend\s+(me\s+a\s+)?notification\b",
            "send_notification",
        ),
        (r"(?i)\b(remind|reminder)\s+me\b", "schedule_reminder"),
        (
            r"(?i)\b(email|compose|draft)\s*(an?\s+)?email\b",
            "compose_email",
        ),
        // Code execution
        (
            r"(?i)\b(run|execute)\s+(this\s+)?(bash|shell|command)\b",
            "execute_bash",
        ),
        (
            r"(?i)\b(run|execute)\s+(this\s+)?python\b",
            "execute_python",
        ),
        // Developer / git
        (r"(?i)\bgit\s+(status|stat)\b", "git_status"),
        (r"(?i)\bgit\s+(log|history|commits?)\b", "git_log"),
        (r"(?i)\bgit\s+diff\b", "git_diff"),
        (r"(?i)\bgit\s+(commit|save)\b", "git_commit"),
        (r"(?i)\bgit\s+(branch|branches)\b", "git_branch_list"),
        (r"(?i)\bgit\s+(stash)\b", "git_stash"),
        (r"(?i)\bgit\s+(push)\b", "git_push"),
        (r"(?i)\bgit\s+(checkout|switch)\b", "git_checkout"),
        (r"(?i)\banalyze.{0,20}(project|codebase|repo)\b", "analyze_project"),
        // File ops extras
        (r"(?i)\bcount\s+(lines|loc)\b", "count_lines_of_code"),
        (r"(?i)\bproject\s+(structure|tree|layout)\b", "get_project_structure"),
        (r"(?i)\b(find|show).{0,20}(todo|fixme)\b", "find_todos"),
        (r"(?i)\b(dir|folder)\s*(size|how\s+big)\b|\bhow\s+big.{0,20}(dir|folder|directory)\b", "calculate_dir_size"),
        // Image generation — MUST come before vision "analyze image" rule to avoid shadowing.
        // Covers: "generate/create/make/draw/paint/design an image/picture/photo/art of ..."
        // Also handles: "draw me a robot", "make me an image"
        (
            r"(?i)\b(generate|create|make|draw|paint|design|render|produce)\s+(me\s+)?(a\s+|an\s+|one\s+)?\b(image|picture|photo|artwork|art|illustration|wallpaper|poster|banner|thumbnail)\b",
            "generate_image",
        ),
        // Handle "generate/draw/paint/create an image/photo/art OF ..."
        (
            r"(?i)\b(generate|create|make|draw|paint|design|render|produce)\b.{0,30}\b(image|picture|photo|artwork|art|illustration|wallpaper|poster|banner|thumbnail)\b",
            "generate_image",
        ),
        // Hinglish: "image banao", "photo bana", "tasveer banao"
        (
            r"(?i)\b(image|photo|tasveer|pic)\s*(banao?|bana|create|generate|draw)\b|\b(banao?|bana)\s*(ek\s+)?(image|photo|tasveer|pic)\b",
            "generate_image",
        ),
        // Vision extras
        (r"(?i)\b(ocr|extract\s+text).{0,20}image\b", "ocr_image"),
        (r"(?i)\banalyze.{0,20}image\b|\bwhat.{0,20}\b(on\s+)?screen\b", "screenshot_analyze"),
        // Article extraction (sidecar)
        (
            r"(?i)\bextract\s+(the\s+)?article\b",
            "web_extract_article",
        ),
        // Embeddings (sidecar)
        (
            r"(?i)\b(generate|create|make|compute|get)\s+(text\s+)?embeddings?\b|\bembedding\s+for\b",
            "embeddings_generate",
        ),
        // Accessibility settings
        (
            r"(?i)\b(get|show|list|view|check)\b.{0,10}\baccessibility\b|^accessibility\s+settings\b",
            "get_accessibility_settings",
        ),
        // Languages list (must come BEFORE conversation 'what is/are' patterns via DIRECT_TOOL precedence)
        (
            r"(?i)\b(what|which)\s+languages?\s+(do\s+)?(you\s+)?(support|speak)\b|\blist\s+(supported\s+)?languages?\b",
            "list_languages",
        ),
        // Installed packages / applications listing
        (
            r"(?i)\blist\s+(all\s+)?(installed\s+)?(applications?|apps?|packages?|programs?)\b",
            "list_installed_packages",
        ),
        (
            r"(?i)\b(installed|all)\s+(applications?|apps?|packages?|programs?)\b",
            "list_installed_packages",
        ),
        // Package — use correct tool name
        (r"(?i)\binstall\s+\w+\b", "install_package"),
        (r"(?i)\buninstall\s+\w+\b", "uninstall_package"),
        (r"(?i)\bremove\s+package\b|\bremove\s+\w+\s+package\b", "uninstall_package"),
        // Hinglish patterns — fetch_webpage must come BEFORE generic Hinglish so URLs aren't lost
        // Web — fetch_webpage (placed after all Google Workspace patterns so gdocs/gsheets take priority)
        (
            r"(?i)\b(fetch|scrape|get|read|load)\b.{0,40}https?://",
            "fetch_webpage",
        ),
        (
            r"(?i)\bfetch\s+the\s+content\s+of\b",
            "fetch_webpage",
        ),
        (
            r"(?i)\b(get|load|scrape|read)\s+the\s+(content|page|text|html)\b",
            "fetch_webpage",
        ),
        // Hinglish patterns
        (
            r"(?i)\bvolume\s+(band|zero|mute|off)\s+karo\b|\bband\s+karo\b.{0,15}volume",
            "set_volume",
        ),
        (r"(?i)\b(cpu|processor)\s+kitna\b|\bram\s+(kitna|kya)\b", "get_cpu_usage"),
        (r"(?i)\binternet\s+(hai|check|nahi|connected)\b", "ping_host"),
        (r"(?i)\bbattery\s+(kitna|kya|check)\b", "get_battery_status"),
    ];

    mappings
        .into_iter()
        .filter_map(|(pat, tool)| Regex::new(pat).ok().map(|r| (r, tool)))
        .collect()
});

// ─── Verb → category mapping for complex tasks ───
static VERB_TO_CATEGORY: Lazy<HashMap<&str, &str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    for (verbs, cat) in [
        (
            &[
                "open", "launch", "start", "run", "focus", "close", "quit", "kill",
            ][..],
            "app_lifecycle",
        ),
        (
            &[
                "read", "write", "create", "delete", "move", "copy", "rename", "search", "find",
                "list",
            ][..],
            "file_ops",
        ),
        (&["install", "uninstall", "update"][..], "disk"),
        (
            &["volume", "brightness", "wifi", "bluetooth"][..],
            "system_config",
        ),
        (
            &["shutdown", "reboot", "sleep", "hibernate", "lock"][..],
            "power",
        ),
        (
            &["search", "google", "download", "fetch", "ping"][..],
            "internet",
        ),
        (&["remember", "recall", "forget", "save"][..], "knowledge"),
        (&["notify", "remind", "email"][..], "communication"),
        (&["screenshot", "clipboard", "type"][..], "interaction"),
        (&["schedule", "cron"][..], "scheduler"),
    ] {
        for verb in verbs {
            m.insert(*verb, cat);
        }
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
        let first_word = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
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

    #[test]
    fn routes_check_gmail_prompts_to_gmail_inbox_tool() {
        let result = IntentRouter::classify("check my gmail for unread emails");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_gmail_inbox"));
    }

    #[test]
    fn routes_search_gmail_prompts_to_gmail_search_tool() {
        let result = IntentRouter::classify("search gmail for from:boss subject:invoice");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_gmail_search"));
    }

    #[test]
    fn routes_fetch_latest_unread_gmails_to_gmail_inbox_tool() {
        let result = IntentRouter::classify("Fetch 3 latest unread gmails");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_gmail_inbox"));
    }

    #[test]
    fn routes_send_mail_prompts_to_gmail_send_tool() {
        let result =
            IntentRouter::classify("Send a Hye mail to \"zeeshanobaid335@gmail.com\"");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_gmail_send"));
    }

    #[test]
    fn routes_delete_gmail_prompts_to_gmail_delete_tool() {
        let result = IntentRouter::classify("Delete this email message_id 18af9f0a8bcdef12");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_gmail_delete"));
    }

    #[test]
    fn routes_schedule_meeting_to_calendar_create_tool() {
        let result = IntentRouter::classify("Schedule a Google Meet for tomorrow at 3pm");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_calendar_create"));
    }

    #[test]
    fn routes_calendar_cancel_prompts_to_calendar_delete_tool() {
        let result = IntentRouter::classify("Cancel my calendar event with event id abc123def456");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_calendar_delete"));
    }

    #[test]
    fn routes_create_doc_to_docs_create_tool() {
        let result = IntentRouter::classify("Create a new Google Doc called Weekly Plan");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_docs_create"));
    }

    #[test]
    fn routes_forms_listing_to_curated_forms_tool() {
        let result = IntentRouter::classify("List my google forms");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_forms_list"));
    }

    #[test]
    fn routes_actionable_question_to_calendar_today_tool() {
        let result = IntentRouter::classify("Do I have meetings today?");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_calendar_today"));
    }

    #[test]
    fn routes_drive_listing_prompts_to_drive_list_tool() {
        let result = IntentRouter::classify("List files in my Google drive");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_drive_list"));
    }

    #[test]
    fn routes_drive_read_prompts_to_drive_read_tool() {
        let result = IntentRouter::classify("Read this file from Google Drive");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_drive_read"));
    }

    #[test]
    fn routes_docs_delete_prompts_to_drive_delete_tool() {
        let result = IntentRouter::classify("Delete this Google Doc");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_drive_delete"));
    }

    #[test]
    fn routes_sheets_delete_prompts_to_drive_delete_tool() {
        let result = IntentRouter::classify("Remove this spreadsheet from my drive");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_drive_delete"));
    }

    #[test]
    fn routes_calendar_update_prompts_to_calendar_search_tool() {
        let result = IntentRouter::classify("Get latest updates about Google calendar");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("gw_calendar_search"));
    }

    #[test]
    fn routes_folder_lookup_prompts_to_search_files() {
        let result = IntentRouter::classify("search for folder name zrok");
        assert!(matches!(result.intent, Intent::DirectTool(_)));
        assert_eq!(result.tool_hint.as_deref(), Some("search_files"));
    }
}

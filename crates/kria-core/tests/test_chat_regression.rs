// ─────────────────────────────────────────────────────────────────────────────
//  test_chat_regression.rs
//
//  Regression tests for real user chat failures observed on 2026-04-24
//  (see kria_hi_2026-04-24.txt and kria_Search_for_notes_txt_file_2026-04-24.txt).
//
//  Each test guards a SPECIFIC failure mode that broke the assistant in a
//  real session.  These tests must NEVER be removed — they are the floor
//  below which user-visible quality cannot regress.
//
//  Failure-IDs (REG-*) map to the chat-export entries documented in
//  /media/obaid/SSD/KRIA/diagnostics — keep them stable across refactors.
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F1 — "Open chrome and search for youtube" must always route to a
//           browser tool.  In chat 1 the model refused; in chat 2 it ran
//           browser_search.  This guards consistency.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f1_open_chrome_and_search_routes_to_browser_search() {
    let phrases = [
        "Open chrome and search for youtube",
        "open Chrome and search YouTube",
        "open firefox and search for rust docs",
        "launch chrome and look up cricket scores",
    ];
    for p in &phrases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert!(
                t == "browser_search" || t == "open_url" || t == "open_application",
                "REG-F1: '{p}' should route to a browser tool, got {t}"
            ),
            other => panic!("REG-F1: '{p}' must produce a DirectTool intent, got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F2 — "Search for notes.txt" / "Search for sem-8.pdf"
//           must route to search_files (NOT web_search, NOT mcp_fs only).
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f2_search_for_filename_with_extension_routes_to_search_files() {
    let cases = [
        "Search for notes.txt",
        "Search for \"notes.txt\" file",
        "Search for sem-8.pdf",
        "find report.docx",
        "look for data.csv",
        "locate config.toml",
        "search for pic.png",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "search_files",
                "REG-F2: '{p}' must route to search_files, got {t}"
            ),
            other => panic!("REG-F2: '{p}' must be DirectTool(search_files), got {other:?}"),
        }
    }
}

#[test]
fn reg_f2b_search_for_filename_must_not_route_to_web_search() {
    // The chat showed "Search for sem-8.pdf" routed to web_search → empty results.
    let phrases = ["Search for notes.txt", "Search for sem-8.pdf", "find report.docx"];
    for p in &phrases {
        let r = IntentRouter::classify(p);
        if let Intent::DirectTool(t) = &r.intent {
            assert_ne!(t, "web_search", "REG-F2b: '{p}' must NOT go to web_search");
            assert_ne!(t, "fetch_webpage", "REG-F2b: '{p}' must NOT go to fetch_webpage");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F3 — Read-only file searches are GREEN tier and must NOT trigger HITL.
//           (Chat showed "operation was denied by the user" for search.)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f3_search_files_is_green_no_approval() {
    use kria_core::safety::policy::{PolicyEngine, RiskLevel};
    let p = PolicyEngine::new();
    for tool in ["search_files", "read_file", "list_directory", "find_files_by_pattern"] {
        let d = p.evaluate(tool, &serde_json::json!({ "directory": "/home/obaid", "pattern": "*.txt" }));
        assert_eq!(d.risk_level, RiskLevel::Green, "REG-F3: {tool} must be Green");
        assert!(!d.requires_approval, "REG-F3: {tool} must not require approval");
        assert!(!d.blocked, "REG-F3: {tool} must not be blocked");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F4 — "What is my CPU stats?" must call get_cpu_usage tool.
//           Chat showed model emitted ```bash top -b -n 1``` without calling.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f4_what_is_my_cpu_stats_routes_to_get_cpu_usage() {
    let cases = [
        "What is my CPU stats?",
        "what's my CPU stats",
        "show CPU stats",
        "cpu stats?",
        "CPU usage",
        "processor info",
        "my CPU",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => {
                let ok = t == "get_cpu_usage" || t == "check_system_health";
                assert!(ok, "REG-F4: '{p}' must route to get_cpu_usage or check_system_health, got {t}");
            }
            other => panic!("REG-F4: '{p}' must be DirectTool, got {other:?}"),
        }
    }
}

#[test]
fn reg_f4b_system_stats_routes_correctly() {
    let r = IntentRouter::classify("What are my system stats?");
    match &r.intent {
        Intent::DirectTool(t) => {
            assert!(
                t == "check_system_health" || t == "get_cpu_usage",
                "system stats should route to a system tool, got {t}"
            );
        }
        other => panic!("system stats must be DirectTool, got {other:?}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F5 — "Extract the article from <URL>" must route to web_extract_article.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f5_extract_article_routes_to_web_extract_article() {
    let cases = [
        "Extract the article from https://arxiv.org/abs/2302.04761",
        "extract article from https://example.com/post",
        "extract the article",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "web_extract_article",
                "REG-F5: '{p}' must route to web_extract_article, got {t}"
            ),
            other => panic!("REG-F5: '{p}' must be DirectTool(web_extract_article), got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F6 — "Generate embeddings for: ..." must route to embeddings_generate.
//           Chat showed model said "current environment does not have the
//           capability" — but embeddings_generate IS registered (precognitive).
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f6_generate_embeddings_routes_to_embeddings_generate() {
    let cases = [
        "Generate embeddings for the text: 'machine learning in Rust'",
        "generate embeddings for hello world",
        "create embeddings for this sentence",
        "compute embedding for: foo",
        "make text embeddings for x",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "embeddings_generate",
                "REG-F6: '{p}' must route to embeddings_generate, got {t}"
            ),
            other => panic!("REG-F6: '{p}' must be DirectTool(embeddings_generate), got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F7 — "What languages do you support?" must route to list_languages.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f7_what_languages_routes_to_list_languages() {
    let cases = [
        "What languages do you support?",
        "what languages do you speak",
        "which languages do you support",
        "list supported languages",
        "list languages",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "list_languages",
                "REG-F7: '{p}' must route to list_languages, got {t}"
            ),
            other => panic!("REG-F7: '{p}' must be DirectTool(list_languages), got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F8 — "Get accessibility settings" must route to get_accessibility_settings
//           (NOT a bash xsettingsd suggestion).
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f8_accessibility_settings_routes_to_correct_tool() {
    let cases = [
        "Get accessibility settings.",
        "get accessibility settings",
        "show accessibility settings",
        "list accessibility settings",
        "view accessibility",
        "check accessibility",
        "accessibility settings",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "get_accessibility_settings",
                "REG-F8: '{p}' must route to get_accessibility_settings, got {t}"
            ),
            other => panic!("REG-F8: '{p}' must be DirectTool, got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F9 — "Open WhatsApp" should route to either send_message or
//           browser/open_url (Web fallback).  Never refuse outright.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f9_open_whatsapp_routes_somewhere_useful() {
    let cases = [
        "Open Whatsapp",
        "open whatsapp",
        "Open Chrome and search whatsapp web",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => {
                let ok = matches!(
                    t.as_str(),
                    "send_message" | "open_application" | "browser_search" | "open_url"
                );
                assert!(
                    ok,
                    "REG-F9: '{p}' must route to a launcher/browser tool, got {t}"
                );
            }
            other => panic!("REG-F9: '{p}' must be DirectTool, got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F10 — "list all the installed applications" must route to a tool
//            that returns the package/app list.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f10_list_installed_applications_routes_to_packages_tool() {
    let cases = [
        "list all the installed applications",
        "list installed apps",
        "list installed packages",
        "show all installed programs",
        "all installed apps",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "list_installed_packages",
                "REG-F10: '{p}' must route to list_installed_packages, got {t}"
            ),
            other => panic!("REG-F10: '{p}' must be DirectTool, got {other:?}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-F11 — gw_drive_search MCP failures must surface a friendly tool name,
//            not the raw mcp_call_failed JSON contract.  This tool is added
//            to the registry by the kria-desktop runtime via the Google
//            Workspace MCP bridge, so we cannot probe registration here.
//            What we CAN guard: the policy classification of any future
//            registration must be Green so reads do not require approval.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_f11_gw_drive_search_policy_is_green() {
    use kria_core::safety::policy::{PolicyEngine, RiskLevel};
    let p = PolicyEngine::new();
    let d = p.evaluate("gw_drive_search", &serde_json::json!({ "query": "quarterly report" }));
    // Even if not yet listed in the policy table, default for unknown read-style
    // tool names should not be Red/Black. This guards against a regression that
    // would force HITL on every Drive search.
    assert!(
        matches!(d.risk_level, RiskLevel::Green | RiskLevel::Yellow),
        "REG-F11: gw_drive_search must classify as Green/Yellow at worst, got {:?}",
        d.risk_level
    );
    assert!(!d.blocked, "REG-F11: gw_drive_search must not be blocked");
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-TOOLS — Sanity: every tool referenced by the regression router rules
//              MUST exist in the registry, OR the router is sending users to
//              dead ends.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_tools_all_router_targets_exist_in_registry() {
    // Build the full registry the way the live app does (with sidecar tools).
    let reg = registry::build_default_registry();

    // We cannot register precognitive without a SidecarBridge here, so we
    // verify the precognitive-backed tools live in source and accept that
    // they are added by kria-desktop at runtime.  The non-sidecar tools below
    // MUST be present in build_default_registry().
    let must_exist = [
        "get_cpu_usage",
        "get_memory_info",
        "get_disk_space",
        "get_battery_status",
        "check_system_health",
        "search_files",
        "read_file",
        "list_directory",
        "list_languages",
        "get_accessibility_settings",
        "list_installed_packages",
        "browser_search",
        "open_application",
        "open_url",
        "send_message",
        "fetch_webpage",
        "web_search",
        // generate_image is registered by kria-desktop (requires ImageOrchestrator),
        // so we only test routing — not registry presence — in this file.
    ];
    for t in &must_exist {
        assert!(
            reg.get_def(t).is_some(),
            "REG-TOOLS: registry is missing '{t}' — router can route to it but no handler exists"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-NO-BASH — When the user asks something whose answer is a tool, the
//                router must NOT push the request to the LLM unguided.  We
//                assert these prompts produce a DirectTool intent (so the
//                LLM is guided to a tool and cannot hallucinate a bash block).
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_no_bash_critical_prompts_are_routed_to_tools() {
    // Each of these previously triggered ```bash hallucination in production.
    let prompts_that_must_be_direct_tool = [
        "What is my CPU stats?",
        "Get accessibility settings.",
        "list all the installed applications",
        "Search for notes.txt",
        "Generate embeddings for the text: 'machine learning in Rust'",
        "Extract the article from https://arxiv.org/abs/2302.04761",
        "What languages do you support?",
    ];
    for p in &prompts_that_must_be_direct_tool {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(r.intent, Intent::DirectTool(_)),
            "REG-NO-BASH: '{p}' must produce a DirectTool intent so the LLM cannot hallucinate bash; \
             actual intent: {:?}",
            r.intent
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-DENY-MSG — HITL "Denied" must come ONLY from ApprovalResponse::Denied,
//                 never from Timeout or transport errors.  Guards against the
//                 chat where "operation denied by user" appeared without the
//                 user denying anything.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reg_deny_msg_timeout_is_not_called_denied() {
    use kria_core::safety::hitl::{ApprovalResponse, HitlGateway};
    use std::sync::Arc;
    use std::time::Duration;

    let gw = Arc::new(HitlGateway::new(1)); // 1-second timeout
    let id = HitlGateway::generate_request_id();

    let gw2 = Arc::clone(&gw);
    let handle = tokio::spawn(async move {
        gw2.request_approval_with_id(
            &id,
            "search_files",
            serde_json::json!({}),
            kria_core::safety::policy::RiskLevel::Red,
            "test",
            false,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(1300)).await;
    let resp = handle.await.expect("HITL task panicked");
    assert!(
        matches!(resp, ApprovalResponse::Timeout),
        "REG-DENY-MSG: a non-response must produce Timeout, never Denied; got {resp:?}"
    );
    assert!(
        !matches!(resp, ApprovalResponse::Denied),
        "REG-DENY-MSG: Denied must require an explicit user action"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  REG-IMAGE-GEN — "generate an image" must route to generate_image tool,
//                  never hallucinate bash/Inkscape/GIMP/Stable Diffusion.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn reg_image_generation_routes_to_tool() {
    use kria_core::agent::router::{Intent, IntentRouter};

    let must_route = [
        // English
        "Generate an image of a flying car",
        "Create an image of a sunset over the mountains",
        "Draw a picture of a cat",
        "Make an image of a robot",
        "Paint an artwork of the Eiffel Tower at night",
        "Design a wallpaper of a space station",
        "Render an illustration of a dragon",
        // With style hints
        "Generate an anime image of a girl with blue hair",
        "Create a photorealistic photo of a sports car",
        "Draw a cartoon picture of a dog",
        // Hinglish
        "ek flying car ki image banao",
        "photo banao ek sunset ki",
    ];

    for p in &must_route {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(
                t, "generate_image",
                "REG-IMAGE-GEN: '{p}' must route to generate_image, got '{t}'"
            ),
            other => panic!(
                "REG-IMAGE-GEN: '{p}' must be DirectTool(generate_image), got {other:?}"
            ),
        }
    }
}

#[test]
fn reg_no_bash_image_generation_never_hallucinates_shell() {
    use kria_core::agent::router::{Intent, IntentRouter};

    let prompts = [
        "Generate an image of a flying car",
        "Create a picture of mountains",
        // Note: short prompts like "Draw me a robot" may be ComplexTask — that's acceptable
        // because the LLM still has generate_image in its tool catalog and will use it.
        "Draw me an image of a robot",
        "Make an image of a sunset",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(r.intent, Intent::DirectTool(_)),
            "REG-NO-BASH: '{p}' must produce DirectTool so LLM cannot hallucinate inkscape/gimp/bash; \
             actual: {:?}",
            r.intent
        );
    }
}

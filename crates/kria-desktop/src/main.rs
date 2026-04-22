#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod commands;
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;

static RUNTIME_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

fn main() {
    // Ring 4 — install Linux seccomp-BPF filter before anything else.
    // On non-Linux platforms this is a no-op.
    if let Err(e) = kria_core::platform::install_seccomp_filter() {
        eprintln!("[WARN] seccomp filter not installed: {e}");
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // Register the AppStateCell immediately so Tauri never panics with
            // "state not managed" — commands that arrive before init_runtime()
            // finishes will get a clean "still initializing" error instead.
            app.handle().manage(commands::AppStateCell::new());

            // Initialize tray icon
            tray::create_tray(app.handle())?;

            // Initialize kria-core runtime in background
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = commands::init_runtime(&handle).await {
                    tracing::error!("failed to initialize KRIA runtime: {e}");
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::send_message,
            commands::send_lab_message,
            commands::get_session_history,
            commands::create_session,
            commands::list_sessions,
            commands::switch_session,
            commands::delete_session,
            commands::rename_session,
            commands::search_sessions,
            commands::cancel_request,
            commands::cancel_turn,
            commands::approve_action,
            commands::deny_action,
            commands::get_health,
            commands::get_settings,
            commands::list_audio_devices,
            commands::update_settings,
            commands::list_models,
            commands::start_voice,
            commands::stop_voice,
            commands::get_voice_status,
            commands::send_image_message,
            commands::list_mcp_servers,
            commands::reconcile_mcp_runtime,
            commands::add_mcp_server,
            commands::remove_mcp_server,
            commands::toggle_mcp_server,
            commands::restart_mcp_server_runtime,
            commands::get_telegram_config,
            commands::update_telegram_config,
            commands::start_telegram_mcp,
            commands::stop_telegram_mcp,
            commands::test_telegram_connection,
            commands::list_scheduled_tasks,
            commands::add_scheduled_task,
            commands::remove_scheduled_task,
            commands::list_macros,
            commands::start_macro_recording,
            commands::stop_macro_recording,
            commands::delete_macro,
            commands::list_workflows,
            commands::delete_workflow,
            commands::get_hardware_info,
            commands::list_knowledge_base,
            commands::get_alerts,
            commands::save_export_file,
            commands::open_html_for_print,
            commands::get_colab_tier_status,
            commands::connect_colab_tier,
            commands::disconnect_colab_tier,
            commands::set_colab_selected_notebook,
            commands::get_google_workspace_status,
            commands::set_google_workspace_account,
            commands::connect_google_workspace,
            commands::disconnect_google_workspace,
            commands::get_orchestrator_status,
            // Provisioning (first-boot setup wizard)
            commands::get_provisioning_state,
            commands::start_provisioning,
            commands::complete_provisioning,
            commands::set_provisioning_backend,
            commands::run_provisioning_step,
            commands::get_provisioning_diagnostics,
            commands::get_hardware_profile,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            if RUNTIME_SHUTDOWN_REQUESTED.swap(true, Ordering::SeqCst) {
                return;
            }

            tauri::async_runtime::block_on(commands::shutdown_runtime(app_handle));
        }
    });
}

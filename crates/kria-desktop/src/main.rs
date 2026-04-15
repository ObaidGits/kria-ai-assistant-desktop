#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod commands;
mod tray;

fn main() {
    tauri::Builder::default()
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
            commands::get_session_history,
            commands::create_session,
            commands::list_sessions,
            commands::switch_session,
            commands::delete_session,
            commands::rename_session,
            commands::search_sessions,
            commands::cancel_request,
            commands::approve_action,
            commands::deny_action,
            commands::get_health,
            commands::get_settings,
            commands::update_settings,
            commands::list_models,
            commands::start_voice,
            commands::stop_voice,
            commands::get_voice_status,
            commands::send_image_message,
            commands::list_mcp_servers,
            commands::add_mcp_server,
            commands::remove_mcp_server,
            commands::toggle_mcp_server,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

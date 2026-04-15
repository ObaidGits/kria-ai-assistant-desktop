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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

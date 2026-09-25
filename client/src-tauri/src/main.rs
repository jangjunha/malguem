#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_loopback;
mod game_detect;
mod keychain;
mod webview_permissions;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        // Open shared links in the user's real browser (never our webview), and
        // fetch OpenGraph metadata client-side (no CORS, server stays blind).
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        // Mute/deafen hotkeys that fire while a fullscreen game has focus.
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            keychain::keychain_get,
            keychain::keychain_set,
            keychain::keychain_delete,
            game_detect::fullscreen_app_active,
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                // Allow microphone/camera in the webview (Windows needs this).
                webview_permissions::allow_media(&window);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running malguem");
}

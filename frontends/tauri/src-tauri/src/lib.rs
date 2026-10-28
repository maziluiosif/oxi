//! The native half of the Tauri app: the `oxi headless` bridge the web UI talks to, and the
//! pseudo-terminals of the terminal panel. Everything about chats, git and files goes through
//! the oxi daemon, shared with the egui app, the editor plugins and the phone.

mod bridge;
mod pty;

use tauri::{Manager, RunEvent};

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(bridge::Bridge::default())
        .manage(pty::Terminals::default())
        .invoke_handler(tauri::generate_handler![
            bridge::oxi_start,
            bridge::oxi_send,
            bridge::oxi_stop,
            bridge::oxi_binary,
            pty::pty_spawn,
            pty::pty_write,
            pty::pty_resize,
            pty::pty_kill,
            home_dir,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                apply_vibrancy(&window);
            }
            let _ = app;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the oxi Tauri app");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            handle.state::<bridge::Bridge>().stop();
            handle.state::<pty::Terminals>().kill_all();
        }
    });
}

#[tauri::command]
fn home_dir() -> Option<String> {
    dirs::home_dir().map(|p| p.to_string_lossy().to_string())
}

/// The sidebar sits on the window's frosted material, like Finder's.
#[cfg(target_os = "macos")]
fn apply_vibrancy(window: &tauri::WebviewWindow) {
    use tauri::window::{Effect, EffectState, EffectsBuilder};
    let effects = EffectsBuilder::new()
        .effect(Effect::Sidebar)
        .state(EffectState::FollowsWindowActiveState)
        .build();
    let _ = window.set_effects(effects);
}

mod api;
mod axum_server;
mod config;
mod convert;
mod proxy;
mod takeover;
mod tray;

use config::load_config;
use tauri::Manager;
use crate::takeover::{check_codex_takeover_status, check_takeover_status,
    take_over_claude, take_over_codex};

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // 1. Load config
            let app_config = load_config().map_err(|e| e.to_string())?;

            // 2. Create shared AppState (frontend is embedded via rust-embed)
            let state = config::AppState::new(app_config)
                .map_err(|e| e.to_string())?;

            // 4. Start axum server in background thread with its own runtime.
            //    Uses a Notify-based shutdown signal so the thread can be
            //    gracefully stopped before process exit.
            let shutdown = config::ServerShutdown::new();
            let shutdown_notify = shutdown.notifier();
            app.manage(shutdown);

            let (tx, rx) = std::sync::mpsc::channel();
            let server_state = state.clone();
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        log::error!("[Tauri] failed to create tokio runtime: {}", e);
                        return;
                    }
                };
                let port = rt.block_on(async { axum_server::start(server_state).await });
                let _ = tx.send(port);
                // Wait for shutdown signal instead of blocking forever.
                // The tray "Quit" handler calls notify_one() on this Notify.
                rt.block_on(shutdown_notify.notified());
                log::info!("[Tauri] axum server shutting down");
            });

            // 5. Wait for server to be ready
            let port = rx.recv().map_err(|e| format!("server failed to start: {}", e))?;
            log::info!("[Tauri] axum server ready on port {}", port);

            // 5b. Rewrite stale takeover configs if the port changed since last run
            // This ensures CLI tools (Claude Code, Codex) can reach the proxy even
            // if the user changed the port in config.yaml.
            refresh_takeover_configs(port);

            // 6. Create main window
            // In dev mode, load from Vite dev server (API calls are proxied by Vite)
            // In production, load from axum (which serves bundled static files)
            let window_url = if cfg!(debug_assertions) {
                // cannot fail: well-known URL format
                "http://localhost:5173".parse().unwrap()
            } else {
                // cannot fail: well-formed URL from known port
                format!("http://127.0.0.1:{}", port).parse().unwrap()
            };
            log::info!("[Tauri] opening window at {}", window_url);

            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(window_url),
            )
            .title("AginxLLM")
            .inner_size(960.0, 700.0)
            .min_inner_size(600.0, 400.0)
            .center()
            .build()?;

            // 7. Close-to-tray: intercept close request, hide instead
            let window_clone = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_clone.hide();
                }
            });

            // 8. Set up system tray
            tray::setup_tray(app)?;

            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        log::error!("[Tauri] fatal error: {}", e);
        panic!("error while running tauri application: {}", e);
    }
}

/// On startup, rewrite takeover configs if they were active but point to a
/// different port than the one the server just bound to.  This handles the
/// common case where the user changes `port` in config.yaml and restarts.
fn refresh_takeover_configs(port: u16) {
    // Claude Code takeover
    let claude_status = check_takeover_status(port);
    if claude_status.active {
        log::info!("[Startup] Claude Code takeover active on correct port {}", port);
    } else {
        // Check if settings.json has a stale model-router base URL
        let claude_settings = match dirs::home_dir() {
            Some(h) => h.join(".claude").join("settings.json"),
            None => return,
        };
        if claude_settings.exists() {
            if let Ok(content) = std::fs::read_to_string(&claude_settings) {
                if content.contains("model-router") || content.contains("127.0.0.1") {
                    log::info!("[Startup] Refreshing stale Claude Code takeover to port {}", port);
                    if let Err(e) = take_over_claude(port) {
                        log::warn!("[Startup] Failed to refresh Claude Code takeover: {}", e);
                    }
                }
            }
        }
    }

    // Codex takeover
    let codex_status = check_codex_takeover_status(port);
    if codex_status.active {
        log::info!("[Startup] Codex takeover active on correct port {}", port);
    } else {
        // Check if config.toml has a stale model-router provider
        let codex_config_path = match dirs::home_dir() {
            Some(h) => h.join(".codex").join("config.toml"),
            None => return,
        };
        if codex_config_path.exists() {
            let content = match std::fs::read_to_string(&codex_config_path) {
                Ok(c) => c,
                Err(_) => return,
            };
            if content.contains("model-router") && !content.contains(&format!("127.0.0.1:{}", port)) {
                log::info!("[Startup] Refreshing stale Codex takeover to port {}", port);
                if let Err(e) = take_over_codex(port, "gpt-5.5") {
                    log::warn!("[Startup] Failed to refresh Codex takeover: {}", e);
                }
            }
        }
    }
}

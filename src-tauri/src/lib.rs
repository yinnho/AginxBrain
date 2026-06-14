mod api;
mod axum_server;
mod config;
mod convert;
mod db;
mod models;
mod proxy;
mod takeover;

#[cfg(feature = "desktop")]
mod tray;

use config::load_config;
use crate::takeover::{check_codex_takeover_status, check_takeover_status,
    take_over_claude, take_over_codex};

#[cfg(feature = "desktop")]
use tauri::Manager;

/// Desktop mode: Tauri window + system tray + Axum server in background.
#[cfg(feature = "desktop")]
pub fn run_desktop(port_override: Option<u16>, host_override: Option<String>) {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(move |app| {
            // 1. Load config
            let mut app_config = load_config().map_err(|e| e.to_string())?;
            if let Some(p) = port_override { app_config.port = p; }
            if let Some(h) = host_override.clone() { app_config.host = h; }

            // 2. Create shared AppState
            let state = std::thread::scope(|s| {
                s.spawn(|| {
                    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
                    rt.block_on(config::AppState::new(app_config)).map_err(|e| e.to_string())
                }).join()
            }).map_err(|e| format!("failed to create state: {:?}", e))?;
            let state = state?;

            // 3. Start axum server in background thread
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
                let (host, port) = rt.block_on(async { axum_server::start(server_state).await });
                let _ = tx.send((host, port));
                rt.block_on(shutdown_notify.notified());
                log::info!("[Tauri] axum server shutting down");
            });

            // 4. Wait for server to be ready
            let (host, port) = rx.recv().map_err(|e| format!("server failed to start: {}", e))?;
            log::info!("[Tauri] axum server ready on {}:{}", host, port);

            // 5. Rewrite stale takeover configs if the port changed
            refresh_takeover_configs(port);

            // 6. Create main window
            let window_url = if cfg!(debug_assertions) {
                "http://localhost:5173".parse().unwrap()
            } else {
                format!("http://{}:{}", host, port).parse().unwrap()
            };
            log::info!("[Tauri] opening window at {}", window_url);

            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(window_url),
            )
            .title("AginxBrain")
            .inner_size(960.0, 700.0)
            .min_inner_size(600.0, 400.0)
            .center()
            .build()?;

            // 7. Close-to-tray
            let window_clone = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_clone.hide();
                }
            });

            // 8. System tray
            tray::setup_tray(app)?;

            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        log::error!("[Tauri] fatal error: {}", e);
        panic!("error while running tauri application: {}", e);
    }
}

/// Server mode: no Tauri window, just Axum HTTP server in foreground.
/// Accessible via browser at http://host:port/
pub fn run_server(port_override: Option<u16>, host_override: Option<String>) {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    rt.block_on(async {
        let mut app_config = load_config().expect("failed to load config");
        if let Some(p) = port_override { app_config.port = p; }
        // Server mode defaults to 0.0.0.0 unless explicitly overridden
        if host_override.is_some() {
            app_config.host = host_override.unwrap();
        } else {
            app_config.host = "0.0.0.0".to_string();
        }

        let state = config::AppState::new(app_config).await.expect("failed to create state");
        let (host, port) = axum_server::start(state).await;

        println!("========================================");
        println!("AginxBrain v{}", env!("CARGO_PKG_VERSION"));
        println!("Listening on {}:{}", host, port);
        println!("Web UI: http://{}:{}/", host, port);
        println!("========================================");

        // Block forever (Ctrl+C to stop)
        tokio::signal::ctrl_c().await.expect("failed to listen for ctrl+c");
        log::info!("[Server] shutting down");
    });
}

/// On startup, rewrite takeover configs if they were active but point to a
/// different port than the one the server just bound to.
fn refresh_takeover_configs(port: u16) {
    // Claude Code takeover
    let claude_status = check_takeover_status(port);
    if claude_status.active {
        log::info!("[Startup] Claude Code takeover active on correct port {}", port);
    } else {
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

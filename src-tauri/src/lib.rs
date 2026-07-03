mod api;
mod axum_server;
mod config;
mod convert;
mod dashscope_ws;
mod db;
mod models;
mod proxy;
mod smart_routing;
mod takeover;

#[cfg(feature = "desktop")]
mod tray;

use config::load_config;

/// Desktop mode: thin client. No local proxy server — the window loads the
/// bundled minimal client frontend directly, and config files are written via
/// Tauri commands to point Claude Code / Codex at a remote AginxBrain server.
#[cfg(feature = "desktop")]
pub fn run_desktop() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            takeover_claude,
            restore_claude,
            takeover_codex,
            restore_codex,
            get_takeover_state,
        ])
        .setup(move |app| {
            // Create main window. The frontend (web-client) is bundled via
            // tauri.conf.json `frontendDist` and served over the custom
            // protocol — no local HTTP server needed.
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("AginxBrain")
            .inner_size(820.0, 680.0)
            .min_inner_size(520.0, 420.0)
            .center()
            .build()?;

            // Close-to-tray
            let window_clone = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_clone.hide();
                }
            });

            // System tray
            tray::setup_tray(app)?;

            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        log::error!("[Tauri] fatal error: {}", e);
        eprintln!("Fatal error: {}", e);
        std::process::exit(1);
    }
}

// ─── Tauri commands (desktop client → remote AginxBrain) ────────────────────
//
// Command names match what the web-client frontend invokes (`invoke('takeover_claude')`).
// `rename_all = "camelCase"` lets Rust `api_key` bind to the JS `apiKey` argument.

#[cfg(feature = "desktop")]
#[derive(serde::Serialize)]
struct TakeoverStateCmd {
    claude: bool,
    codex: bool,
}

/// Toggle Claude Code on: write ~/.claude/settings.json to route through `server`.
#[cfg(feature = "desktop")]
#[tauri::command(rename_all = "camelCase")]
fn takeover_claude(server: String, api_key: String) -> Result<bool, String> {
    takeover::take_over_claude_remote(&server, &api_key).map_err(|e| e.to_string())
}

/// Toggle Claude Code off: restore the backed-up settings.json.
#[cfg(feature = "desktop")]
#[tauri::command]
fn restore_claude() -> Result<bool, String> {
    takeover::restore_claude().map_err(|e| e.to_string())?;
    Ok(false)
}

/// Toggle Codex on: write ~/.codex/config.toml to route through `server`.
#[cfg(feature = "desktop")]
#[tauri::command(rename_all = "camelCase")]
fn takeover_codex(server: String, api_key: String) -> Result<bool, String> {
    takeover::take_over_codex_remote(&server, &api_key).map_err(|e| e.to_string())
}

/// Toggle Codex off: restore the backed-up config.
#[cfg(feature = "desktop")]
#[tauri::command]
fn restore_codex() -> Result<bool, String> {
    takeover::restore_codex().map_err(|e| e.to_string())?;
    Ok(false)
}

/// Return current takeover state by inspecting the local config files.
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_takeover_state() -> TakeoverStateCmd {
    TakeoverStateCmd {
        claude: takeover::check_claude_remote_active(),
        codex: takeover::check_codex_remote_active(),
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
        if let Some(h) = host_override {
            app_config.host = h;
        } else {
            app_config.host = "0.0.0.0".to_string();
        }

        let state = config::AppState::new(app_config).await.expect("failed to create state");
        config::spawn_config_watcher(state.config.clone());
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

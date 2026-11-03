mod api;
mod axum_server;
mod config;
mod convert;
mod dashscope_ws;
mod db;
mod failure_class;
mod models;
mod proxy;
mod smart_routing;
mod takeover;

#[cfg(feature = "desktop")]
mod tray;

use config::load_config;
#[cfg(feature = "desktop")]
use tauri::Manager;

/// Desktop mode: local gateway + thin client. On launch we start the full
/// proxy engine on the configured host:port (default 127.0.0.1:8083; set
/// `host: 0.0.0.0` in ~/.aginxbrain/config.yaml to share on the LAN) — the
/// admin UI lives at http://<host>:<port>/. The window keeps the remote-client
/// flow (point Claude/Codex at any AginxBrain server, including this one via
/// server=http://127.0.0.1:<port>).
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
            takeover_claude_desktop,
            restore_claude_desktop,
            get_takeover_state,
            gateway_status,
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

            // Start the local gateway on Tauri's own tokio runtime (no second
            // runtime, no block_on). Thin-client remote mode keeps working even
            // if this fails — the status card in the window shows the error.
            let status = std::sync::Arc::new(std::sync::Mutex::new(GatewayStatus::default()));
            app.manage(status.clone());
            tauri::async_runtime::spawn(async move {
                let s = match bootstrap_gateway().await {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("[Gateway] failed to start local gateway: {e:#}");
                        GatewayStatus {
                            running: false,
                            lan_ip: detect_lan_ip(),
                            error: Some(format!("{e:#}")),
                            ..Default::default()
                        }
                    }
                };
                if let Ok(mut guard) = status.lock() {
                    *guard = s;
                }
            });

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

// ─── Local gateway (desktop) ─────────────────────────────────────────────────

#[cfg(feature = "desktop")]
#[derive(serde::Serialize, Clone, Default)]
pub struct GatewayStatus {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub lan_ip: Option<String>,
    pub error: Option<String>,
}

#[cfg(feature = "desktop")]
async fn bootstrap_gateway() -> anyhow::Result<GatewayStatus> {
    let t0 = std::time::Instant::now();
    let app_config = load_config()?;
    let state = config::AppState::new(app_config).await?;
    config::spawn_config_watcher(state.config.clone());
    let (host, port) = axum_server::start(state).await;

    // axum_server::start swallows bind errors (logs, returns anyway), so probe
    // the listener to tell "running" from "port in use / permission denied".
    let probe_host = if host == "0.0.0.0" || host.is_empty() {
        "127.0.0.1".to_string()
    } else {
        host.clone()
    };
    let running = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect((probe_host.as_str(), port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    let error = (!running).then(|| {
        format!("本地网关未在 {}:{} 监听（端口被占用或无权限？）", host, port)
    });
    if running {
        log::info!(
            "[Gateway] local gateway up on {}:{} in {:?} (admin UI: http://{}:{}/)",
            host, port, t0.elapsed(), probe_host, port
        );
    }
    Ok(GatewayStatus { running, host, port, lan_ip: detect_lan_ip(), error })
}

/// LAN IP without extra deps: a UDP connect picks the default-route interface
/// without sending any packet. None when offline.
#[cfg(feature = "desktop")]
fn detect_lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() => Some(ip.to_string()),
        _ => None,
    }
}

#[cfg(feature = "desktop")]
#[tauri::command]
fn gateway_status(
    status: tauri::State<'_, std::sync::Arc<std::sync::Mutex<GatewayStatus>>>,
) -> GatewayStatus {
    status.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[cfg(feature = "desktop")]
#[derive(serde::Serialize)]
struct TakeoverStateCmd {
    claude: bool,
    codex: bool,
    claude_desktop: bool,
}

/// Toggle Claude Code on: write ~/.claude/settings.json to route through `server`.
#[cfg(feature = "desktop")]
#[tauri::command(rename_all = "camelCase")]
fn takeover_claude(server: String, api_key: String) -> Result<bool, String> {
    takeover::take_over_claude_remote(&server, &api_key).map_err(|e| format!("{e:#}"))
}

/// Toggle Claude Code off: restore the backed-up settings.json.
#[cfg(feature = "desktop")]
#[tauri::command]
fn restore_claude() -> Result<bool, String> {
    takeover::restore_claude().map_err(|e| format!("{e:#}"))?;
    Ok(false)
}

/// Toggle Codex on: write ~/.codex/config.toml to route through `server`.
#[cfg(feature = "desktop")]
#[tauri::command(rename_all = "camelCase")]
fn takeover_codex(server: String, api_key: String) -> Result<bool, String> {
    takeover::take_over_codex_remote(&server, &api_key).map_err(|e| format!("{e:#}"))
}

/// Toggle Codex off: restore the backed-up config.
#[cfg(feature = "desktop")]
#[tauri::command]
fn restore_codex() -> Result<bool, String> {
    takeover::restore_codex().map_err(|e| format!("{e:#}"))?;
    Ok(false)
}

/// Toggle Claude Desktop (the app's Code tab) on: write its third-party
/// inference config to route through `server`. Takes effect at next app
/// launch — restart the Claude app if it's running.
#[cfg(feature = "desktop")]
#[tauri::command(rename_all = "camelCase")]
fn takeover_claude_desktop(server: String, api_key: String) -> Result<bool, String> {
    takeover::take_over_claude_desktop_remote(&server, &api_key).map_err(|e| format!("{e:#}"))
}

/// Toggle Claude Desktop off: restore the backed-up 3p config.
#[cfg(feature = "desktop")]
#[tauri::command]
fn restore_claude_desktop() -> Result<bool, String> {
    takeover::restore_claude_desktop_remote().map_err(|e| format!("{e:#}"))?;
    Ok(false)
}

/// Return current takeover state by inspecting the local config files.
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_takeover_state() -> TakeoverStateCmd {
    TakeoverStateCmd {
        claude: takeover::check_claude_remote_active(),
        codex: takeover::check_codex_remote_active(),
        claude_desktop: takeover::check_claude_desktop_remote_active(),
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

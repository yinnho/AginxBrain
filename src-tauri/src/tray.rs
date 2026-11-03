use tauri::{
    App, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
};

pub fn setup_tray(app: &App) -> Result<(), Box<dyn std::error::Error>> {
    let show_item = MenuItemBuilder::with_id("show", "显示 AginxBrain").build(app)?;
    let admin_item = MenuItemBuilder::with_id("admin", "打开管理界面").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&show_item, &admin_item, &quit_item])
        .build()?;

    let mut tray_builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("AginxBrain")
        .menu(&menu);

    if let Some(icon) = app.default_window_icon() {
        tray_builder = tray_builder.icon(icon.clone());
    } else {
        log::warn!("[Tray] no default window icon configured");
    }

    tray_builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "admin" => {
                // Open the embedded admin dashboard in the system browser.
                // Rust-side shell open needs no capability grant.
                let port = app
                    .try_state::<std::sync::Arc<std::sync::Mutex<crate::GatewayStatus>>>()
                    .and_then(|s| s.lock().ok().map(|g| g.port))
                    .filter(|p| *p > 0)
                    .unwrap_or(8083);
                use tauri_plugin_shell::ShellExt;
                // shell::open is deprecated in favor of tauri-plugin-opener;
                // the shell plugin is already wired on both sides — migrate
                // together with the next plugin round.
                #[allow(deprecated)]
                let _ = app.shell().open(format!("http://127.0.0.1:{}", port), None);
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

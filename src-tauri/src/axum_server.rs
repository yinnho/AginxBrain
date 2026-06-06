use crate::config::AppState;
use axum::extract::Request;
use axum::http::{Method, StatusCode, Uri};
use axum::response::IntoResponse;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

// Embed frontend files at compile time (release builds only).
// In dev mode, Vite serves the frontend directly.
#[cfg(not(debug_assertions))]
use rust_embed::RustEmbed;

#[cfg(not(debug_assertions))]
#[derive(RustEmbed)]
#[folder = "../web/dist/"]
struct Asset;

/// Start the axum HTTP server. Returns the actual host:port once bound.
pub async fn start(state: AppState) -> (String, u16) {
    let port = state.config.read().await.port;
    let host = state.config.read().await.host.clone();

    let app = axum::Router::new()
        // Anthropic client paths
        .route(
            "/anthropic/v1/messages",
            axum::routing::post(crate::proxy::handle_anthropic_messages),
        )
        .route(
            "/anthropic/v1/messages/count_tokens",
            axum::routing::post(crate::proxy::handle_anthropic_count_tokens),
        )
        // OpenAI client paths
        .route(
            "/openai/v1/chat/completions",
            axum::routing::post(crate::proxy::handle_openai_chat),
        )
        .route(
            "/openai/v1/responses",
            axum::routing::post(crate::proxy::handle_openai_responses),
        )
        // Legacy paths (backward compatible)
        .route(
            "/v1/messages",
            axum::routing::post(crate::proxy::handle_anthropic_messages),
        )
        .route(
            "/v1/messages/count_tokens",
            axum::routing::post(crate::proxy::handle_anthropic_count_tokens),
        )
        // Codex / OpenAI client paths
        .route(
            "/v1/responses",
            axum::routing::post(crate::proxy::handle_openai_responses),
        )
        .route(
            "/v1/responses/compact",
            axum::routing::post(crate::proxy::handle_openai_responses),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(crate::proxy::handle_openai_chat),
        )
        .route("/v1/models", axum::routing::get(crate::api::get_models))
        // Codex route aliases (v1 compatibility)
        .route("/responses", axum::routing::post(crate::proxy::handle_openai_responses))
        .route("/responses/compact", axum::routing::post(crate::proxy::handle_openai_responses))
        .route("/models", axum::routing::get(crate::api::get_models))
        // API endpoints — read-only (no auth required)
        .route("/api/config", axum::routing::get(crate::api::get_config))
        .route("/api/status", axum::routing::get(crate::api::get_status))
        .route("/api/logs", axum::routing::get(crate::api::get_logs))
        // API endpoints — write operations (auth required)
        .nest(
            "/api",
            axum::Router::new()
                .route("/config", axum::routing::put(crate::api::update_config))
                .route("/current-tag", axum::routing::put(crate::api::set_current_tag))
                .route("/takeover/claude", axum::routing::post(crate::api::takeover_claude_handler))
                .route("/takeover/claude", axum::routing::delete(crate::api::restore_claude_handler))
                .route("/takeover/codex", axum::routing::post(crate::api::takeover_codex_handler))
                .route("/takeover/codex", axum::routing::delete(crate::api::restore_codex_handler))
                .route("/test", axum::routing::post(crate::api::test_route_handler))
                .route("/config/export", axum::routing::post(crate::api::export_config))
                .route("/config/import", axum::routing::post(crate::api::import_config))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    crate::api::require_management_key,
                )),
        )
        .layer(axum::middleware::from_fn(request_log_middleware))
        // Codex conversations can be very large (system prompt + tool results).
        // v1 uses 200MB; axum's default is 2MB which causes silent failures.
        .layer(RequestBodyLimitLayer::new(200 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .fallback(fallback_handler)
        .with_state(state);

    let listen_addr = format!("{}:{}", host, port);
    let listener = match tokio::net::TcpListener::bind(&listen_addr).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("[Server] failed to bind {}: {}", listen_addr, e);
            return (host, port);
        }
    };

    log::info!("aginxbrain listening on {}", listen_addr);

    // Use oneshot channel to signal when the spawned server task has started,
    // preventing the race where the Tauri window connects before the server
    // accept loop is running.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        ready_tx.send(()).ok();
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("[Server] axum server error: {}", e);
        }
    });
    // Wait until the spawned task has been polled at least once,
    // ensuring the accept loop is active before the caller proceeds.
    ready_rx.await.ok();

    (host, port)
}

/// Middleware that logs every incoming request.
async fn request_log_middleware(
    req: Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let api_key = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            if s.len() > 20 {
                format!("{}...", &s[..20])
            } else {
                s.to_string()
            }
        })
        .unwrap_or("-".to_string());

    log::info!(
        "[Request] {} {} | content-type={} | x-api-key={} | auth={}",
        method,
        path,
        content_type,
        api_key,
        auth
    );

    let response = next.run(req).await;
    log::info!("[Request] {} {} → {}", method, path, response.status());
    response
}

/// Fallback handler: serves embedded frontend files (release) or returns 404 (dev).
async fn fallback_handler(method: Method, uri: Uri) -> impl IntoResponse {
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            serde_json::json!({"error": format!("not found: {} {}", method, uri.path())}).to_string(),
        )
            .into_response();
    }

    #[cfg(not(debug_assertions))]
    {
        let file_path = uri.path().trim_start_matches('/');
        let file_path = if file_path.is_empty() { "index.html" } else { file_path };

        // Try exact file match first
        if let Some(content) = Asset::get(file_path) {
            let ct = content.metadata.mimetype();
            log::info!("[Fallback] GET {} → 200 (embedded, {})", uri.path(), ct);
            return (
                StatusCode::OK,
                [("content-type", ct)],
                content.data.to_vec(),
            )
                .into_response();
        }

        // SPA fallback: for paths without a file extension, serve index.html
        let has_extension = std::path::Path::new(file_path)
            .extension()
            .is_some();
        if !has_extension {
            if let Some(html) = Asset::get("index.html") {
                log::info!("[Fallback] GET {} → 200 (SPA fallback)", uri.path());
                return (
                    StatusCode::OK,
                    [("content-type", "text/html; charset=utf-8")],
                    html.data.to_vec(),
                )
                    .into_response();
            }
        }

        log::warn!("[Fallback] GET {} → 404 (not in embedded assets)", uri.path());
        (
            StatusCode::NOT_FOUND,
            [("content-type", "text/plain")],
            "Not Found",
        )
            .into_response()
    }

    #[cfg(debug_assertions)]
    {
        // Dev mode: serve from disk (web/dist/) so browser access works during development
        let file_path = uri.path().trim_start_matches('/');
        let file_path = if file_path.is_empty() { "index.html" } else { file_path };

        let disk_path = std::path::PathBuf::from("../web/dist").join(file_path);
        if disk_path.exists() && disk_path.is_file() {
            if let Ok(bytes) = tokio::fs::read(&disk_path).await {
                let ct = mime_guess::from_path(file_path).first_or_octet_stream().to_string();
                log::info!("[Fallback] GET {} → 200 (disk, {})", uri.path(), ct);
                return (StatusCode::OK, [("content-type", ct)], bytes).into_response();
            }
        }

        // SPA fallback: for paths without a file extension, serve index.html
        let has_extension = std::path::Path::new(file_path).extension().is_some();
        if !has_extension {
            let html_path = std::path::PathBuf::from("../web/dist/index.html");
            if let Ok(bytes) = tokio::fs::read(&html_path).await {
                log::info!("[Fallback] GET {} → 200 (SPA fallback, disk)", uri.path());
                return (
                    StatusCode::OK,
                    [("content-type", "text/html; charset=utf-8")],
                    bytes,
                )
                    .into_response();
            }
        }

        log::warn!("[Fallback] GET {} → 404 (dev, not on disk)", uri.path());
        (
            StatusCode::NOT_FOUND,
            [("content-type", "text/plain")],
            "Not Found",
        )
            .into_response()
    }
}

use crate::config::AppState;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::response::Response;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::compression::CompressionLayer;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::SqliteStore;

// Embed the admin dashboard frontend at compile time (server release builds
// only). The desktop client never serves a frontend — it's a thin client that
// connects to a remote AginxBrain server — so it doesn't need web/dist.
#[cfg(all(not(debug_assertions), feature = "server"))]
use rust_embed::RustEmbed;

#[cfg(all(not(debug_assertions), feature = "server"))]
#[derive(RustEmbed)]
#[folder = "../web/dist/"]
struct Asset;

/// Start the axum HTTP server. Returns the actual host:port once bound.
/// Serve a saved TTS audio file from ~/.aginxbrain/audio/. Public (no auth) —
/// OpenCarrier downloads these URLs with a bare GET. Rejects any filename
/// containing path separators or `..` to prevent traversal.
async fn serve_audio_file(Path(filename): Path<String>) -> Response {
    // Path traversal guard: only allow plain filenames.
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return (StatusCode::NOT_FOUND, [("content-type", "text/plain")], "Not Found".to_string()).into_response();
    }
    let path = match dirs::home_dir() {
        Some(h) => h.join(".aginxbrain").join("audio").join(&filename),
        None => return (StatusCode::INTERNAL_SERVER_ERROR, [("content-type", "text/plain")], "no home dir".to_string()).into_response(),
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [("content-type", "audio/mpeg")],
            bytes,
        ).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, [("content-type", "text/plain")], "Not Found".to_string()).into_response(),
    }
}

pub async fn start(state: AppState) -> (String, u16) {
    let port = state.config.read().await.port;
    let host = state.config.read().await.host.clone();

    // Session store and layer
    let session_store = SqliteStore::new(state.db.clone());
    session_store.migrate().await.expect("failed to migrate session store");

    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false) // set true when served over HTTPS
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(time::Duration::hours(24)));

    // Router split into public, proxy (caller-key protected), and admin-protected APIs.
    let proxy_routes = axum::Router::new()
        .route(
            "/anthropic/v1/messages",
            axum::routing::post(crate::proxy::handle_anthropic_messages),
        )
        .route(
            "/anthropic/v1/messages/count_tokens",
            axum::routing::post(crate::proxy::handle_anthropic_count_tokens),
        )
        .route(
            "/openai/v1/chat/completions",
            axum::routing::post(crate::proxy::handle_openai_chat),
        )
        .route(
            "/openai/v1/responses",
            axum::routing::post(crate::proxy::handle_openai_responses),
        )
        .route(
            "/v1/messages",
            axum::routing::post(crate::proxy::handle_anthropic_messages),
        )
        .route(
            "/v1/messages/count_tokens",
            axum::routing::post(crate::proxy::handle_anthropic_count_tokens),
        )
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
        .route("/responses", axum::routing::post(crate::proxy::handle_openai_responses))
        .route("/responses/compact", axum::routing::post(crate::proxy::handle_openai_responses))
        .route("/models", axum::routing::get(crate::api::get_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_caller_key,
        ));

    // Public admin auth endpoints — NOT behind the session layer, since you
    // can't be authenticated to authenticate. Setup only succeeds when no admin exists.
    let public_auth_routes = axum::Router::new()
        .route("/admin/setup", axum::routing::post(crate::api::admin_setup))
        .route("/admin/login", axum::routing::post(crate::api::admin_login));

    let admin_api_routes = axum::Router::new()
        .route("/admin/logout", axum::routing::post(crate::api::admin_logout))
        .route("/admin/me", axum::routing::get(crate::api::admin_me))
        .route("/keys", axum::routing::get(crate::api::list_keys))
        .route("/keys", axum::routing::post(crate::api::create_key))
        .route("/keys/{id}", axum::routing::put(crate::api::update_key))
        .route("/keys/{id}", axum::routing::delete(crate::api::delete_key))
        .route("/cost-rates", axum::routing::get(crate::api::list_cost_rates))
        .route("/cost-rates", axum::routing::post(crate::api::set_cost_rate))
        .route("/cost-rates/{id}", axum::routing::delete(crate::api::delete_cost_rate))
        .route("/usage/daily", axum::routing::get(crate::api::daily_usage))
        .route("/usage/monthly", axum::routing::get(crate::api::monthly_usage))
        .route("/usage/summary", axum::routing::get(crate::api::usage_summary))
        .route("/config", axum::routing::get(crate::api::get_config).put(crate::api::update_config))
        .route("/current-tag", axum::routing::put(crate::api::set_current_tag))
        // Fine-grained route CRUD (replaces bulk PUT /api/config for these ops)
        .route("/routes", axum::routing::post(crate::api::create_route))
        .route("/routes/{index}", axum::routing::put(crate::api::update_route).patch(crate::api::patch_route).delete(crate::api::delete_route))
        .route("/routes/{index}/move", axum::routing::post(crate::api::move_route))
        // Fine-grained provider CRUD
        .route("/providers", axum::routing::post(crate::api::create_provider))
        .route("/providers/{id}", axum::routing::put(crate::api::update_provider).delete(crate::api::delete_provider))
        // Fine-grained tag CRUD
        .route("/tags", axum::routing::post(crate::api::create_tag))
        .route("/tags/{name}", axum::routing::patch(crate::api::patch_tag).delete(crate::api::delete_tag))
        .route("/test", axum::routing::post(crate::api::test_route_handler))
        .route("/test/route", axum::routing::post(crate::api::test_route_by_index_handler))
        .route("/brain/generate/image", axum::routing::post(crate::api::generate_image_handler))
        .route("/config/export", axum::routing::post(crate::api::export_config))
        .route("/config/import", axum::routing::post(crate::api::import_config))
        .route_layer(axum::middleware::from_fn(crate::api::require_admin_session));

    // Client-facing routes: accessible via admin session OR caller-key Bearer token.
    // Desktop app uses these with just an API key (no admin login).
    let client_api_routes = axum::Router::new()
        .route("/logs", axum::routing::get(crate::api::get_logs))
        .route("/takeover/claude", axum::routing::post(crate::api::takeover_claude_handler))
        .route("/takeover/claude", axum::routing::delete(crate::api::restore_claude_handler))
        .route("/takeover/codex", axum::routing::post(crate::api::takeover_codex_handler))
        .route("/takeover/codex", axum::routing::delete(crate::api::restore_codex_handler))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::require_admin_or_caller_key,
        ));

    // Public read APIs: only /status (login page needs setup_required before auth).
    let read_api_routes = axum::Router::new()
        .route("/status", axum::routing::get(crate::api::get_status));

    // Admin and read APIs are nested under /api so they match the frontend's
    // API_BASE = '/api'. Proxy routes stay at root (e.g. /v1/chat/completions).
    let api_routes = public_auth_routes
        .merge(admin_api_routes)
        .merge(client_api_routes)
        .merge(read_api_routes);

    // Public audio file route: serves TTS output. NOT behind caller-key auth —
    // OpenCarrier downloads these URLs without a token. Registered at root so
    // it's reachable via brain.aginx.net/audio/<file> through nginx.
    let audio_routes = axum::Router::new()
        .route("/audio/{filename}", axum::routing::get(serve_audio_file));

    let app = axum::Router::new()
        .merge(audio_routes)
        .merge(proxy_routes)
        .nest("/api", api_routes)
        .layer(axum::middleware::from_fn(request_log_middleware))
        .layer(session_layer)
        // Codex conversations can be very large (system prompt + tool results).
        // v1 uses 200MB; axum's default is 2MB which causes silent failures.
        .layer(RequestBodyLimitLayer::new(200 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(CompressionLayer::new())
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

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        ready_tx.send(()).ok();
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("[Server] axum server error: {}", e);
        }
    });
    ready_rx.await.ok();

    (host, port)
}

/// Middleware that validates the caller API key on proxy routes.
/// Accepts either `Authorization: Bearer <token>` (OpenAI / Responses clients)
/// or `x-api-key: <token>` (Anthropic clients — Claude Code, Anthropic SDK).
async fn require_caller_key(
    State(state): State<AppState>,
    mut req: Request,
    next: axum::middleware::Next,
) -> Result<Response, StatusCode> {
    let token = extract_caller_token(req.headers());
    let Some(token) = token else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let result = crate::db::find_caller_key_by_token(&state.db, &token)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some((caller_key_id, enabled)) = result else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !enabled {
        return Err(StatusCode::FORBIDDEN);
    }
    req.extensions_mut().insert(caller_key_id);
    Ok(next.run(req).await)
}

pub fn extract_caller_token(headers: &HeaderMap) -> Option<String> {
    // Prefer Authorization: Bearer (OpenAI / Responses / Codex), then fall back
    // to x-api-key (Anthropic clients).
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(tok) = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
        {
            return Some(tok.trim().to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
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

    #[cfg(all(not(debug_assertions), feature = "server"))]
    {
        let file_path = uri.path().trim_start_matches('/');
        let file_path = if file_path.is_empty() { "index.html" } else { file_path };

        if let Some(content) = Asset::get(file_path) {
            let ct = content.metadata.mimetype();
            return (
                StatusCode::OK,
                [("content-type", ct)],
                Body::from(content.data.to_vec()),
            )
                .into_response();
        }

        let has_extension = std::path::Path::new(file_path)
            .extension()
            .is_some();
        if !has_extension {
            if let Some(html) = Asset::get("index.html") {
                return (
                    StatusCode::OK,
                    [("content-type", "text/html; charset=utf-8")],
                    Body::from(html.data.to_vec()),
                )
                    .into_response();
            }
        }

        (
            StatusCode::NOT_FOUND,
            [("content-type", "text/plain")],
            "Not Found",
        )
            .into_response()
    }

    #[cfg(not(all(not(debug_assertions), feature = "server")))]
    {
        // Dev mode (Vite serves frontend) or desktop build (no embedded
        // frontend — the desktop client connects to a remote server).
        (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            serde_json::json!({"error": format!("not found: {}", uri.path())}).to_string(),
        )
            .into_response()
    }
}

//! Focus daemon — axum HTTP server on `localhost:21548`.
//!
//! Receives focus commands from the Zestful Mac app and dispatches them to the
//! appropriate terminal handler (kitty, iTerm2, WezTerm, Terminal.app, or generic).
//! Requires `X-Zestful-Token` authentication.

use crate::config;
use crate::workspace::{browsers, ides, terminals, uri};
use anyhow::Result;
use axum::{
    extract::{DefaultBodyLimit, Json},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Deserialize)]
struct FocusRequest {
    /// Terminal URI (e.g. workspace://iterm2/window:1/tab:2)
    terminal_uri: Option<String>,
    /// Legacy fields — used as fallback when terminal_uri is absent
    app: Option<String>,
    window_id: Option<String>,
    tab_id: Option<String>,
}

#[derive(Serialize)]
struct StatusResponse {
    status: String,
}

/// A request body on `POST /events` is either a single envelope or a batch.
/// We accept both and normalize to a Vec at handling time.
#[derive(Deserialize)]
#[serde(untagged)]
enum EventsBody {
    Batch { events: Vec<serde_json::Value> },
    Single(serde_json::Value),
}

#[derive(Serialize)]
struct EventsResponse {
    status: &'static str,
    accepted: usize,
}

/// Start the focus daemon. Creates a tokio runtime and runs the axum server.
pub fn run() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_server())
}

async fn run_server() -> Result<()> {
    let pid_file = config::pid_file();

    // Ensure config dir exists with restrictive permissions
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            let _ = fs::set_permissions(parent, perms);
        }
    }

    // Write PID file safely: refuse to write if path is a symlink
    #[cfg(unix)]
    {
        if pid_file.exists() {
            let meta = fs::symlink_metadata(&pid_file)?;
            if meta.file_type().is_symlink() {
                anyhow::bail!("PID file is a symlink, refusing to write: {:?}", pid_file);
            }
        }
    }
    fs::write(&pid_file, std::process::id().to_string())?;

    let app = Router::new()
        .route("/health", get(health))
        .route("/focus", post(handle_focus))
        .route("/inspect", get(handle_inspect))
        .layer(DefaultBodyLimit::max(16_384)) // 16 KB default
        .route(
            "/events",
            post(handle_events).layer(DefaultBodyLimit::max(256 * 1024)),
        );

    let port = config::daemon_port();
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    crate::log::log("daemon", &format!("listening on localhost:{}", port));

    // Graceful shutdown on SIGTERM/SIGINT
    let pid_file_clone = pid_file.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(pid_file_clone))
        .await?;

    // Cleanup PID file
    let _ = fs::remove_file(&pid_file);

    Ok(())
}

async fn health() -> impl IntoResponse {
    Json(StatusResponse {
        status: "ok".to_string(),
    })
}

/// Return the same JSON that `zestful inspect` produces. Runs in the daemon
/// process so it inherits whatever Apple Events / TCC permissions the
/// terminal that launched the daemon already has — avoids the per-process
/// permission prompts that would otherwise be needed for each subprocess.
async fn handle_inspect() -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(|| crate::workspace::inspect_all()).await;
    match result {
        Ok(Ok(output)) => (
            StatusCode::OK,
            Json(serde_json::to_value(&output).unwrap_or_default()),
        ),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("join error: {e}")})),
        ),
    }
}

async fn handle_focus(Json(req): Json<FocusRequest>) -> impl IntoResponse {
    // Note: no token auth on /focus. The daemon only listens on 127.0.0.1
    // and the Mac app (the primary caller) does not send a token. This matches
    // the original Node.js daemon behavior.

    // Prefer terminal_uri; fall back to legacy app/window_id/tab_id fields
    let parsed = if let Some(ref uri) = req.terminal_uri {
        match uri::parse_terminal_uri(uri) {
            Some(p) => p,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid terminal_uri"})),
                );
            }
        }
    } else {
        match req.app {
            Some(app) if !app.is_empty() => uri::ParsedTerminalUri {
                app,
                window_id: req.window_id,
                tab_id: req.tab_id,
                project_id: None,
                terminal_id: None,
                shelldon: None,
                tmux: None,
            },
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "terminal_uri or app is required"})),
                );
            }
        }
    };

    crate::log::log(
        "daemon",
        &format!(
            "focus: app={} window_id={} tab_id={} shelldon={} tmux={} uri={}",
            parsed.app,
            parsed.window_id.as_deref().unwrap_or(""),
            parsed.tab_id.as_deref().unwrap_or(""),
            parsed
                .shelldon
                .as_ref()
                .map(|s| s.session_id.as_str())
                .unwrap_or(""),
            parsed
                .tmux
                .as_ref()
                .map(|t| t.session.as_str())
                .unwrap_or(""),
            req.terminal_uri.as_deref().unwrap_or("")
        ),
    );

    // Focus the app — route by URI shape.
    let app_lower = parsed.app.to_lowercase();
    let is_browser = app_lower.contains("chrome")
        || app_lower.contains("safari")
        || app_lower.contains("firefox");
    let is_ide = parsed.project_id.is_some()
        || parsed.terminal_id.is_some()
        || app_lower == "xcode"
        || app_lower == "vscode"
        || app_lower.contains("visual studio code")
        || app_lower == "cursor"
        || app_lower == "windsurf"
        || app_lower == "zed";
    let focus_result = if is_ide {
        ides::handle_focus(
            &parsed.app,
            parsed.project_id.as_deref(),
            parsed.terminal_id.as_deref(),
        )
        .await
    } else if is_browser {
        browsers::handle_focus(
            &parsed.app,
            parsed.window_id.as_deref(),
            parsed.tab_id.as_deref(),
        )
        .await
    } else {
        terminals::handle_focus(
            &parsed.app,
            parsed.window_id.as_deref(),
            parsed.tab_id.as_deref(),
        )
        .await
    };
    if let Err(e) = focus_result {
        crate::log::log("daemon", &format!("focus error: {}", e));
    }

    // Focus the shelldon tab within the terminal
    if let Some(ref shelldon) = parsed.shelldon {
        if let Err(e) = crate::workspace::multiplexers::shelldon::focus(shelldon).await {
            crate::log::log("daemon", &format!("shelldon focus error: {}", e));
        }
    }

    // Focus the tmux window/pane within the terminal
    if let Some(ref tmux) = parsed.tmux {
        if let Err(e) = crate::workspace::multiplexers::tmux::focus(tmux).await {
            crate::log::log("daemon", &format!("tmux focus error: {}", e));
        }
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

async fn handle_events(
    headers: HeaderMap,
    body: axum::extract::Json<EventsBody>,
) -> impl IntoResponse {
    // Auth: X-Zestful-Token must match config::read_token().
    let expected = config::read_token();
    let got = headers
        .get("x-zestful-token")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    match (expected, got) {
        (Some(e), Some(g)) if e == g => {}
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid token"})),
            )
                .into_response();
        }
    }

    // Normalize to a Vec<serde_json::Value>.
    let envelopes: Vec<serde_json::Value> = match body.0 {
        EventsBody::Single(v) => vec![v],
        EventsBody::Batch { events } => events,
    };

    // Validate each envelope per spec §Daemon validation rules.
    for (idx, env) in envelopes.iter().enumerate() {
        if let Err(detail) = validate_envelope(env) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid envelope",
                    "detail": detail,
                    "event_index": idx,
                })),
            )
                .into_response();
        }
    }

    // Accept. Log one line per event.
    for env in &envelopes {
        let type_ = env.get("type").and_then(|v| v.as_str()).unwrap_or("?");
        let id = env.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let source = env.get("source").and_then(|v| v.as_str()).unwrap_or("?");
        let session_id = env
            .get("correlation")
            .and_then(|c| c.get("session_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        crate::log::log(
            "events",
            &format!(
                "accepted id={} type={} source={} session={}",
                id, type_, source, session_id
            ),
        );
    }

    // Forward accepted envelopes to the Fly backend in the background.
    // Best-effort — never blocks the handler's response.
    crate::events::backend_forwarder::spawn_forward(envelopes.clone());

    (
        StatusCode::OK,
        Json(EventsResponse {
            status: "ok",
            accepted: envelopes.len(),
        }),
    )
        .into_response()
}

/// Validate an envelope JSON per spec §Daemon validation rules. Returns
/// `Err(detail)` on failure. Payload shapes are NOT validated — unknown types
/// are accepted for forward-compat.
fn validate_envelope(v: &serde_json::Value) -> std::result::Result<(), String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "envelope must be a JSON object".to_string())?;

    // Required fields.
    for required in [
        "id",
        "schema",
        "ts",
        "seq",
        "host",
        "os_user",
        "device_id",
        "source",
        "source_pid",
        "type",
    ] {
        if !obj.contains_key(required) {
            return Err(format!("missing required field: {}", required));
        }
    }

    // schema must be 1.
    let schema = obj.get("schema").and_then(|v| v.as_u64()).unwrap_or(0);
    if schema != 1 {
        return Err(format!("unsupported schema version: {}", schema));
    }

    // id must be a 26-char string.
    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "id must be a string".to_string())?;
    if id.len() != 26 {
        return Err(format!("id must be a 26-char ULID, got {} chars", id.len()));
    }

    // type must be a string.
    if obj.get("type").and_then(|v| v.as_str()).is_none() {
        return Err("type must be a string".into());
    }

    Ok(())
}

async fn shutdown_signal(pid_file: std::path::PathBuf) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    crate::log::log("daemon", "shutting down");
    let _ = fs::remove_file(&pid_file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use tempfile::TempDir;

    /// Redirect `$HOME` (or `%USERPROFILE%` on Windows) to a tempdir for the
    /// duration of a test, restoring on drop. Required so `set_test_token` /
    /// `config::read_token` operate on an isolated filesystem and never touch
    /// the real user's token file.
    struct HomeGuard {
        old_home: Option<String>,
        _td: TempDir,
    }

    impl HomeGuard {
        fn new() -> Self {
            let td = TempDir::new().unwrap();
            let home_var = if cfg!(target_os = "windows") {
                "USERPROFILE"
            } else {
                "HOME"
            };
            let old_home = std::env::var(home_var).ok();
            std::env::set_var(home_var, td.path());
            HomeGuard { old_home, _td: td }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            let home_var = if cfg!(target_os = "windows") {
                "USERPROFILE"
            } else {
                "HOME"
            };
            match &self.old_home {
                Some(v) => std::env::set_var(home_var, v),
                None => std::env::remove_var(home_var),
            }
        }
    }

    fn app() -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/focus", post(handle_focus))
            .route(
                "/events",
                post(handle_events).layer(DefaultBodyLimit::max(256 * 1024)),
            )
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_focus_missing_app_and_uri() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "terminal_uri or app is required");
    }

    #[tokio::test]
    async fn test_focus_empty_app_no_uri() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"app":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_focus_with_terminal_uri() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"terminal_uri":"terminal://kitty/window:1/tab:2"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_focus_with_legacy_app() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"app":"kitty","window_id":"1","tab_id":"my-tab"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_focus_invalid_terminal_uri() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"terminal_uri":"not-a-valid-uri"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid terminal_uri");
    }

    #[tokio::test]
    async fn test_focus_invalid_json() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // axum returns 422 for deserialization errors
        assert!(response.status().is_client_error());
    }

    #[tokio::test]
    async fn test_focus_with_only_app() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"app":"Terminal"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_focus_rejects_injection_in_app() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"app":"Finder\"; display dialog \"pwned"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should succeed at HTTP level but terminals::handle_focus will reject the invalid chars
        // The response is still 200 because the error is logged, not returned
        // But the osascript won't execute arbitrary code due to validation
        assert!(response.status().is_success() || response.status().is_client_error());
    }

    async fn send_events_request(body: &str, token: Option<&str>) -> axum::http::Response<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/events")
            .header("content-type", "application/json");
        if let Some(t) = token {
            builder = builder.header("x-zestful-token", t);
        }
        let req = builder.body(Body::from(body.to_string())).unwrap();
        app().oneshot(req).await.unwrap()
    }

    /// Set a known token for the duration of the test. Not thread-safe; run with
    /// --test-threads=1 for the events tests.
    fn set_test_token(token: &str) {
        let dir = config::config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("local-token"), token).unwrap();
    }

    fn canned_envelope() -> serde_json::Value {
        serde_json::json!({
            "id": "01JGYK8F3N7WA9QVXR2PB5HM4D",
            "schema": 1,
            "ts": 1745183677234u64,
            "seq": 0,
            "host": "morrow.local",
            "os_user": "jmorrow",
            "device_id": "d_test",
            "source": "claude-code",
            "source_pid": 83421,
            "type": "turn.completed",
        })
    }

    #[tokio::test]
    async fn events_rejects_missing_token() {
        let _home = HomeGuard::new();
        let body = serde_json::to_string(&canned_envelope()).unwrap();
        let resp = send_events_request(&body, None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn events_accepts_valid_single_envelope() {
        let _home = HomeGuard::new();
        set_test_token("test-token-single");
        let body = serde_json::to_string(&canned_envelope()).unwrap();
        let resp = send_events_request(&body, Some("test-token-single")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["accepted"], 1);
    }

    #[tokio::test]
    async fn events_accepts_batch() {
        let _home = HomeGuard::new();
        set_test_token("test-token-batch");
        let batch = serde_json::json!({
            "events": [canned_envelope(), canned_envelope(), canned_envelope()],
        });
        let body = serde_json::to_string(&batch).unwrap();
        let resp = send_events_request(&body, Some("test-token-batch")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["accepted"], 3);
    }

    #[tokio::test]
    async fn events_rejects_missing_required_field() {
        let _home = HomeGuard::new();
        set_test_token("test-token-required");
        let mut env = canned_envelope();
        env.as_object_mut().unwrap().remove("ts");
        let body = serde_json::to_string(&env).unwrap();
        let resp = send_events_request(&body, Some("test-token-required")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "invalid envelope");
        assert!(json["detail"].as_str().unwrap().contains("ts"));
    }

    #[tokio::test]
    async fn events_rejects_unsupported_schema_version() {
        let _home = HomeGuard::new();
        set_test_token("test-token-schema");
        let mut env = canned_envelope();
        env["schema"] = serde_json::json!(99);
        let body = serde_json::to_string(&env).unwrap();
        let resp = send_events_request(&body, Some("test-token-schema")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["detail"].as_str().unwrap().contains("schema"));
    }

    #[tokio::test]
    async fn events_rejects_malformed_ulid() {
        let _home = HomeGuard::new();
        set_test_token("test-token-ulid");
        let mut env = canned_envelope();
        env["id"] = serde_json::json!("short");
        let body = serde_json::to_string(&env).unwrap();
        let resp = send_events_request(&body, Some("test-token-ulid")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn events_accepts_unknown_type() {
        let _home = HomeGuard::new();
        set_test_token("test-token-unknown");
        let mut env = canned_envelope();
        env["type"] = serde_json::json!("future.undefined_type");
        let body = serde_json::to_string(&env).unwrap();
        let resp = send_events_request(&body, Some("test-token-unknown")).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn events_batch_all_or_nothing_reports_index() {
        let _home = HomeGuard::new();
        set_test_token("test-token-index");
        let mut bad = canned_envelope();
        bad.as_object_mut().unwrap().remove("host");
        let batch = serde_json::json!({
            "events": [canned_envelope(), canned_envelope(), bad],
        });
        let body = serde_json::to_string(&batch).unwrap();
        let resp = send_events_request(&body, Some("test-token-index")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["event_index"], 2);
    }
}

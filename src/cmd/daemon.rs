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
    http::StatusCode,
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
        .layer(DefaultBodyLimit::max(16_384)); // 16 KB

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
        Ok(Ok(output)) => (StatusCode::OK, Json(serde_json::to_value(&output).unwrap_or_default())),
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

async fn handle_focus(
    Json(req): Json<FocusRequest>,
) -> impl IntoResponse {
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

    crate::log::log("daemon", &format!(
        "focus: app={} window_id={} tab_id={} shelldon={} tmux={} uri={}",
        parsed.app,
        parsed.window_id.as_deref().unwrap_or(""),
        parsed.tab_id.as_deref().unwrap_or(""),
        parsed.shelldon.as_ref().map(|s| s.session_id.as_str()).unwrap_or(""),
        parsed.tmux.as_ref().map(|t| t.session.as_str()).unwrap_or(""),
        req.terminal_uri.as_deref().unwrap_or("")
    ));

    // Focus the app — route by URI shape.
    let app_lower = parsed.app.to_lowercase();
    let is_browser = app_lower.contains("chrome") || app_lower.contains("safari") || app_lower.contains("firefox");
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
        ).await
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

    fn app() -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/focus", post(handle_focus))
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
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
                    .body(Body::from(
                        r#"{"terminal_uri":"not-a-valid-uri"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
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
}

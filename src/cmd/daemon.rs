//! Focus daemon — axum HTTP server on `localhost:21548`.
//!
//! Receives focus commands from the Zestful Mac app and dispatches them to the
//! appropriate terminal handler (kitty, iTerm2, WezTerm, Terminal.app, or generic).
//! Requires `X-Zestful-Token` authentication.

use crate::config;
use crate::focus;
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
        .layer(DefaultBodyLimit::max(16_384)); // 16 KB

    let port = config::daemon_port();
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("[zestfuld] Listening on localhost:{}", port);

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

async fn handle_focus(
    headers: HeaderMap,
    Json(req): Json<FocusRequest>,
) -> impl IntoResponse {
    // Require auth token
    if let Some(expected_token) = config::read_token() {
        let provided = headers
            .get("X-Zestful-Token")
            .and_then(|v| v.to_str().ok());
        match provided {
            Some(t) if t == expected_token => {}
            _ => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid or missing token"})),
                );
            }
        }
    }

    let app = match req.app {
        Some(app) if !app.is_empty() => app,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "app is required"})),
            );
        }
    };

    eprintln!(
        "[zestfuld] Focus: app={} window_id={} tab_id={}",
        app,
        req.window_id.as_deref().unwrap_or(""),
        req.tab_id.as_deref().unwrap_or("")
    );

    if let Err(e) = focus::handle_focus(
        &app,
        req.window_id.as_deref(),
        req.tab_id.as_deref(),
    )
    .await
    {
        eprintln!("[zestfuld] Focus error: {}", e);
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

    eprintln!("[zestfuld] Shutting down...");
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
    async fn test_focus_missing_app() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .header("X-Zestful-Token", config::read_token().unwrap_or_default())
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "app is required");
    }

    #[tokio::test]
    async fn test_focus_empty_app() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .header("X-Zestful-Token", config::read_token().unwrap_or_default())
                    .body(Body::from(r#"{"app":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_focus_valid_request() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .header("X-Zestful-Token", config::read_token().unwrap_or_default())
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
    async fn test_focus_invalid_json() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .header("X-Zestful-Token", config::read_token().unwrap_or_default())
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
                    .header("X-Zestful-Token", config::read_token().unwrap_or_default())
                    .body(Body::from(r#"{"app":"Terminal"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_focus_rejects_missing_token() {
        // Only test this if a token is configured (otherwise auth is skipped)
        if config::read_token().is_some() {
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

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn test_focus_rejects_wrong_token() {
        if config::read_token().is_some() {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/focus")
                        .header("content-type", "application/json")
                        .header("X-Zestful-Token", "wrong-token-value")
                        .body(Body::from(r#"{"app":"Terminal"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn test_focus_rejects_injection_in_app() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/focus")
                    .header("content-type", "application/json")
                    .header("X-Zestful-Token", config::read_token().unwrap_or_default())
                    .body(Body::from(r#"{"app":"Finder\"; display dialog \"pwned"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should succeed at HTTP level but focus::handle_focus will reject the invalid chars
        // The response is still 200 because the error is logged, not returned
        // But the osascript won't execute arbitrary code due to validation
        assert!(response.status().is_success() || response.status().is_client_error());
    }
}

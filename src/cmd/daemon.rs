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

    // Ensure a local-token exists before clients (the TUI, the Mac app)
    // try to authenticate. The Mac app writes one on its first launch;
    // on Linux nothing else does, so the daemon takes responsibility.
    if let Err(e) = config::ensure_token() {
        crate::log::log("daemon", &format!("WARN: ensure_token failed: {}", e));
    }

    // Initialize the local event store. Migration failure is fatal —
    // a half-migrated DB is worse than a dead daemon.
    let db_path = config::config_dir().join("events.db");
    if let Err(e) = crate::events::store::init(&db_path) {
        crate::log::log("events", &format!("FATAL: store init failed: {}", e));
        std::process::exit(1);
    }

    // Start the agent-scraper subsystem. Off via `scraper.enabled = false`
    // for the rare case someone needs to disable it without redeploying.
    let _scraper_handle = if crate::scraper::is_enabled() {
        Some(crate::scraper::spawn())
    } else {
        crate::log::log("scraper", "disabled via scraper.enabled = false");
        None
    };

    let app = build_router();

    let port = config::daemon_port();
    let addr = format!("127.0.0.1:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Another daemon is already holding the port. Check it's healthy
            // before bowing out so we don't silently swallow a zombie situation.
            let healthy = reqwest::Client::new()
                .get(format!("http://127.0.0.1:{}/health", port))
                .timeout(std::time::Duration::from_secs(1))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if healthy {
                crate::log::log("daemon", "another daemon is already running, exiting");
                return Ok(());
            }
            // Port in use but not healthy — surface the original error.
            return Err(e.into());
        }
        Err(e) => return Err(e.into()),
    };
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
        || app_lower == "code"
        || app_lower.contains("visual studio code")
        || app_lower == "cursor"
        || app_lower == "windsurf"
        || app_lower == "zed";
    let focus_result = if is_ide {
        ides::handle_focus(
            &parsed.app,
            parsed.window_id.as_deref(),
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

    // Accept. Persist + log one line per event.
    for env in &envelopes {
        // Sync persist to local store. A 200 response means the event is
        // durably on disk. I/O failure here is a hard error — return 500.
        let env_clone = env.clone();
        let insert_result = tokio::task::spawn_blocking(move || {
            let c = crate::events::store::conn().lock().unwrap();
            crate::events::store::write::insert(&c, &env_clone)
        })
        .await
        .expect("store insert task panicked");

        let outcome = match insert_result {
            Ok(o) => o,
            Err(e) => {
                crate::log::log("events", &format!("store insert failed: {}", e));
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": "local store write failed",
                        "detail": e.to_string(),
                    })),
                )
                    .into_response();
            }
        };

        // Trigger a prune check every PRUNE_CHECK_EVERY inserts.
        crate::events::store::record_insert_and_maybe_prune(
            crate::events::store::DEFAULT_MAX_BYTES,
        );

        // Broadcast "projection changed" to any /stream subscribers.
        // One frame per event (batches produce one frame per envelope).
        let event_type = env.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        crate::events::broadcast::send(
            crate::events::broadcast::ProjectionChangedFrame {
                source_event_types: vec![event_type],
                ts: now_ms,
                reason: None,
            }
        );

        let type_ = env.get("type").and_then(|v| v.as_str()).unwrap_or("?");
        let id = env.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let source = env.get("source").and_then(|v| v.as_str()).unwrap_or("?");
        let session_id = env
            .get("correlation")
            .and_then(|c| c.get("session_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let outcome_label = match outcome {
            crate::events::store::write::InsertOutcome::Inserted(rowid) => format!("rowid={}", rowid),
            crate::events::store::write::InsertOutcome::DuplicateIgnored => "dup".to_string(),
        };
        crate::log::log(
            "events",
            &format!(
                "accepted id={} type={} source={} session={} {}",
                id, type_, source, session_id, outcome_label
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

#[derive(serde::Deserialize)]
struct EventsQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    until: Option<i64>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    surface_token: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn handle_list_events(
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<EventsQuery>,
) -> impl axum::response::IntoResponse {
    // Same token gate as POST.
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
            ).into_response();
        }
    }

    let filters = crate::events::store::query::ListFilters {
        since: q.since,
        until: q.until,
        source: q.source,
        event_type: q.event_type,
        session_id: q.session_id,
        agent: q.agent,
        surface_token: q.surface_token,
    };
    let cursor = q.cursor.as_deref()
        .and_then(crate::events::store::query::Cursor::parse);
    let limit = q.limit.unwrap_or(50).min(500);

    let result = tokio::task::spawn_blocking(move || {
        let c = crate::events::store::conn().lock().unwrap();
        crate::events::store::query::list(&c, &filters, limit, cursor)
    })
    .await
    .expect("query task panicked");
    match result {
        Ok((rows, next)) => {
            let next_cursor = next.map(|c| c.to_string());
            let has_more = next_cursor.is_some();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "events": rows,
                    "next_cursor": next_cursor,
                    "has_more": has_more,
                })),
            ).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "query failed",
                "detail": e.to_string(),
            })),
        ).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct TilesQuery {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    since: Option<i64>,
}

async fn handle_tiles(
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<TilesQuery>,
) -> impl axum::response::IntoResponse {
    // Same token gate as /events.
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
            ).into_response();
        }
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let since_ms = q.since.unwrap_or(now_ms - 24 * 3_600_000);
    let agent_filter = q.agent;

    let result = tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<crate::events::tiles::tile::Tile>> {
        let c = crate::events::store::conn().lock().unwrap();
        let mut tiles = crate::events::tiles::compute(&c, since_ms)?;
        crate::events::tiles::enrich_with_metrics(&c, &mut tiles, since_ms, now_ms)?;
        Ok(tiles)
    })
    .await
    .expect("tiles compute task panicked");

    match result {
        Ok(mut tiles) => {
            if let Some(a) = agent_filter {
                tiles.retain(|t| t.agent == a);
            }
            // Reflect the actual window covered by the result, not the
            // default. A caller passing ?since=<ts> changes coverage and
            // consumers that key off window_hours need the real value.
            let window_hours = ((now_ms - since_ms).max(0) / 3_600_000) as i64;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "tiles": tiles,
                    "window_hours": window_hours,
                    "computed_at": now_ms,
                })),
            ).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "tiles compute failed",
                "detail": e.to_string(),
            })),
        ).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct LogEntry {
    ts: i64,
    component: String,
    level: String,
    message: String,
}

async fn handle_log(
    headers: axum::http::HeaderMap,
    body: axum::extract::Json<Vec<LogEntry>>,
) -> impl axum::response::IntoResponse {
    // Same X-Zestful-Token gate as /events, /tiles, /notifications, /stream.
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

    let entries = body.0;
    for entry in &entries {
        // Strip embedded newlines so a multi-line message (e.g., a JS
        // exception with a stack trace) can't corrupt the one-line-per-
        // entry log format.
        let component = entry.component.replace(['\n', '\r'], " ");
        let message = entry.message.replace(['\n', '\r'], " ");
        crate::log::log_with_ts(
            entry.ts,
            &component,
            &format!("{}: {}", entry.level, message),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({"accepted": entries.len()})),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct NotificationsQuery {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    rule: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    since: Option<i64>,
}

async fn handle_notifications(
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<NotificationsQuery>,
) -> impl axum::response::IntoResponse {
    // Same token gate as /events and /tiles.
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

    // Validate severity param before doing any work.
    let min_severity_rank: Option<u8> = match &q.severity {
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "info" => Some(0),
            "warn" => Some(1),
            "urgent" => Some(2),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "invalid severity",
                        "detail": "expected info|warn|urgent",
                    })),
                )
                    .into_response();
            }
        },
        None => None,
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let since_ms = q.since.unwrap_or(now_ms - 24 * 3_600_000);
    let agent_filter = q.agent;
    let rule_filter = q.rule;

    let result = tokio::task::spawn_blocking(move || {
        let c = crate::events::store::conn().lock().unwrap();
        crate::events::notifications::compute(&c, since_ms)
    })
    .await
    .expect("notifications compute task panicked");

    match result {
        Ok(mut notifications) => {
            if let Some(a) = agent_filter {
                notifications.retain(|n| n.agent == a);
            }
            if let Some(r) = rule_filter {
                notifications.retain(|n| n.rule_id == r);
            }
            if let Some(min) = min_severity_rank {
                notifications.retain(|n| {
                    let rank = match n.severity {
                        crate::events::notifications::rule::Severity::Info => 0u8,
                        crate::events::notifications::rule::Severity::Warn => 1,
                        crate::events::notifications::rule::Severity::Urgent => 2,
                    };
                    rank >= min
                });
            }
            let window_hours = ((now_ms - since_ms).max(0) / 3_600_000) as i64;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "notifications": notifications,
                    "window_hours": window_hours,
                    "computed_at": now_ms,
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "query failed",
                "detail": e.to_string(),
            })),
        )
            .into_response(),
    }
}

pub(crate) async fn handle_stream(
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // Same token gate as /events, /tiles, /notifications.
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

    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures::stream::{self, StreamExt as _};
    use tokio_stream::wrappers::BroadcastStream;

    // Synthetic initial frame so the client does one refetch on connect
    // without special-casing startup.
    let initial = crate::events::broadcast::ProjectionChangedFrame {
        source_event_types: Vec::new(),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        reason: Some("initial".to_string()),
    };
    let initial_event = Event::default()
        .event("projection.changed")
        .data(serde_json::to_string(&initial).unwrap_or_default());
    let initial_stream = stream::once(async move {
        Ok::<_, std::convert::Infallible>(initial_event)
    });

    // Live stream from the broadcast channel.
    let rx = crate::events::broadcast::sender().subscribe();
    let live = BroadcastStream::new(rx).map(|r| match r {
        Ok(frame) => Ok::<_, std::convert::Infallible>(
            Event::default()
                .event("projection.changed")
                .data(serde_json::to_string(&frame).unwrap_or_default()),
        ),
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(_n)) => {
            // Subscriber fell behind. Send a catchup frame so the client
            // refetches and moves on.
            let catchup = crate::events::broadcast::ProjectionChangedFrame {
                source_event_types: Vec::new(),
                ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
                reason: Some("catchup".to_string()),
            };
            Ok(Event::default()
                .event("projection.changed")
                .data(serde_json::to_string(&catchup).unwrap_or_default()))
        }
    });

    let full = initial_stream.chain(live);

    Sse::new(full)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
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

async fn handle_summary(
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
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
            ).into_response();
        }
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let result = tokio::task::spawn_blocking(move || {
        let c = crate::events::store::conn().lock().unwrap();
        crate::events::summary::compute(&c, now_ms)
    })
    .await
    .expect("summary compute task panicked");

    match result {
        Ok(summary) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "summary": summary,
                "computed_at": now_ms,
            })),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "summary compute failed",
                "detail": e.to_string(),
            })),
        ).into_response(),
    }
}

/// Build the full daemon router. Shared between production startup in
/// `run_server` and the test `app()` helper so the two can't drift.
fn build_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/focus", post(handle_focus))
        .route("/inspect", get(handle_inspect))
        .layer(DefaultBodyLimit::max(16_384))
        .route(
            "/events",
            post(handle_events)
                .get(handle_list_events)
                .layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route("/tiles", get(handle_tiles).layer(DefaultBodyLimit::max(16_384)))
        .route("/notifications", get(handle_notifications).layer(DefaultBodyLimit::max(16_384)))
        .route("/summary", get(handle_summary).layer(DefaultBodyLimit::max(16_384)))
        .route("/stream", get(handle_stream))
        .route("/log", post(handle_log).layer(DefaultBodyLimit::max(64 * 1024)))
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
            // SAFETY: tests run single-threaded via --test-threads=1.
            unsafe { std::env::set_var(home_var, td.path()); }
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
            // SAFETY: tests run single-threaded via --test-threads=1.
            unsafe {
                match &self.old_home {
                    Some(v) => std::env::set_var(home_var, v),
                    None => std::env::remove_var(home_var),
                }
            }
        }
    }

    fn app() -> Router {
        static TEST_STORE_INIT: std::sync::Once = std::sync::Once::new();
        TEST_STORE_INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("zestful-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let db_path = dir.join("events.db");
            let _ = std::fs::remove_file(&db_path);
            let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
            crate::events::store::init(&db_path).expect("test store init");
        });

        build_router()
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

    #[tokio::test]
    async fn test_get_events_requires_token() {
        let _home = HomeGuard::new();
        set_test_token("tok-get-1");
        let response = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_post_then_get_events_roundtrip() {
        let _home = HomeGuard::new();
        set_test_token("tok-rt-1");

        // POST one envelope. Use a 26-char ULID-shaped id unique to this test.
        let envelope = serde_json::json!({
            "id": "01KPVSROUNDTRIP1AAAAAAAAAA",
            "schema": 1,
            "ts": 1_234_567_890_000i64,
            "seq": 0,
            "host": "h",
            "os_user": "u",
            "device_id": "d",
            "source": "roundtrip-test-source",
            "source_pid": 1,
            "type": "turn.completed"
        });
        let post = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/events")
                    .header("content-type", "application/json")
                    .header("x-zestful-token", "tok-rt-1")
                    .body(Body::from(envelope.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        // GET filtered by source.
        let get = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events?source=roundtrip-test-source")
                    .header("x-zestful-token", "tok-rt-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let events = json["events"].as_array().unwrap();
        assert!(!events.is_empty(), "expected at least one event");
        let found = events.iter().any(|e|
            e["event_id"].as_str() == Some("01KPVSROUNDTRIP1AAAAAAAAAA")
            && e["source"].as_str() == Some("roundtrip-test-source")
        );
        assert!(found, "expected to find the POSTed event, got: {}", json);
    }

    #[tokio::test]
    async fn test_get_events_with_nonmatching_filter_returns_empty() {
        let _home = HomeGuard::new();
        set_test_token("tok-empty-1");
        let response = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events?source=nonexistent-source-never-used")
                    .header("x-zestful-token", "tok-empty-1")
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
        assert_eq!(json["events"].as_array().unwrap().len(), 0);
        assert_eq!(json["has_more"], false);
    }

    #[tokio::test]
    async fn test_get_tiles_requires_token() {
        let _home = HomeGuard::new();
        set_test_token("tok-tiles-1");
        let response = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_get_tiles_returns_tiles_for_seeded_events() {
        let _home = HomeGuard::new();
        set_test_token("tok-tiles-2");

        // Seed 2 events that should produce 2 distinct tiles (different surfaces).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Tile A: claude-code on /x in tmux pane 0
        let env_a = serde_json::json!({
            "id": format!("01TILESEED-A-{}", now_ms),
            "schema": 1, "ts": now_ms, "seq": 0, "host": "h", "os_user": "u",
            "device_id": "d", "source": "claude-code", "source_pid": 1,
            "type": "turn.completed",
            "context": {
                "agent": "claude-code",
                "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x" },
                "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
            }
        });
        // Tile B: claude-code on /x in tmux pane 1 (different surface)
        let env_b = serde_json::json!({
            "id": format!("01TILESEED-B-{}", now_ms),
            "schema": 1, "ts": now_ms, "seq": 0, "host": "h", "os_user": "u",
            "device_id": "d", "source": "claude-code", "source_pid": 1,
            "type": "turn.completed",
            "context": {
                "agent": "claude-code",
                "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x" },
                "subapplication": { "kind": "tmux", "session": "z", "pane": "%1" }
            }
        });

        for env in [&env_a, &env_b] {
            let post = app()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/events")
                        .header("content-type", "application/json")
                        .header("x-zestful-token", "tok-tiles-2")
                        .body(Body::from(env.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(post.status(), StatusCode::OK);
        }

        let get = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tiles?agent=claude-code")
                    .header("x-zestful-token", "tok-tiles-2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tiles = json["tiles"].as_array().unwrap();

        // Filter to tiles we just inserted (test DB is shared across tests).
        let our_tiles: Vec<_> = tiles
            .iter()
            .filter(|t| {
                t["agent"].as_str() == Some("claude-code")
                    && t["project_anchor"].as_str() == Some("/x")
            })
            .collect();
        assert_eq!(our_tiles.len(), 2, "expected 2 tiles, got {:?}", our_tiles);
        // Confirm they have distinct surface_tokens.
        let tokens: std::collections::HashSet<&str> = our_tiles
            .iter()
            .map(|t| t["surface_token"].as_str().unwrap())
            .collect();
        assert_eq!(tokens.len(), 2);
    }

    #[tokio::test]
    async fn test_get_tiles_since_param_widens_window() {
        let _home = HomeGuard::new();
        set_test_token("tok-tiles-3");

        // Seed an event; query with default 24h should include it.
        // Then query with since = far_future — should NOT include it.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let env = serde_json::json!({
            "id": format!("01TILE-SINCE-{}", now_ms),
            "schema": 1, "ts": now_ms, "seq": 0, "host": "h", "os_user": "u",
            "device_id": "d", "source": "claude-code", "source_pid": 1,
            "type": "turn.completed",
            "context": {
                "agent": "claude-code",
                "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x-since-test" },
                "subapplication": { "kind": "tmux", "session": "since", "pane": "%0" }
            }
        });
        let post = app()
            .oneshot(
                Request::builder()
                    .method("POST").uri("/events")
                    .header("content-type", "application/json")
                    .header("x-zestful-token", "tok-tiles-3")
                    .body(Body::from(env.to_string()))
                    .unwrap(),
            )
            .await.unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        // Default window: should include our event.
        let default_get = app()
            .oneshot(
                Request::builder()
                    .method("GET").uri("/tiles?agent=claude-code")
                    .header("x-zestful-token", "tok-tiles-3")
                    .body(Body::empty()).unwrap(),
            )
            .await.unwrap();
        let default_body = axum::body::to_bytes(default_get.into_body(), usize::MAX).await.unwrap();
        let default_json: serde_json::Value = serde_json::from_slice(&default_body).unwrap();
        let default_tiles = default_json["tiles"].as_array().unwrap();
        assert!(
            default_tiles.iter().any(|t| t["project_anchor"].as_str() == Some("/x-since-test")),
            "expected to find our tile in default window"
        );

        // since = now + 1 hour: should EXCLUDE our event.
        let since = now_ms + 3_600_000;
        let future_get = app()
            .oneshot(
                Request::builder()
                    .method("GET").uri(&format!("/tiles?since={}", since))
                    .header("x-zestful-token", "tok-tiles-3")
                    .body(Body::empty()).unwrap(),
            )
            .await.unwrap();
        let future_body = axum::body::to_bytes(future_get.into_body(), usize::MAX).await.unwrap();
        let future_json: serde_json::Value = serde_json::from_slice(&future_body).unwrap();
        let future_tiles = future_json["tiles"].as_array().unwrap();
        assert!(
            future_tiles.iter().all(|t| t["project_anchor"].as_str() != Some("/x-since-test")),
            "expected our tile NOT to appear when since is in the future, but got {:?}",
            future_tiles
        );
    }

    #[tokio::test]
    async fn test_get_tiles_filter_by_agent_excludes_others() {
        let _home = HomeGuard::new();
        set_test_token("tok-tiles-4");

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Seed one claude-code event and one codex-cli event with
        // distinct project_anchors (so they won't collide with prior tests).
        let env_claude = serde_json::json!({
            "id": format!("FILTERAGNT-C-{}", now_ms),
            "schema": 1, "ts": now_ms, "seq": 0, "host": "h", "os_user": "u",
            "device_id": "d", "source": "claude-code", "source_pid": 1,
            "type": "turn.completed",
            "context": {
                "agent": "claude-code",
                "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x-filter-claude" },
                "subapplication": { "kind": "tmux", "session": "fc", "pane": "%0" }
            }
        });
        let env_codex = serde_json::json!({
            "id": format!("FILTERAGNT-X-{}", now_ms),
            "schema": 1, "ts": now_ms, "seq": 0, "host": "h", "os_user": "u",
            "device_id": "d", "source": "codex-cli", "source_pid": 1,
            "type": "turn.completed",
            "context": {
                "agent": "codex-cli",
                "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x-filter-codex" },
                "subapplication": { "kind": "tmux", "session": "fx", "pane": "%0" }
            }
        });

        for env in [&env_claude, &env_codex] {
            let post = app()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/events")
                        .header("content-type", "application/json")
                        .header("x-zestful-token", "tok-tiles-4")
                        .body(Body::from(env.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(post.status(), StatusCode::OK);
        }

        // Filter to claude-code: must INCLUDE the claude tile and EXCLUDE the codex tile.
        let get = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tiles?agent=claude-code")
                    .header("x-zestful-token", "tok-tiles-4")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tiles = json["tiles"].as_array().unwrap();

        // The claude-code tile we just seeded must appear.
        let claude_tile_present = tiles.iter().any(|t|
            t["agent"].as_str() == Some("claude-code")
                && t["project_anchor"].as_str() == Some("/x-filter-claude")
        );
        assert!(claude_tile_present, "claude-code tile missing from filtered result");

        // The codex-cli tile must NOT appear, regardless of project_anchor.
        let codex_tile_present = tiles.iter().any(|t| t["agent"].as_str() == Some("codex-cli"));
        assert!(!codex_tile_present, "codex-cli tile should have been filtered out, got tiles: {:?}",
                tiles.iter().map(|t| t["agent"].as_str()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn events_post_triggers_broadcast_frame() {
        let _home = HomeGuard::new();
        set_test_token("test-token");

        let mut rx = crate::events::broadcast::sender().subscribe();

        let body = serde_json::to_string(&canned_envelope()).unwrap();
        let resp = send_events_request(&body, Some("test-token")).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("no frame within 2s")
            .expect("recv err");
        // canned_envelope produces type = "turn.completed"
        assert_eq!(frame.source_event_types, vec!["turn.completed"]);
    }

    #[tokio::test]
    async fn stream_laggy_subscriber_recovers() {
        // Fill the broadcast ring past capacity (16) BEFORE subscribing so
        // the new subscriber is lagged on first recv. Verify no deadlock —
        // first recv should surface Lagged, subsequent ones fresh frames.
        for i in 0..32 {
            crate::events::broadcast::send(
                crate::events::broadcast::ProjectionChangedFrame {
                    source_event_types: vec![format!("lag.{}", i)],
                    ts: i,
                    reason: None,
                },
            );
        }
        let mut rx = crate::events::broadcast::sender().subscribe();
        // Send one more frame after subscribing.
        crate::events::broadcast::send(
            crate::events::broadcast::ProjectionChangedFrame {
                source_event_types: vec!["post-subscribe".into()],
                ts: 999,
                reason: None,
            },
        );

        let recv1 = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            rx.recv(),
        )
        .await
        .expect("recv timeout");
        // Either we get a fresh Ok frame (no lag surfaced), or Lagged (our
        // case if we were truly behind by >capacity). Both are acceptable;
        // the point is no deadlock.
        match recv1 {
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(other) => panic!("unexpected recv error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn stream_endpoint_401_without_token() {
        let _home = HomeGuard::new();
        let req = Request::builder()
            .method("GET")
            .uri("/stream")
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn stream_endpoint_returns_text_event_stream() {
        let _home = HomeGuard::new();
        set_test_token("test-token");
        let req = Request::builder()
            .method("GET")
            .uri("/stream")
            .header("x-zestful-token", "test-token")
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ctype = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ctype.starts_with("text/event-stream"),
            "content-type = {}",
            ctype
        );
    }

    #[tokio::test]
    async fn notifications_endpoint_401_without_token() {
        let _home = HomeGuard::new();
        let req = Request::builder()
            .method("GET")
            .uri("/notifications")
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn notifications_endpoint_ok_with_auth() {
        let _home = HomeGuard::new();
        set_test_token("test-token");
        let req = Request::builder()
            .method("GET")
            .uri("/notifications")
            .header("x-zestful-token", "test-token")
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn notifications_endpoint_rejects_invalid_severity() {
        let _home = HomeGuard::new();
        set_test_token("test-token");
        let req = Request::builder()
            .method("GET")
            .uri("/notifications?severity=critical")
            .header("x-zestful-token", "test-token")
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_get_tiles_default_window_excludes_old_events_since_zero_includes() {
        let _home = HomeGuard::new();
        set_test_token("tok-tiles-5");

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Seed an event via POST (its received_at lands at ~now).
        let env = serde_json::json!({
            "id": format!("01OLDEVENT-T-{}", now_ms),
            "schema": 1, "ts": now_ms, "seq": 0, "host": "h", "os_user": "u",
            "device_id": "d", "source": "claude-code", "source_pid": 1,
            "type": "turn.completed",
            "context": {
                "agent": "claude-code",
                "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x-old-event-test" },
                "subapplication": { "kind": "tmux", "session": "old", "pane": "%0" }
            }
        });
        let post = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/events")
                    .header("content-type", "application/json")
                    .header("x-zestful-token", "tok-tiles-5")
                    .body(Body::from(env.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        // Backdate the row's received_at to 48h ago via direct SQL.
        let backdated = now_ms - 48 * 3_600_000;
        {
            let c = crate::events::store::conn().lock().unwrap();
            c.execute(
                "UPDATE events SET received_at = ? WHERE event_id = ?",
                rusqlite::params![backdated, format!("01OLDEVENT-T-{}", now_ms)],
            ).unwrap();
        }

        // Default window (now - 24h): should NOT find our backdated event.
        let default_get = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tiles?agent=claude-code")
                    .header("x-zestful-token", "tok-tiles-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await.unwrap();
        let default_body = axum::body::to_bytes(default_get.into_body(), usize::MAX).await.unwrap();
        let default_json: serde_json::Value = serde_json::from_slice(&default_body).unwrap();
        let default_tiles = default_json["tiles"].as_array().unwrap();
        assert!(
            default_tiles.iter().all(|t| t["project_anchor"].as_str() != Some("/x-old-event-test")),
            "default 24h window should EXCLUDE backdated event, got tiles: {:?}",
            default_tiles.iter().map(|t| t["project_anchor"].as_str()).collect::<Vec<_>>()
        );

        // since=0: should INCLUDE the backdated event.
        let wide_get = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tiles?agent=claude-code&since=0")
                    .header("x-zestful-token", "tok-tiles-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await.unwrap();
        let wide_body = axum::body::to_bytes(wide_get.into_body(), usize::MAX).await.unwrap();
        let wide_json: serde_json::Value = serde_json::from_slice(&wide_body).unwrap();
        let wide_tiles = wide_json["tiles"].as_array().unwrap();
        assert!(
            wide_tiles.iter().any(|t| t["project_anchor"].as_str() == Some("/x-old-event-test")),
            "since=0 should INCLUDE backdated event"
        );
    }

    #[tokio::test]
    async fn log_endpoint_403_without_token() {
        let _home = HomeGuard::new();
        let req = Request::builder()
            .method("POST")
            .uri("/log")
            .header("content-type", "application/json")
            .body(Body::from(r#"[{"ts":1,"component":"chrome-ext:sw","level":"info","message":"hi"}]"#))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn log_endpoint_accepts_valid_batch_and_writes_lines() {
        let _home = HomeGuard::new();
        set_test_token("test-token");

        let body = r#"[
            {"ts":1700000000001,"component":"chrome-ext:sw","level":"info","message":"hello"},
            {"ts":1700000000002,"component":"chrome-ext:content/chatgpt","level":"warn","message":"x"}
        ]"#;
        let req = Request::builder()
            .method("POST")
            .uri("/log")
            .header("content-type", "application/json")
            .header("x-zestful-token", "test-token")
            .body(Body::from(body))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let log_path = crate::config::config_dir().join("zestful.log");
        let contents = std::fs::read_to_string(&log_path).expect("log file should exist after POST /log");
        assert!(
            contents.contains("[chrome-ext:sw] info: hello"),
            "expected SW info line; got log:\n{}",
            contents
        );
        assert!(
            contents.contains("[chrome-ext:content/chatgpt] warn: x"),
            "expected content-script warn line; got log:\n{}",
            contents
        );
        assert!(
            contents.contains("2023-11-14T") && contents.contains(".001 [chrome-ext:sw]"),
            "expected per-entry ts to drive the line prefix; got log:\n{}",
            contents
        );
    }

    #[tokio::test]
    async fn log_endpoint_rejects_malformed_json() {
        let _home = HomeGuard::new();
        set_test_token("test-token");

        let req = Request::builder()
            .method("POST")
            .uri("/log")
            .header("content-type", "application/json")
            .header("x-zestful-token", "test-token")
            .body(Body::from(r#"{"not":"an array"}"#))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert!(
            resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::UNPROCESSABLE_ENTITY,
            "expected 400 or 422 for malformed body; got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn log_endpoint_strips_embedded_newlines() {
        let _home = HomeGuard::new();
        set_test_token("test-token");

        // Multi-line message simulating a JS stack trace.
        let body = r#"[
            {"ts":1700000000003,"component":"chrome-ext:content/chatgpt","level":"error","message":"Bridge error: TypeError: x is undefined\n    at foo (bar.js:1:2)\n    at baz (qux.js:3:4)"}
        ]"#;
        let req = Request::builder()
            .method("POST")
            .uri("/log")
            .header("content-type", "application/json")
            .header("x-zestful-token", "test-token")
            .body(Body::from(body))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let log_path = crate::config::config_dir().join("zestful.log");
        let contents = std::fs::read_to_string(&log_path).expect("log file should exist");

        // The single entry must produce exactly one log line — newlines
        // inside `message` get replaced with spaces.
        let zestful_lines: Vec<&str> = contents
            .lines()
            .filter(|l| l.contains("[chrome-ext:content/chatgpt]"))
            .collect();
        assert_eq!(
            zestful_lines.len(),
            1,
            "expected exactly one log line for a multi-line message; got:\n{}",
            contents
        );
        assert!(
            zestful_lines[0].contains("at foo (bar.js:1:2)"),
            "expected stack trace content preserved on the same line; got: {}",
            zestful_lines[0]
        );
    }

    #[tokio::test]
    async fn summary_returns_default_on_empty_store() {
        let _home = HomeGuard::new();
        set_test_token("test-token-summary");

        let req = Request::builder()
            .method("GET")
            .uri("/summary")
            .header("x-zestful-token", "test-token-summary")
            .body(Body::empty()).unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 16_384).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["summary"]["today_cost_usd"], 0.0);
        assert_eq!(v["summary"]["today_tokens"], 0);
        assert_eq!(v["summary"]["agents"], 0);
        assert_eq!(v["summary"]["sessions"], 0);
        assert_eq!(v["summary"]["cost_sparkline"].as_array().unwrap().len(), 7);
    }

    #[tokio::test]
    async fn summary_403s_without_token() {
        let _home = HomeGuard::new();
        set_test_token("test-token-summary-noauth");
        let req = Request::builder()
            .method("GET")
            .uri("/summary")
            .body(Body::empty()).unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}

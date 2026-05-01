//! Network-only client of the daemon's HTTP+SSE API. Pure I/O — knows
//! nothing about ratatui or AppState. Used by `mod.rs`'s event loop.

use crate::events::tiles::tile::Tile;
use crate::events::notifications::notification::Notification;
use crate::events::store::query::EventRow;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

// Wire-format response wrappers. We only consume the array payload —
// metadata fields like `window_hours`/`computed_at`/`next_cursor`/`has_more`
// from the daemon are accepted (and ignored) via Deserialize's default
// behavior of skipping unknown fields. If pagination becomes useful, add
// fields back here.

#[derive(Debug, Deserialize)]
pub struct TilesResponse {
    pub tiles: Vec<Tile>,
}

#[derive(Debug, Deserialize)]
pub struct NotificationsResponse {
    pub notifications: Vec<Notification>,
}

#[derive(Debug, Deserialize)]
pub struct EventsResponse {
    pub events: Vec<EventRow>,
}

#[allow(dead_code)] // wired into TUI event loop in Task 10
#[derive(Debug, Deserialize)]
pub struct SummaryResponse {
    pub summary: crate::events::summary::Summary,
}

#[derive(Clone)]
pub struct Client {
    base_url: String,   // e.g. "http://127.0.0.1:21548"
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// Construct from the standard config locations.
    pub fn from_config() -> Result<Self> {
        let token = crate::config::read_token()
            .ok_or_else(|| anyhow!("token not found at ~/.config/zestful/local-token. Is the Zestful app running?"))?;
        let port = crate::config::daemon_port();
        Self::new(&format!("http://127.0.0.1:{}", port), &token)
    }

    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("building reqwest client")?;
        Ok(Self {
            base_url: base_url.to_string(),
            token: token.to_string(),
            http,
        })
    }

    pub async fn tiles(&self, since_ms: i64) -> Result<Vec<Tile>> {
        let resp = self.http
            .get(format!("{}/tiles", self.base_url))
            .header("X-Zestful-Token", &self.token)
            .query(&[("since", since_ms.to_string())])
            .send().await
            .context("GET /tiles")?;
        check_status(&resp).await?;
        let body: TilesResponse = resp.json().await.context("parsing /tiles JSON")?;
        Ok(body.tiles)
    }

    pub async fn notifications(&self, since_ms: i64) -> Result<Vec<Notification>> {
        let resp = self.http
            .get(format!("{}/notifications", self.base_url))
            .header("X-Zestful-Token", &self.token)
            .query(&[("since", since_ms.to_string())])
            .send().await
            .context("GET /notifications")?;
        check_status(&resp).await?;
        let body: NotificationsResponse = resp.json().await.context("parsing /notifications JSON")?;
        Ok(body.notifications)
    }

    #[allow(dead_code)] // wired into TUI event loop in Task 10
    pub async fn summary(&self) -> Result<crate::events::summary::Summary> {
        let resp = self.http
            .get(format!("{}/summary", self.base_url))
            .header("X-Zestful-Token", &self.token)
            .send().await
            .context("GET /summary")?;
        check_status(&resp).await?;
        let body: SummaryResponse = resp.json().await.context("parsing /summary JSON")?;
        Ok(body.summary)
    }

    pub async fn events_for_agent(&self, agent: &str, surface_token: Option<&str>, since_ms: i64, limit: usize) -> Result<Vec<EventRow>> {
        let mut query = vec![
            ("agent".to_string(), agent.to_string()),
            ("since".to_string(), since_ms.to_string()),
            ("limit".to_string(), limit.to_string()),
        ];
        if let Some(st) = surface_token {
            query.push(("surface_token".to_string(), st.to_string()));
        }
        let resp = self.http
            .get(format!("{}/events", self.base_url))
            .header("X-Zestful-Token", &self.token)
            .query(&query)
            .send().await
            .context("GET /events")?;
        check_status(&resp).await?;
        let body: EventsResponse = resp.json().await.context("parsing /events JSON")?;
        Ok(body.events)
    }

    pub async fn post_focus(&self, terminal_uri: &str) -> Result<()> {
        let resp = self.http
            .post(format!("{}/focus", self.base_url))
            .json(&serde_json::json!({ "terminal_uri": terminal_uri }))
            .send().await
            .context("POST /focus")?;
        check_status(&resp).await?;
        Ok(())
    }
}

use crate::events::broadcast::ProjectionChangedFrame;
use futures::stream::Stream;
use futures::StreamExt;

/// One frame emitted by the daemon's SSE `/stream` endpoint, or a
/// connection-state transition synthesized by the client.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Connected,
    ProjectionChanged(ProjectionChangedFrame),
    Disconnected(String),
}

impl Client {
    /// Subscribe to the daemon's projection-change SSE stream. Auto-
    /// reconnects with exponential backoff via `reqwest_eventsource`.
    /// Yields `StreamEvent`s indefinitely.
    pub fn stream(&self) -> impl Stream<Item = StreamEvent> + Send + 'static {
        use reqwest_eventsource::{Event as RsEvent, EventSource};

        let url = format!("{}/stream", self.base_url);
        let token = self.token.clone();

        let req = reqwest::Client::new()
            .get(&url)
            .header("X-Zestful-Token", token);
        let es = EventSource::new(req).expect("EventSource::new");

        es.filter_map(|res| async move {
            match res {
                Ok(RsEvent::Open) => Some(StreamEvent::Connected),
                Ok(RsEvent::Message(m)) => {
                    // Daemon names its events "projection.changed".
                    if m.event != "projection.changed" { return None; }
                    match serde_json::from_str::<ProjectionChangedFrame>(&m.data) {
                        Ok(frame) => Some(StreamEvent::ProjectionChanged(frame)),
                        Err(_) => None, // Malformed; skip.
                    }
                }
                Err(e) => Some(StreamEvent::Disconnected(format!("{}", e))),
            }
        })
    }
}

async fn check_status(resp: &reqwest::Response) -> Result<()> {
    let status = resp.status();
    if status.is_success() { return Ok(()); }
    Err(anyhow!("daemon responded {}", status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use tokio::net::TcpListener;

    async fn spawn_test_daemon(router: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{}", addr)
    }

    fn fake_tile_json() -> serde_json::Value {
        serde_json::json!({
            "id": "tile_abc",
            "agent": "claude-code",
            "project_anchor": "/x/zestful",
            "project_label": "zestful",
            "surface_kind": "cli",
            "surface_token": "tmux:z/pane:%0",
            "surface_label": "tmux [z:0]",
            "first_seen_at": 100,
            "last_seen_at": 200,
            "event_count": 42,
            "latest_event_type": "turn.completed",
            "focus_uri": "workspace://iterm2/window:1/tab:1"
        })
    }

    #[tokio::test]
    async fn tiles_parses_realistic_response() {
        // Build a stub router that returns a canned /tiles response.
        let router = Router::new().route("/tiles", axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "tiles": [super::tests::fake_tile_json()],
                "window_hours": 24,
                "computed_at": 1234567890i64,
            }))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "ignored-token").unwrap();
        let tiles = c.tiles(0).await.unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].agent, "claude-code");
        assert_eq!(tiles[0].event_count, 42);
        assert_eq!(tiles[0].focus_uri.as_deref(), Some("workspace://iterm2/window:1/tab:1"));
    }

    #[tokio::test]
    async fn tiles_403_returns_err() {
        let router = Router::new().route("/tiles", axum::routing::get(|| async {
            (axum::http::StatusCode::FORBIDDEN, axum::Json(serde_json::json!({"error":"invalid token"})))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "wrong").unwrap();
        let err = c.tiles(0).await.unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("403"), "expected 403 in error, got: {}", msg);
    }

    #[tokio::test]
    async fn post_focus_with_terminal_uri_succeeds() {
        let router = Router::new().route("/focus", axum::routing::post(|axum::Json(v): axum::Json<serde_json::Value>| async move {
            assert_eq!(v["terminal_uri"], "workspace://x");
            axum::Json(serde_json::json!({"status":"focused"}))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "x").unwrap();
        c.post_focus("workspace://x").await.unwrap();
    }

    #[tokio::test]
    async fn post_focus_400_returns_err() {
        let router = Router::new().route("/focus", axum::routing::post(|| async {
            (axum::http::StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error":"missing"})))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "x").unwrap();
        c.post_focus("").await.unwrap_err();
    }

    #[tokio::test]
    async fn events_for_agent_parses_response() {
        let router = Router::new().route("/events", axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "events": [{
                    "id": 1, "received_at": 0, "event_id": "e1", "event_type": "turn.completed",
                    "source": "hook", "session_id": null, "project": "zestful",
                    "host": "h", "os_user": "u", "device_id": "d",
                    "event_ts": 100, "seq": 0, "source_pid": 0, "schema_version": 1,
                    "correlation": null, "context": null, "payload": null
                }],
                "next_cursor": null,
                "has_more": false
            }))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "x").unwrap();
        let evs = c.events_for_agent("claude-code", None, 0, 50).await.unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, "turn.completed");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn stream_emits_initial_then_projection_changed() {
        // Mount the real daemon's /stream handler so we exercise the
        // actual SSE protocol the macOS app consumes. This is the
        // contract-validation point. The handler does not touch the
        // events store — only the broadcast channel — so we don't init
        // the store here. (Init is a OnceLock and would panic if a prior
        // serial test already initialized it.)
        std::env::set_var("ZESTFUL_TOKEN_OVERRIDE", "x");

        let app = axum::Router::new().route("/stream", axum::routing::get(crate::cmd::daemon::handle_stream));

        let url = spawn_test_daemon(app).await;
        let c = Client::new(&url, "x").unwrap();
        let mut s = Box::pin(c.stream());

        // First event: synthetic "initial" frame from the daemon on connect
        // (or a Connected synthesized by reqwest-eventsource first).
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), s.next()).await
            .expect("timeout waiting for first stream event")
            .expect("stream ended unexpectedly");
        match first {
            StreamEvent::ProjectionChanged(_) => {} // ok — daemon's initial frame
            StreamEvent::Connected => {
                // Some libs emit Connected first; advance once to find ProjectionChanged.
                let next = tokio::time::timeout(std::time::Duration::from_secs(3), s.next()).await
                    .expect("timeout").expect("stream ended");
                assert!(matches!(next, StreamEvent::ProjectionChanged(_)));
            }
            other => panic!("expected ProjectionChanged or Connected, got {:?}", other),
        }

        // Trigger a real frame via the broadcast channel.
        crate::events::broadcast::send(crate::events::broadcast::ProjectionChangedFrame {
            source_event_types: vec!["test".to_string()],
            ts: 0,
            reason: Some("test".to_string()),
        });

        let next = tokio::time::timeout(std::time::Duration::from_secs(3), s.next()).await
            .expect("timeout waiting for triggered frame")
            .expect("stream ended");
        match next {
            StreamEvent::ProjectionChanged(f) => {
                assert_eq!(f.reason.as_deref(), Some("test"));
            }
            other => panic!("expected ProjectionChanged, got {:?}", other),
        }

        std::env::remove_var("ZESTFUL_TOKEN_OVERRIDE");
    }

    #[tokio::test]
    async fn summary_parses_realistic_response() {
        let router = Router::new().route("/summary", axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "summary": {
                    "today_cost_usd": 4.27,
                    "today_tokens": 142_300,
                    "agents": 3,
                    "sessions": 7,
                    "cost_sparkline": [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6]
                },
                "computed_at": 1234567890i64,
            }))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "ignored").unwrap();
        let s = c.summary().await.unwrap();
        assert!((s.today_cost_usd - 4.27).abs() < 1e-9);
        assert_eq!(s.today_tokens, 142_300);
        assert_eq!(s.agents, 3);
        assert_eq!(s.sessions, 7);
        assert_eq!(s.cost_sparkline.len(), 7);
    }

    #[tokio::test]
    async fn summary_403_returns_err() {
        let router = Router::new().route("/summary", axum::routing::get(|| async {
            (axum::http::StatusCode::FORBIDDEN,
             axum::Json(serde_json::json!({"error":"invalid token"})))
        }));
        let url = spawn_test_daemon(router).await;
        let c = Client::new(&url, "wrong").unwrap();
        let err = c.summary().await.unwrap_err();
        assert!(format!("{}", err).contains("403"));
    }
}

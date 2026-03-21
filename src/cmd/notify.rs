//! `zestful notify` — send a notification to the Zestful Mac app.
//!
//! Builds a JSON payload and POSTs it to `localhost:{port}/notify` with the
//! auth token. Applies saved focus context if `--app` is not explicitly passed.

use crate::config;
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
struct NotifyBody {
    agent: String,
    message: String,
    severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tab_id: Option<String>,
    #[serde(skip_serializing_if = "is_true")]
    push: bool,
}

fn is_true(v: &bool) -> bool {
    *v
}

/// Execute the `notify` command: read config, apply focus context, send HTTP POST.
pub fn run(
    agent: String,
    message: String,
    severity: String,
    app: Option<String>,
    window_id: Option<String>,
    tab_id: Option<String>,
    no_push: bool,
) -> Result<()> {
    let token = config::read_token().ok_or_else(|| {
        anyhow::anyhow!("Zestful app not running or not configured. Token not found.")
    })?;
    let port = config::read_port();

    // Apply focus context if --app was not explicitly passed, or if the
    // explicit value is a multiplexer (tmux/screen) which isn't focusable:
    // 1. Try saved focus-context file
    // 2. Fall back to auto-detecting the terminal (looks through tmux/screen)
    let is_multiplexer = app.as_deref().map_or(false, |a| {
        let lower = a.to_lowercase();
        lower == "tmux" || lower == "screen"
    });
    let (app, window_id, tab_id) = if app.is_none() || is_multiplexer {
        let ctx = config::read_focus_context();
        let app = ctx
            .get("app")
            .cloned()
            .or_else(|| config::detect_terminal());
        let tab_id = tab_id
            .or_else(|| ctx.get("tab_id").cloned())
            .or_else(|| config::detect_tab_id());
        (
            app,
            window_id.or_else(|| ctx.get("window_id").cloned()),
            tab_id,
        )
    } else {
        (app, window_id, tab_id)
    };

    send(&token, port, &agent, &message, &severity, app, window_id, tab_id, no_push)
}

/// Send a notification to the Zestful app. Used by both `notify` and `watch` commands.
pub fn send(
    token: &str,
    port: u16,
    agent: &str,
    message: &str,
    severity: &str,
    app: Option<String>,
    window_id: Option<String>,
    tab_id: Option<String>,
    no_push: bool,
) -> Result<()> {
    let body = NotifyBody {
        agent: agent.to_string(),
        message: message.to_string(),
        severity: severity.to_string(),
        app,
        window_id,
        tab_id,
        push: !no_push,
    };

    let url = format!("http://127.0.0.1:{}/notify", port);
    let json = serde_json::to_string(&body)?;

    let result = ureq::post(&url)
        .header("X-Zestful-Token", token)
        .header("Content-Type", "application/json")
        .send(json.as_bytes());

    match result {
        Ok(_) => {}
        Err(ureq::Error::StatusCode(code)) => {
            // App responded but rejected the request — log but don't fail
            eprintln!("zestful: Zestful app returned HTTP {}", code);
        }
        Err(e) => {
            // Connection-level failure — log but don't fail
            let reason = match &e {
                ureq::Error::Io(_) => "connection refused",
                ureq::Error::Timeout(_) => "timeout",
                ureq::Error::HostNotFound => "host not found",
                _ => "request failed",
            };
            eprintln!("zestful: could not reach Zestful app ({})", reason);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notify_body_serialization_full() {
        let body = NotifyBody {
            agent: "test-agent".into(),
            message: "hello world".into(),
            severity: "warning".into(),
            app: Some("kitty".into()),
            window_id: Some("42".into()),
            tab_id: Some("my-tab".into()),
            push: true,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["agent"], "test-agent");
        assert_eq!(json["message"], "hello world");
        assert_eq!(json["severity"], "warning");
        assert_eq!(json["app"], "kitty");
        assert_eq!(json["window_id"], "42");
        assert_eq!(json["tab_id"], "my-tab");
        // push=true should be skipped
        assert!(json.get("push").is_none());
    }

    #[test]
    fn test_notify_body_serialization_minimal() {
        let body = NotifyBody {
            agent: "test".into(),
            message: "msg".into(),
            severity: "info".into(),
            app: None,
            window_id: None,
            tab_id: None,
            push: false,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["agent"], "test");
        assert!(json.get("app").is_none());
        assert!(json.get("window_id").is_none());
        assert!(json.get("tab_id").is_none());
        // push=false should be serialized
        assert_eq!(json["push"], false);
    }

    #[test]
    fn test_notify_body_special_chars() {
        let body = NotifyBody {
            agent: "test".into(),
            message: "hello \"world\"\nnewline".into(),
            severity: "info".into(),
            app: None,
            window_id: None,
            tab_id: None,
            push: true,
        };
        let json_str = serde_json::to_string(&body).unwrap();
        // serde_json properly escapes special characters
        assert!(json_str.contains("\\\"world\\\""));
        assert!(json_str.contains("\\n"));
    }

    #[test]
    fn test_send_no_server_returns_ok() {
        // Connection failure is not an error — CLI runs as a hook
        let result = send("fake-token", 19999, "test", "msg", "info", None, None, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_send_with_all_optional_fields() {
        let result = send(
            "fake-token",
            19999,
            "test",
            "msg",
            "info",
            Some("kitty".into()),
            Some("1".into()),
            Some("tab-1".into()),
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_true_function() {
        assert!(is_true(&true));
        assert!(!is_true(&false));
    }

    #[test]
    fn test_notify_body_push_false_serialized() {
        let body = NotifyBody {
            agent: "a".into(),
            message: "m".into(),
            severity: "info".into(),
            app: None,
            window_id: None,
            tab_id: None,
            push: false,
        };
        let json_str = serde_json::to_string(&body).unwrap();
        assert!(json_str.contains("\"push\":false"));
    }

    #[test]
    fn test_notify_body_push_true_skipped() {
        let body = NotifyBody {
            agent: "a".into(),
            message: "m".into(),
            severity: "info".into(),
            app: None,
            window_id: None,
            tab_id: None,
            push: true,
        };
        let json_str = serde_json::to_string(&body).unwrap();
        assert!(!json_str.contains("push"));
    }
}

//! `zestful notify` — send a notification to the Zestful Mac app.
//!
//! Builds a JSON payload and POSTs it to `localhost:{port}/notify` with the
//! auth token. Applies saved focus context if `--app` is not explicitly passed.

use crate::config;
use anyhow::{bail, Result};
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

    // Apply saved focus context if --app was not explicitly passed
    let (app, window_id, tab_id) = if app.is_none() {
        let ctx = config::read_focus_context();
        (
            ctx.get("app").cloned(),
            window_id.or_else(|| ctx.get("window_id").cloned()),
            tab_id.or_else(|| ctx.get("tab_id").cloned()),
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

    let url = format!("http://localhost:{}/notify", port);
    let json = serde_json::to_string(&body)?;

    let result = ureq::post(&url)
        .header("X-Zestful-Token", token)
        .header("Content-Type", "application/json")
        .send(json.as_bytes());

    match result {
        Ok(_) => Ok(()),
        Err(_) => {
            bail!("Could not connect to Zestful. Is the app running?");
        }
    }
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
    fn test_send_fails_no_server() {
        // Trying to connect to a port with no server should fail
        let result = send("fake-token", 19999, "test", "msg", "info", None, None, None, false);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Could not connect"));
        // Error must not leak the token
        assert!(!err_msg.contains("fake-token"));
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
        assert!(result.is_err()); // no server running
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

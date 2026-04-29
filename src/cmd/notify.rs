//! `zestful notify` — send a notification to the Zestful Mac app.
//!
//! Builds a JSON payload and POSTs it to `localhost:{port}/notify` with the
//! auth token. Auto-captures the terminal URI via the built-in workspace
//! inspector for click-to-focus.

use crate::config;
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
struct NotifyBody {
    agent: String,
    message: String,
    severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_uri: Option<String>,
    #[serde(skip_serializing_if = "is_true")]
    push: bool,
}

fn is_true(v: &bool) -> bool {
    *v
}

/// Execute the `notify` command: read config, auto-locate terminal, send HTTP POST.
pub fn run(
    agent: String,
    message: String,
    severity: String,
    terminal_uri: Option<String>,
    no_push: bool,
    debug: bool,
) -> Result<()> {
    let token = config::read_token().ok_or_else(|| {
        anyhow::anyhow!("Zestful app not running or not configured. Token not found.")
    })?;
    let port = config::read_port();

    // Use explicit URI if provided, otherwise auto-detect via workspace inspector,
    // falling back to saved URI file (written by `zestful ssh` for remote sessions)
    let terminal_uri = terminal_uri
        .or_else(|| crate::workspace::locate().ok())
        .or_else(|| config::read_terminal_uri());

    crate::log::log(
        "notify",
        &format!(
            "agent={} severity={} message=\"{}\" uri={} push={}",
            agent,
            severity,
            message,
            terminal_uri.as_deref().unwrap_or("none"),
            !no_push
        ),
    );

    if debug {
        eprintln!("zestful: uri={}", terminal_uri.as_deref().unwrap_or("none"));
    }

    // `send` already swallows its own network errors and always returns Ok;
    // the discard makes the fire-and-forget intent explicit and matches the
    // pattern already used in cmd/watch.rs.
    let _ = send(
        &token,
        port,
        &agent,
        &message,
        &severity,
        terminal_uri.clone(),
        no_push,
    );

    // Also emit a structured event to the daemon (PR1 of legacy-/notify retirement).
    // Best-effort: errors are logged and swallowed — failure to reach the daemon must
    // never break the user-facing notify path. Mirror cmd/hook.rs.
    let envelopes = crate::events::map_cli_notify(&agent, &message, terminal_uri);
    if let Err(e) = crate::events::send_to_daemon(&envelopes) {
        crate::log::log("notify", &format!("event emission failed: {}", e));
    }

    Ok(())
}

/// Send a notification to the Zestful app. Used by both `notify` and `watch` commands.
pub fn send(
    token: &str,
    port: u16,
    agent: &str,
    message: &str,
    severity: &str,
    terminal_uri: Option<String>,
    no_push: bool,
) -> Result<()> {
    let body = NotifyBody {
        agent: agent.to_string(),
        message: message.to_string(),
        severity: severity.to_string(),
        terminal_uri,
        push: !no_push,
    };

    let json = serde_json::to_string(&body)?;

    crate::log::log("notify", &format!("sending {} bytes: {}", json.len(), json));

    // Raw TCP to avoid HTTP library connection pooling issues
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let request = format!(
        "POST /notify HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Content-Type: application/json\r\n\
         X-Zestful-Token: {}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        port,
        token,
        json.len(),
        json
    );

    match TcpStream::connect(format!("127.0.0.1:{}", port)) {
        Ok(mut stream) => {
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .ok();
            if let Err(e) = stream.write_all(request.as_bytes()) {
                crate::log::log("notify", &format!("write error: {}", e));
            } else {
                let mut response = Vec::new();
                let _ = stream.read_to_end(&mut response);
                let resp = String::from_utf8_lossy(&response);
                if let Some(status_line) = resp.lines().next() {
                    if !status_line.contains("200") {
                        crate::log::log("notify", &format!("app returned: {}", status_line));
                    }
                }
            }
        }
        Err(e) => {
            crate::log::log("notify", &format!("could not reach app ({})", e));
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
            terminal_uri: Some("terminal://iterm2/window:1/tab:2".into()),
            push: true,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["agent"], "test-agent");
        assert_eq!(json["message"], "hello world");
        assert_eq!(json["severity"], "warning");
        assert_eq!(json["terminal_uri"], "terminal://iterm2/window:1/tab:2");
        // push=true should be skipped
        assert!(json.get("push").is_none());
    }

    #[test]
    fn test_notify_body_serialization_minimal() {
        let body = NotifyBody {
            agent: "test".into(),
            message: "msg".into(),
            severity: "info".into(),
            terminal_uri: None,
            push: false,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["agent"], "test");
        assert!(json.get("terminal_uri").is_none());
        assert_eq!(json["push"], false);
    }

    #[test]
    fn test_notify_body_special_chars() {
        let body = NotifyBody {
            agent: "test".into(),
            message: "hello \"world\"\nnewline".into(),
            severity: "info".into(),
            terminal_uri: None,
            push: true,
        };
        let json_str = serde_json::to_string(&body).unwrap();
        assert!(json_str.contains("\\\"world\\\""));
        assert!(json_str.contains("\\n"));
    }

    #[test]
    fn test_send_no_server_returns_ok() {
        let result = send("fake-token", 19999, "test", "msg", "info", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_send_with_terminal_uri() {
        let result = send(
            "fake-token",
            19999,
            "test",
            "msg",
            "info",
            Some("terminal://iterm2/window:1/tab:2".into()),
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
            terminal_uri: None,
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
            terminal_uri: None,
            push: true,
        };
        let json_str = serde_json::to_string(&body).unwrap();
        assert!(!json_str.contains("push"));
    }
}

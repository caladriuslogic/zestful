//! POST a batch of event envelopes to `127.0.0.1:21548/events`.
//!
//! Uses raw TCP (matching `cmd/notify.rs`) to avoid HTTP client connection
//! pooling issues in short-lived CLI processes. Best-effort: errors are
//! logged via `crate::log::log` and never propagate.

use crate::config;
use crate::events::envelope::Envelope;
use anyhow::Result;
use serde::Serialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Serialize)]
struct Batch<'a> {
    events: &'a [Envelope],
}

/// Send a batch of envelopes to the daemon's `/events` endpoint.
///
/// Returns `Err` only when serialization fails (which would be a bug). All
/// network errors are logged and converted to `Ok(())` so this never blocks
/// or fails a hook.
pub fn send_to_daemon(envelopes: &[Envelope]) -> Result<()> {
    if envelopes.is_empty() {
        return Ok(());
    }

    let token = match config::read_token() {
        Some(t) => t,
        None => {
            crate::log::log("events", "no token available; skipping /events POST");
            return Ok(());
        }
    };

    let body = serde_json::to_string(&Batch { events: envelopes })?;
    let port = config::daemon_port();
    post_raw(&token, port, &body);
    Ok(())
}

fn post_raw(token: &str, port: u16, body: &str) {
    let request = format!(
        "POST /events HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         X-Zestful-Token: {token}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        port = port,
        token = token,
        len = body.len(),
        body = body,
    );

    match TcpStream::connect(format!("127.0.0.1:{}", port)) {
        Ok(mut stream) => {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
            if let Err(e) = stream.write_all(request.as_bytes()) {
                crate::log::log("events", &format!("write error: {}", e));
                return;
            }
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response);
            let resp = String::from_utf8_lossy(&response);
            if let Some(status_line) = resp.lines().next() {
                if !status_line.contains("200") {
                    crate::log::log("events", &format!("daemon returned: {}", status_line));
                }
            }
        }
        Err(e) => {
            crate::log::log("events", &format!("could not reach daemon ({})", e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::envelope::Envelope;

    fn sample_envelope() -> Envelope {
        Envelope {
            id: "01JGYK8F3N7WA9QVXR2PB5HM4D".into(),
            schema: 1,
            ts: 1,
            seq: 0,
            host: "h".into(),
            os_user: "u".into(),
            device_id: "d".into(),
            source: "claude-code".into(),
            source_pid: 1,
            type_: "turn.completed".into(),
            correlation: None,
            context: None,
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn send_empty_slice_is_ok() {
        assert!(send_to_daemon(&[]).is_ok());
    }

    #[test]
    fn send_with_no_daemon_returns_ok() {
        // No daemon listening on 21548 in test env → post_raw logs and returns.
        // send_to_daemon itself only fails on serialization bugs, which don't
        // happen with well-formed envelopes.
        let env = sample_envelope();
        assert!(send_to_daemon(&[env]).is_ok());
    }

    #[test]
    fn batch_serializes_as_events_array() {
        let env = sample_envelope();
        let json = serde_json::to_string(&Batch {
            events: std::slice::from_ref(&env),
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["events"].is_array());
        assert_eq!(parsed["events"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["events"][0]["type"], "turn.completed");
    }
}

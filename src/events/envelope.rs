//! Event envelope types per docs/superpowers/specs/2026-04-21-event-protocol-design.md §Envelope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    pub id: String,
    pub schema: u32,
    pub ts: u64,
    pub seq: u64,
    pub host: String,
    pub os_user: String,
    pub device_id: String,
    pub source: String,
    pub source_pid: u32,

    /// Tagged union discriminator; the full tagged `Payload` enum lands in Task 4.
    /// Kept as a string at this layer so this type is reusable by the daemon's
    /// ingestion handler without locking into the v1 payload set.
    #[serde(rename = "type")]
    pub type_: String,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub correlation: Option<Correlation>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context: Option<Context>,

    /// Type-specific payload, kept as a raw `serde_json::Value` at this layer
    /// so schema evolution doesn't require changing this struct.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Correlation {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub application: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub application_instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subapplication: Option<Subapplication>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub focus_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Subapplication {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub window: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pane: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_minimal_roundtrip() {
        let env = Envelope {
            id: "01JGYK8F3N7WA9QVXR2PB5HM4D".into(),
            schema: 1,
            ts: 1745183677234,
            seq: 0,
            host: "morrow.local".into(),
            os_user: "jmorrow".into(),
            device_id: "d_01JGYJ12345".into(),
            source: "claude-code".into(),
            source_pid: 83421,
            type_: "turn.completed".into(),
            correlation: None,
            context: None,
            payload: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn envelope_type_field_is_literal_type() {
        let env = Envelope {
            id: "01JGYK8F3N7WA9QVXR2PB5HM4D".into(),
            schema: 1,
            ts: 1,
            seq: 0,
            host: "h".into(),
            os_user: "u".into(),
            device_id: "d".into(),
            source: "s".into(),
            source_pid: 1,
            type_: "turn.completed".into(),
            correlation: None,
            context: None,
            payload: serde_json::Value::Null,
        };
        let v: serde_json::Value = serde_json::to_value(&env).unwrap();
        assert_eq!(v["type"], "turn.completed");
        assert!(v.get("type_").is_none(), "type_ must be serialized as 'type'");
    }

    #[test]
    fn envelope_omits_optional_when_none() {
        let env = Envelope {
            id: "01JGYK8F3N7WA9QVXR2PB5HM4D".into(),
            schema: 1,
            ts: 1,
            seq: 0,
            host: "h".into(),
            os_user: "u".into(),
            device_id: "d".into(),
            source: "s".into(),
            source_pid: 1,
            type_: "x.y".into(),
            correlation: None,
            context: None,
            payload: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("correlation"));
        assert!(!json.contains("context"));
        assert!(!json.contains("payload"));
    }

    #[test]
    fn context_with_subapplication_roundtrip() {
        let ctx = Context {
            application: Some("iTerm2".into()),
            subapplication: Some(Subapplication {
                kind: "tmux".into(),
                session: Some("main".into()),
                window: Some("2".into()),
                pane: Some("0".into()),
            }),
            shell: Some("zsh".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: Context = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
        // Subapplication shape sanity
        let v: serde_json::Value = serde_json::to_value(&ctx).unwrap();
        assert_eq!(v["subapplication"]["kind"], "tmux");
        assert_eq!(v["subapplication"]["session"], "main");
    }

    #[test]
    fn correlation_partial_roundtrip() {
        let c = Correlation {
            session_id: Some("s_1".into()),
            turn_id: Some("t_2".into()),
            tool_use_id: None,
            parent_id: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("session_id"));
        assert!(!json.contains("tool_use_id"));
        let back: Correlation = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}

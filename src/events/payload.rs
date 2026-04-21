//! Payload tagged-union enum covering the v1 event types per
//! docs/superpowers/specs/2026-04-21-event-protocol-design.md §Taxonomy.
//!
//! Used by emitters to construct typed payloads that serialize into the
//! `Envelope.payload` field (a `serde_json::Value`). The daemon's ingestion
//! path does NOT deserialize into this enum — unknown types must be accepted
//! for forward-compat, so the daemon works at the `serde_json::Value` layer.

use serde::{Deserialize, Serialize};

/// Tagged union of all v1 event payloads. The serialization format uses the
/// `type` field as the discriminator, matching the wire format in
/// `Envelope.type_`.
///
/// Emitters construct one of these, serialize to `serde_json::Value`, and
/// assign the value to `Envelope.payload` while also setting `Envelope.type_`
/// to the matching tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Payload {
    #[serde(rename = "turn.prompt_submitted")]
    TurnPromptSubmitted(TurnPromptSubmitted),

    #[serde(rename = "turn.completed")]
    TurnCompleted(TurnCompleted),

    #[serde(rename = "turn.errored")]
    TurnErrored(TurnErrored),

    #[serde(rename = "tool.invoked")]
    ToolInvoked(ToolInvoked),

    #[serde(rename = "tool.completed")]
    ToolCompleted(ToolCompleted),

    #[serde(rename = "permission.requested")]
    PermissionRequested(PermissionRequested),

    #[serde(rename = "agent.notified")]
    AgentNotified(AgentNotified),

    #[serde(rename = "session.started")]
    SessionStarted(SessionStarted),
}

impl Payload {
    /// The matching `Envelope.type_` value for this payload variant.
    pub fn type_str(&self) -> &'static str {
        match self {
            Payload::TurnPromptSubmitted(_) => "turn.prompt_submitted",
            Payload::TurnCompleted(_) => "turn.completed",
            Payload::TurnErrored(_) => "turn.errored",
            Payload::ToolInvoked(_) => "tool.invoked",
            Payload::ToolCompleted(_) => "tool.completed",
            Payload::PermissionRequested(_) => "permission.requested",
            Payload::AgentNotified(_) => "agent.notified",
            Payload::SessionStarted(_) => "session.started",
        }
    }

    /// Serialize this payload's *body* (the struct, without the tag) into a
    /// `serde_json::Value` suitable for `Envelope.payload`. The tag is
    /// communicated via `Envelope.type_` instead.
    pub fn to_body_value(&self) -> serde_json::Value {
        match self {
            Payload::TurnPromptSubmitted(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::TurnCompleted(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::TurnErrored(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::ToolInvoked(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::ToolCompleted(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::PermissionRequested(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::AgentNotified(p) => serde_json::to_value(p).unwrap_or_default(),
            Payload::SessionStarted(p) => serde_json::to_value(p).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnPromptSubmitted {
    pub prompt_preview: String,
    pub prompt_hash: String,
    /// Optional well-known convention: human-readable note from the emitter.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnCompleted {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_message_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnErrored {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolInvoked {
    pub tool_name: String,
    pub args_preview: String,
    pub args_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolCompleted {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PermissionRequested {
    /// "tool" | "write" | "other"
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AgentNotified {
    /// "notification" | "elicitation" | "other"
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionStarted {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_str_matches_tag() {
        let cases: Vec<(Payload, &str)> = vec![
            (
                Payload::TurnPromptSubmitted(TurnPromptSubmitted::default()),
                "turn.prompt_submitted",
            ),
            (
                Payload::TurnCompleted(TurnCompleted::default()),
                "turn.completed",
            ),
            (
                Payload::TurnErrored(TurnErrored::default()),
                "turn.errored",
            ),
            (
                Payload::ToolInvoked(ToolInvoked::default()),
                "tool.invoked",
            ),
            (
                Payload::ToolCompleted(ToolCompleted::default()),
                "tool.completed",
            ),
            (
                Payload::PermissionRequested(PermissionRequested::default()),
                "permission.requested",
            ),
            (
                Payload::AgentNotified(AgentNotified::default()),
                "agent.notified",
            ),
            (
                Payload::SessionStarted(SessionStarted::default()),
                "session.started",
            ),
        ];
        for (p, tag) in cases {
            assert_eq!(p.type_str(), tag);
        }
    }

    #[test]
    fn tagged_serialization_shape() {
        let p = Payload::ToolInvoked(ToolInvoked {
            tool_name: "Bash".into(),
            args_preview: "ls -la".into(),
            args_hash: "abc123".into(),
            message: None,
        });
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["type"], "tool.invoked");
        assert_eq!(v["tool_name"], "Bash");
    }

    #[test]
    fn body_value_omits_tag() {
        let p = Payload::ToolInvoked(ToolInvoked {
            tool_name: "Bash".into(),
            args_preview: "ls -la".into(),
            args_hash: "abc123".into(),
            message: None,
        });
        let body = p.to_body_value();
        assert!(body.get("type").is_none(), "body must not include tag");
        assert_eq!(body["tool_name"], "Bash");
        assert_eq!(body["args_preview"], "ls -la");
        assert_eq!(body["args_hash"], "abc123");
    }

    #[test]
    fn turn_completed_omits_none_fields() {
        let p = Payload::TurnCompleted(TurnCompleted::default());
        let body = p.to_body_value();
        // All fields are Optional; body should be an empty object
        assert!(body.is_object());
        assert_eq!(body.as_object().unwrap().len(), 0);
    }

    #[test]
    fn agent_notified_optional_message() {
        let with_msg = Payload::AgentNotified(AgentNotified {
            kind: "notification".into(),
            message: Some("Waiting for your input".into()),
        });
        let body = with_msg.to_body_value();
        assert_eq!(body["kind"], "notification");
        assert_eq!(body["message"], "Waiting for your input");

        let without_msg = Payload::AgentNotified(AgentNotified {
            kind: "other".into(),
            message: None,
        });
        let body = without_msg.to_body_value();
        assert!(body.get("message").is_none());
    }
}

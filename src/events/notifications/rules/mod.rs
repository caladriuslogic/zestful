//! Rule registry. Each rule is a Send+Sync Rust type implementing the
//! Rule trait; the registry is a slice of references to per-rule static
//! instances.

pub mod agent_completed;
pub mod agent_notified;
pub mod permission_pending;

use crate::events::notifications::rule::Rule;

pub static ALL_RULES: &[&dyn Rule] = &[
    &agent_completed::AgentCompleted,
    &agent_notified::AgentNotified,
    &permission_pending::PermissionPending,
];

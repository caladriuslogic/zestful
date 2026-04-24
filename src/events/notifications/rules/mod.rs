//! Rule registry. Day-one rules will be registered here in Tasks 2–4.
//! Each rule is a Send+Sync Rust type implementing the Rule trait; the
//! registry is a slice of references to per-rule static instances.

use crate::events::notifications::rule::Rule;

/// All rules that run on every `compute` call. Later tasks append here.
pub static ALL_RULES: &[&dyn Rule] = &[];

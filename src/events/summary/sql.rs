//! SQL helpers for the summary projection: window boundaries and
//! bucket math for the 24h cost sparkline. Splitting these out from
//! `mod.rs` keeps the projection logic and the SQL queries independently
//! reviewable.

// Implemented in Task 3.

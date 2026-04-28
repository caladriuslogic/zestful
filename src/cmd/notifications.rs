//! `zestful notifications` — read the notifications projection from the
//! local event store. Parallel to `cmd/tiles.rs`.

use crate::events::notifications;
use crate::events::notifications::notification::Notification;
use crate::events::notifications::rule::Severity;

pub fn run(
    agent: Option<String>,
    rule: Option<String>,
    severity: Option<String>,
    since: Option<i64>,
    json: bool,
) -> anyhow::Result<()> {
    let db_path = crate::config::config_dir().join("events.db");
    if !db_path.exists() {
        anyhow::bail!(
            "event store not found at {}. Is the daemon running?",
            db_path.display()
        );
    }
    crate::events::store::init(&db_path)?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let since_ms = since.unwrap_or(now_ms - 24 * 3_600_000);

    let mut all = {
        let c = crate::events::store::conn().lock().unwrap();
        notifications::compute(&c, since_ms)?
    };

    if let Some(a) = &agent {
        all.retain(|n| &n.agent == a);
    }
    if let Some(r) = &rule {
        all.retain(|n| &n.rule_id == r);
    }
    if let Some(sev_str) = &severity {
        let min = parse_severity(sev_str).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --severity {}: expected info | warn | urgent",
                sev_str
            )
        })?;
        all.retain(|n| severity_rank(&n.severity) >= severity_rank(&min));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
    } else {
        print_table(&all, now_ms);
    }
    Ok(())
}

fn parse_severity(s: &str) -> Option<Severity> {
    match s.to_ascii_lowercase().as_str() {
        "info" => Some(Severity::Info),
        "warn" => Some(Severity::Warn),
        "urgent" => Some(Severity::Urgent),
        _ => None,
    }
}

fn severity_rank(s: &Severity) -> u8 {
    match s {
        Severity::Info => 0,
        Severity::Warn => 1,
        Severity::Urgent => 2,
    }
}

fn print_table(ns: &[Notification], now_ms: i64) {
    println!(
        "{:<22} {:<20} {:<22} {:<8} {:<40} {}",
        "agent", "project", "rule", "severity", "message", "triggered"
    );
    for n in ns {
        println!(
            "{:<22} {:<20} {:<22} {:<8} {:<40} {}",
            truncate(&n.agent, 22),
            truncate(n.project_label.as_deref().unwrap_or("-"), 20),
            truncate(&n.rule_id, 22),
            n.severity,
            truncate(&n.message, 40),
            relative_time(n.triggered_at_ms, now_ms),
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{}…", cut)
}

fn relative_time(then_ms: i64, now_ms: i64) -> String {
    let delta = (now_ms - then_ms).max(0);
    if delta < 60_000 {
        return format!("{}s ago", delta / 1000);
    }
    if delta < 3_600_000 {
        return format!("{}m ago", delta / 60_000);
    }
    if delta < 86_400_000 {
        return format!("{}h ago", delta / 3_600_000);
    }
    format!("{}d ago", delta / 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_severity_accepts_known_values() {
        assert_eq!(parse_severity("info"), Some(Severity::Info));
        assert_eq!(parse_severity("WARN"), Some(Severity::Warn));
        assert_eq!(parse_severity("Urgent"), Some(Severity::Urgent));
    }

    #[test]
    fn parse_severity_rejects_unknown_values() {
        assert_eq!(parse_severity("critical"), None);
        assert_eq!(parse_severity(""), None);
    }

    #[test]
    fn severity_rank_orders_ascending() {
        assert!(severity_rank(&Severity::Info) < severity_rank(&Severity::Warn));
        assert!(severity_rank(&Severity::Warn) < severity_rank(&Severity::Urgent));
    }

    #[test]
    fn truncate_handles_short_and_long() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this-is-way-too-long", 10), "this-is-w…");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(1000, 5000), "4s ago");
        assert_eq!(relative_time(0, 90_000), "1m ago");
    }
}

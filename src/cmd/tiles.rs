//! `zestful tiles` — read the tiles projection from the local store.

use crate::events::tiles;

pub fn run(agent: Option<String>, since: Option<i64>, json: bool) -> anyhow::Result<()> {
    // Open store directly (same pattern as cmd::events::run).
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
        tiles::compute(&c, since_ms)?
    };
    if let Some(a) = &agent {
        all.retain(|t| &t.agent == a);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
    } else {
        print_table(&all, now_ms);
    }
    Ok(())
}

fn print_table(tiles: &[tiles::tile::Tile], now_ms: i64) {
    println!(
        "{:<22} {:<25} {:<35} {:<12} {}",
        "agent", "project", "surface", "last_seen", "events"
    );
    for t in tiles {
        println!(
            "{:<22} {:<25} {:<35} {:<12} {}",
            truncate(&t.agent, 22),
            truncate(t.project_label.as_deref().unwrap_or("-"), 25),
            truncate(&t.surface_label, 35),
            relative_time(t.last_seen_at, now_ms),
            t.event_count,
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
    fn truncate_handles_short_and_long() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this-is-way-too-long", 10), "this-is-w…");
    }

    #[test]
    fn truncate_handles_multibyte() {
        // 5 chars × 3 bytes each = 15 bytes; n=10 chars → no truncation.
        assert_eq!(truncate("日本語テスト", 10), "日本語テスト");
        assert_eq!(truncate("日本語テスト", 3), "日本…");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(1000, 5000), "4s ago");
        assert_eq!(relative_time(0, 90_000), "1m ago");
        assert_eq!(relative_time(0, 7_200_000), "2h ago");
        assert_eq!(relative_time(0, 172_800_000), "2d ago");
    }
}

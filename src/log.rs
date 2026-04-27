//! Simple file logger. Appends timestamped lines to `~/.config/zestful/zestful.log`.

use crate::config;
use std::fs::{self, OpenOptions};
use std::io::Write;

/// Log a message to the central log file.
/// Format: `2026-03-31T15:30:00 [component] message`
pub fn log(component: &str, message: &str) {
    let timestamp = now();
    let line = format!("{} [{}] {}\n", timestamp, component, message);

    let log_path = config::config_dir().join("zestful.log");

    // Ensure config dir exists
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Log a message using a caller-supplied timestamp (unix milliseconds).
/// Used when relaying logs from another process/context whose own clock
/// recorded the original event time — e.g., the Chrome extension shipping
/// logs after the service worker woke from sleep. Format matches `log()`.
pub fn log_with_ts(ts_ms: i64, component: &str, message: &str) {
    let timestamp = format_iso_ms(ts_ms);
    let line = format!("{} [{}] {}\n", timestamp, component, message);

    let log_path = config::config_dir().join("zestful.log");

    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Format unix-ms as ISO-8601 to match `now()`'s output.
fn format_iso_ms(ts_ms: i64) -> String {
    let secs = (ts_ms / 1000).max(0) as u64;
    let millis = (ts_ms % 1000).max(0) as u64;
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_date(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        year, month, day, hours, minutes, seconds, millis
    )
}

/// ISO-8601 timestamp without external dependencies.
fn now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();

    // Convert to broken-down time (UTC)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to Y-M-D (simplified, good enough for logging)
    let (year, month, day) = days_to_date(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        year, month, day, hours, minutes, seconds, millis
    )
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

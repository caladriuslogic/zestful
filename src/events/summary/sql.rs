//! SQL helpers for the summary projection: window boundaries and
//! bucket math for the 24h cost sparkline. Splitting these out from
//! `mod.rs` keeps the projection logic and the SQL queries independently
//! reviewable.

use chrono::{Local, TimeZone};

/// Calendar-local midnight that started "today" — the moment used as
/// the lower bound for the today window. Returns unix-ms.
///
/// `now_ms` is the wall clock; passing it in (rather than reading the
/// clock) keeps this pure for tests.
pub fn today_window_ms(now_ms: i64) -> i64 {
    let now = Local.timestamp_millis_opt(now_ms).single()
        .expect("valid unix-ms");
    let naive_midnight = now.date_naive().and_hms_opt(0, 0, 0)
        .expect("00:00:00 always valid");
    // Local midnight may be `None` (DST spring-forward at midnight) or
    // `Ambiguous` (fall-back through midnight). In either rare case we
    // pick the earliest valid local instant rather than panic.
    use chrono::offset::LocalResult;
    let midnight = match naive_midnight.and_local_timezone(Local) {
        LocalResult::Single(dt)        => dt,
        LocalResult::Ambiguous(a, _)   => a,
        LocalResult::None              => Local
            .from_local_datetime(&naive_midnight)
            .earliest()
            .unwrap_or_else(|| Local.timestamp_millis_opt(now_ms).single()
                .expect("valid unix-ms")),
    };
    midnight.timestamp_millis()
}

/// Map a turn.metrics timestamp to a 0..7 sparkline bucket within the
/// last 24h ending at `now_ms`. Returns `None` if `ts_ms` is older than
/// 24h or in the future.
///
/// Bucket 0 = oldest (24h ago); bucket 6 = newest (containing `now_ms`).
/// Bucket size = 24h / 7 ≈ 3 h 25 m.
///
/// Boundary: `ts == now - span` returns `None`; `ts == now` lands in
/// bucket 6. The `(now-span, now]` half-open window is intentional —
/// callers passing `event_ts >= now - 24h` from SQL get well-defined
/// bucket assignments without any cell straddling the cutoff.
pub fn bucket_idx(ts_ms: i64, now_ms: i64) -> Option<usize> {
    let span = 24 * 3_600_000i64;
    if ts_ms > now_ms || ts_ms <= now_ms - span { return None; }
    let bucket_size = span / 7;
    let offset_from_start = ts_ms - (now_ms - span);
    let idx = (offset_from_start / bucket_size).min(6) as usize;
    Some(idx)
}

/// SQL fragment selecting turn.metrics rows in the today window. Used
/// by `compute()` for the 4 today-scoped aggregates.
///
/// Bound parameters: 1 = today_window_ms.
pub const TODAY_METRICS_SQL: &str = "
    SELECT context, payload
    FROM events
    WHERE event_type = 'turn.metrics'
      AND event_ts >= ?
";

/// SQL fragment selecting turn.metrics rows in the 24h sparkline window.
/// Used by `compute()` for the cost sparkline.
///
/// Bound parameters: 1 = now_ms - 24h.
pub const SPARKLINE_METRICS_SQL: &str = "
    SELECT event_ts, payload
    FROM events
    WHERE event_type = 'turn.metrics'
      AND event_ts >= ?
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_idx_now_is_last_bucket() {
        let now_ms = 1_761_830_000_000;
        assert_eq!(bucket_idx(now_ms, now_ms), Some(6));
    }

    #[test]
    fn bucket_idx_24h_ago_is_first_bucket() {
        let now_ms = 1_761_830_000_000;
        let just_inside = now_ms - 24 * 3_600_000 + 1;
        assert_eq!(bucket_idx(just_inside, now_ms), Some(0));
    }

    #[test]
    fn bucket_idx_older_than_24h_is_none() {
        let now_ms = 1_761_830_000_000;
        let older = now_ms - 24 * 3_600_000 - 1;
        assert_eq!(bucket_idx(older, now_ms), None);
    }

    #[test]
    fn bucket_idx_future_is_none() {
        let now_ms = 1_761_830_000_000;
        assert_eq!(bucket_idx(now_ms + 1, now_ms), None);
    }

    #[test]
    fn bucket_idx_partitions_24h_into_seven() {
        // Sample one ts per bucket, verify mapping covers 0..7 exhaustively.
        let now_ms = 1_761_830_000_000;
        let span = 24 * 3_600_000;
        let bucket_size = span / 7;
        let mut seen = [false; 7];
        for i in 0..7 {
            // Pick a ts in the middle of bucket i (counting from oldest).
            let offset_from_start = i * bucket_size + bucket_size / 2;
            let ts = (now_ms - span) + offset_from_start;
            let idx = bucket_idx(ts, now_ms).expect("in window");
            seen[idx] = true;
        }
        assert_eq!(seen, [true; 7]);
    }

    #[test]
    fn today_window_ms_is_at_or_before_now() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_millis() as i64;
        let midnight = today_window_ms(now_ms);
        assert!(midnight <= now_ms, "midnight ({}) > now ({})", midnight, now_ms);
        // Midnight should be at most ~25 hours ago (DST gives extra hour).
        assert!(now_ms - midnight < 25 * 3_600_000);
    }

    #[test]
    fn today_window_ms_is_aligned_to_midnight() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_millis() as i64;
        let midnight = today_window_ms(now_ms);
        // The returned ms should correspond to a local time of 00:00:00.000.
        use chrono::{Local, TimeZone, Timelike};
        let dt = Local.timestamp_millis_opt(midnight).single().expect("valid");
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
        assert_eq!(dt.second(), 0);
    }
}

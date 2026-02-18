use chrono::{Datelike, Local, TimeZone, Utc};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current UNIX timestamp in milliseconds (UTC).
pub fn now_ts_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0));

    let ms = dur.as_millis();
    if ms > i64::MAX as u128 {
        i64::MAX
    } else {
        ms as i64
    }
}

/// Current local day string in system timezone (e.g. "2026-02-17").
/// Use this for daily memory partitioning.
pub fn local_day_string() -> String {
    let now = Local::now();
    format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day())
}

/// Current UTC day string (e.g. "2026-02-17").
pub fn utc_day_string() -> String {
    let now = Utc::now();
    format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day())
}

/// Convert UNIX ms to local day string (YYYY-MM-DD).
pub fn local_day_from_ts_ms(ts_ms: i64) -> String {
    let dt = Local.timestamp_millis_opt(ts_ms).single();
    match dt {
        Some(dt) => format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day()),
        None => local_day_string(),
    }
}

/// Convert UNIX ms to UTC day string (YYYY-MM-DD).
pub fn utc_day_from_ts_ms(ts_ms: i64) -> String {
    let dt = Utc.timestamp_millis_opt(ts_ms).single();
    match dt {
        Some(dt) => format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day()),
        None => utc_day_string(),
    }
}

/// Difference in milliseconds between now and a timestamp.
pub fn age_ms(ts_ms: i64) -> i64 {
    now_ts_ms().saturating_sub(ts_ms)
}

/// Returns true if `ts_ms` is older than `threshold_ms`.
pub fn is_older_than(ts_ms: i64, threshold_ms: i64) -> bool {
    age_ms(ts_ms) > threshold_ms
}

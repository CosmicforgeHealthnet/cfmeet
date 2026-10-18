//! Fixed-window counters in Messaging's own database (multi-instance safe: one atomic
//! upsert per hit). A refused hit is still counted, so hammering never resets a window.

use chrono::{DateTime, DurationRound, Utc};
use sea_orm::ConnectionTrait;

use crate::{db, error::Result};

/// Window length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Minute,
    Hour,
}

impl Window {
    fn duration(&self) -> chrono::Duration {
        match self {
            Window::Minute => chrono::Duration::minutes(1),
            Window::Hour => chrono::Duration::hours(1),
        }
    }
}

/// Counts one hit on `key` and returns `Some(retry_after_seconds)` when the window's
/// count now exceeds `limit`.
pub async fn hit<C: ConnectionTrait>(
    db: &C,
    key: &str,
    window: Window,
    limit: i64,
    now: DateTime<Utc>,
) -> Result<Option<u64>> {
    let length = window.duration();
    let start = now.duration_trunc(length).unwrap_or(now);
    let key: String = key.chars().take(160).collect();
    let count = db::scalar_i64(
        db,
        "INSERT INTO messaging_rate_windows (window_key, window_start, count) VALUES ($1, $2, 1) \
         ON CONFLICT (window_key, window_start) DO UPDATE SET count = messaging_rate_windows.count + 1 \
         RETURNING count",
        vec![key.into(), start.into()],
    )
    .await?;
    if count > limit {
        crate::metrics::inc("messaging_rate_limited_total", &[]);
        Ok(Some((start + length - now).num_seconds().max(1) as u64))
    } else {
        Ok(None)
    }
}

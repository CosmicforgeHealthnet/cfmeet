//! Service-side routing (`notif_routing`, Document 4 §6 table 7, §9.6): the mode of each
//! (routing category, channel). Rows are changed only by coordinated-release operations
//! (`activate-notifications-*`), each bumping `version` and recording the run.
//!
//! * `OFF` — nothing is delivered; events for a category whose channels are all OFF are
//!   **HELD** at ingest (stored, never sent, alerted), never dropped.
//! * `SHADOW` — the full pipeline runs (preferences, rendering, routing) and deliveries are
//!   recorded as `SHADOW` with a render digest for the comparison; no provider is called.
//! * `ACTIVE` — delivered.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use serde::Serialize;

use crate::{catalogue::Channel, db};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Mode {
    Off,
    Shadow,
    Active,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "ACTIVE" => Mode::Active,
            "SHADOW" => Mode::Shadow,
            _ => Mode::Off,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "OFF",
            Mode::Shadow => "SHADOW",
            Mode::Active => "ACTIVE",
        }
    }
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingRow {
    pub category: String,
    pub channel: String,
    pub mode: String,
    pub version: i32,
    pub canary_percent: i16,
    pub fence_at: DateTime<Utc>,
    pub reminder_boundary: Option<DateTime<Utc>>,
    pub changed_by_run: String,
    pub changed_at: DateTime<Utc>,
}

pub async fn rows<C: ConnectionTrait>(conn: &C) -> Result<Vec<RoutingRow>, DbErr> {
    db::all(
        conn,
        "SELECT category, channel, mode, version, canary_percent, fence_at, reminder_boundary, changed_by_run, changed_at
           FROM notif_routing ORDER BY category, channel",
        vec![],
    )
    .await
}

/// (mode, version) per channel of one routing category.
pub async fn category<C: ConnectionTrait>(
    conn: &C,
    category: &str,
) -> Result<HashMap<Channel, (Mode, i32)>, DbErr> {
    let rows: Vec<RoutingRow> = db::all(
        conn,
        "SELECT category, channel, mode, version, canary_percent, fence_at, reminder_boundary, changed_by_run, changed_at
           FROM notif_routing WHERE category = $1",
        vec![category.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Channel::parse(&r.channel).map(|c| (c, (Mode::parse(&r.mode), r.version))))
        .collect())
}

/// The highest mode over a category's channels (OFF everywhere → HELD at ingest).
pub fn highest(modes: &HashMap<Channel, (Mode, i32)>) -> Mode {
    modes.values().map(|(m, _)| *m).max().unwrap_or(Mode::Off)
}

/// The reminder takeover boundary `B` (Document 4 §9.9), if recorded.
pub async fn reminder_boundary<C: ConnectionTrait>(
    conn: &C,
) -> Result<Option<DateTime<Utc>>, DbErr> {
    let row = conn
        .query_one(db::stmt(
            "SELECT reminder_boundary FROM notif_routing WHERE category = 'MEETING_NOTIFICATION' AND channel = 'EMAIL'",
            vec![],
        ))
        .await?;
    Ok(row
        .and_then(|r| {
            r.try_get::<Option<DateTime<Utc>>>("", "reminder_boundary")
                .ok()
        })
        .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highest_mode_decides_holding() {
        let mut m = HashMap::new();
        assert_eq!(highest(&m), Mode::Off);
        m.insert(Channel::Email, (Mode::Off, 1));
        m.insert(Channel::InApp, (Mode::Shadow, 2));
        assert_eq!(highest(&m), Mode::Shadow);
        m.insert(Channel::Push, (Mode::Active, 3));
        assert_eq!(highest(&m), Mode::Active);
        assert_eq!(Mode::parse("bogus"), Mode::Off);
    }
}

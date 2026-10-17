//! Message and thread reads (`sms.read`): always within the credential's environment
//! (another environment's message or thread is NOT_FOUND), newest first, keyset pages
//! with an opaque cursor.

use base64::Engine;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr};
use uuid::Uuid;

use super::{
    threads::{ThreadRow, THREAD_COLUMNS},
    Direction, SmsError, SmsResult, TransportRow, TRANSPORT_COLUMNS,
};
use crate::db;

pub const DEFAULT_LIMIT: u64 = 50;
pub const MAX_LIMIT: u64 = 100;

/// A keyset position (timestamp, id), opaque to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

impl Cursor {
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "{}:{}",
            self.at.timestamp_micros(),
            self.id.simple()
        ))
    }

    pub fn decode(raw: &str) -> SmsResult<Self> {
        let invalid = || SmsError::Validation("The cursor is invalid.");
        if raw.len() > 200 {
            return Err(invalid());
        }
        let text = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(invalid)?;
        let (micros, id) = text.split_once(':').ok_or_else(invalid)?;
        Ok(Self {
            at: micros
                .parse::<i64>()
                .ok()
                .and_then(DateTime::from_timestamp_micros)
                .ok_or_else(invalid)?,
            id: Uuid::parse_str(id).map_err(|_| invalid())?,
        })
    }
}

pub fn limit(requested: Option<u64>) -> SmsResult<u64> {
    match requested {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(SmsError::Validation("limit must be between 1 and 100.")),
    }
}

/// Filters of a message listing.
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageFilter {
    pub number_resource_id: Option<Uuid>,
    pub direction: Option<Direction>,
    pub thread_id: Option<Uuid>,
}

/// One page of the environment's messages (newest first) and the next cursor.
pub async fn messages<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    filter: MessageFilter,
    after: Option<Cursor>,
    limit: u64,
) -> Result<(Vec<TransportRow>, Option<Cursor>), DbErr> {
    let mut values: Vec<sea_orm::Value> = vec![environment_id.into()];
    let mut clauses = vec!["environment_id = $1".to_string()];
    if let Some(n) = filter.number_resource_id {
        values.push(n.into());
        clauses.push(format!("number_resource_id = ${}", values.len()));
    }
    if let Some(d) = filter.direction {
        values.push(d.as_str().into());
        clauses.push(format!("direction = ${}", values.len()));
    }
    if let Some(t) = filter.thread_id {
        values.push(t.into());
        clauses.push(format!("thread_id = ${}", values.len()));
    }
    if let Some(c) = after {
        values.push(c.at.into());
        values.push(c.id.into());
        clauses.push(format!(
            "(created_at, id) < (${}, ${})",
            values.len() - 1,
            values.len()
        ));
    }
    values.push(((limit + 1) as i64).into());
    let mut rows = db::all::<TransportRow, _>(
        db,
        &format!(
            "SELECT {TRANSPORT_COLUMNS} FROM sms_transports WHERE {} ORDER BY created_at DESC, id DESC LIMIT ${}",
            clauses.join(" AND "),
            values.len()
        ),
        values,
    )
    .await?;
    let next = page(&mut rows, limit, |r| Cursor {
        at: r.created_at,
        id: r.id,
    });
    Ok((rows, next))
}

/// One page of the environment's threads (most recent first).
pub async fn threads<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    number_resource_id: Option<Uuid>,
    after: Option<Cursor>,
    limit: u64,
) -> Result<(Vec<ThreadRow>, Option<Cursor>), DbErr> {
    let mut values: Vec<sea_orm::Value> = vec![environment_id.into()];
    let mut clauses = vec!["environment_id = $1".to_string()];
    if let Some(n) = number_resource_id {
        values.push(n.into());
        clauses.push(format!("number_resource_id = ${}", values.len()));
    }
    if let Some(c) = after {
        values.push(c.at.into());
        values.push(c.id.into());
        clauses.push(format!(
            "(last_message_at, id) < (${}, ${})",
            values.len() - 1,
            values.len()
        ));
    }
    values.push(((limit + 1) as i64).into());
    let mut rows = db::all::<ThreadRow, _>(
        db,
        &format!(
            "SELECT {THREAD_COLUMNS} FROM sms_transport_threads WHERE {} ORDER BY last_message_at DESC, id DESC LIMIT ${}",
            clauses.join(" AND "),
            values.len()
        ),
        values,
    )
    .await?;
    let next = page(&mut rows, limit, |t| Cursor {
        at: t.last_message_at,
        id: t.id,
    });
    Ok((rows, next))
}

fn page<T>(rows: &mut Vec<T>, limit: u64, cursor: impl Fn(&T) -> Cursor) -> Option<Cursor> {
    if rows.len() as u64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(cursor)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_garbage_is_refused() {
        let c = Cursor {
            at: DateTime::from_timestamp_micros(1_900_000_000_123_456).unwrap(),
            id: Uuid::new_v4(),
        };
        assert_eq!(Cursor::decode(&c.encode()).unwrap(), c);
        for bad in ["", "x", "!!!", &"a".repeat(300)] {
            assert!(Cursor::decode(bad).is_err(), "{bad}");
        }
        assert!(limit(Some(0)).is_err() && limit(Some(101)).is_err());
        assert_eq!(limit(None).unwrap(), 50);
    }
}

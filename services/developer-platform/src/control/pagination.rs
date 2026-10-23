//! Cursor pagination shared by every list operation: `?limit=1..100&cursor=<opaque>`,
//! responses `{ "items": [...], "nextCursor": <opaque|null> }`. Cursors are opaque
//! (base64url of a server position); a malformed cursor is a VALIDATION_ERROR.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::http::error::{ApiError, ErrorCode};

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 100;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub limit: usize,
    cursor: Option<String>,
}

impl Page {
    pub fn from_query(q: &PageQuery) -> Result<Self, ApiError> {
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        let cursor = match &q.cursor {
            None => None,
            Some(c) if c.len() <= 128 => Some(
                URL_SAFE_NO_PAD
                    .decode(c)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .ok_or(ApiError(ErrorCode::ValidationError))?,
            ),
            Some(_) => return Err(ApiError(ErrorCode::ValidationError)),
        };
        Ok(Self { limit, cursor })
    }

    /// A sequence cursor (`s:<n>`).
    pub fn cursor_seq(&self) -> Option<i64> {
        self.cursor
            .as_deref()
            .and_then(|c| c.strip_prefix("s:"))
            .and_then(|n| n.parse().ok())
    }

    pub fn seq_cursor(seq: i64) -> String {
        URL_SAFE_NO_PAD.encode(format!("s:{seq}"))
    }

    /// A `(created_at, id)` keyset cursor (`k:<micros>:<uuid>`).
    pub fn cursor_key(&self) -> Option<(DateTime<Utc>, Uuid)> {
        let rest = self.cursor.as_deref()?.strip_prefix("k:")?;
        let (micros, id) = rest.split_once(':')?;
        Some((
            DateTime::<Utc>::from_timestamp_micros(micros.parse().ok()?)?,
            Uuid::parse_str(id).ok()?,
        ))
    }

    pub fn key_cursor(at: DateTime<Utc>, id: Uuid) -> String {
        URL_SAFE_NO_PAD.encode(format!("k:{}:{id}", at.timestamp_micros()))
    }

    /// Whether a cursor was supplied but does not decode to the expected kind.
    pub fn has_cursor(&self) -> bool {
        self.cursor.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_limits_are_bounded() {
        let p = Page::from_query(&PageQuery {
            limit: Some(10),
            cursor: Some(Page::seq_cursor(42)),
        })
        .unwrap();
        assert_eq!(p.cursor_seq(), Some(42));
        let id = Uuid::new_v4();
        let at = Utc::now();
        let k = Page::from_query(&PageQuery {
            limit: None,
            cursor: Some(Page::key_cursor(at, id)),
        })
        .unwrap();
        let (t, i) = k.cursor_key().unwrap();
        assert_eq!(i, id);
        assert_eq!(t.timestamp_micros(), at.timestamp_micros());
        assert!(Page::from_query(&PageQuery {
            limit: Some(0),
            cursor: None
        })
        .is_err());
        assert!(Page::from_query(&PageQuery {
            limit: Some(101),
            cursor: None
        })
        .is_err());
        assert!(Page::from_query(&PageQuery {
            limit: None,
            cursor: Some("%%%".into())
        })
        .is_err());
    }
}

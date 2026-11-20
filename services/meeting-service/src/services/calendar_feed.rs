//! Audit MF2: a private, revocable calendar feed (webcal / ICS subscription) of a person's
//! meetings, for Google Calendar, Outlook, Apple Calendar and any client that subscribes to
//! an iCalendar URL.
//!
//! * One active feed per account (`calendar_feed_tokens`, history
//!   `meeting_audit_closure_migrations`). The URL carries a 256-bit `calf_` token; only its
//!   SHA-256 is stored. Creating a feed again replaces (revokes) the previous URL; deleting it
//!   revokes it. A revoked or unknown token, or an account that is no longer active, is a
//!   plain 404.
//! * The feed is the account's own agenda (`GET /api/v2/me/conferences`, the same access
//!   rules) from 30 days ago to 180 days ahead, at most [`MAX_EVENTS`] occurrences, rendered
//!   in UTC with `METHOD:PUBLISH`. It is read without a live session, so it never counts as a
//!   revocation-checked identity: HEALTHCARE / HIGH_SECURITY meetings are left out, and other
//!   restricted titles follow the agenda's rules. It carries meeting links, never passcodes,
//!   invitation tokens or provider identifiers.
//! * Two-way sync with Google / Microsoft calendars needs provider OAuth credentials that do
//!   not exist; it is not part of this feed.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement, TransactionTrait};
use uuid::Uuid;

use crate::{
    domain::ConferenceStatus,
    dto::v2::AgendaQuery,
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::{ActorContext, ActorKind, RegisteredActor},
        calendar::{effective_end, title_restricted, CalendarService},
        ics::{self, IcsCalendar, IcsEvent, IcsMethod, IcsStatus},
        secrets,
    },
};

pub const TOKEN_PREFIX: &str = "calf_";
/// Most occurrences in one feed.
pub const MAX_EVENTS: usize = 500;
const PAST_DAYS: i64 = 30;
const FUTURE_DAYS: i64 = 180;

/// The feed path of a token (relative to the API origin).
pub fn feed_path(token: &str) -> String {
    format!("/api/v2/calendar-feeds/{token}.ics")
}

/// The state of an account's feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedState {
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

fn unavailable(e: DbErr) -> ApiError {
    // The table is created by `meeting-audit-migration up`; until then the feature is off.
    if e.to_string().contains("calendar_feed_tokens") {
        ApiError::domain(
            ErrorCode::FeatureDisabled,
            "Calendar feeds are not available yet",
        )
    } else {
        e.into()
    }
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Creates the account's feed, revoking any previous one. Returns the token (shown once).
pub async fn create<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    user_id: Uuid,
) -> ApiResult<(String, DateTime<Utc>)> {
    let token = secrets::generate_bearer_secret(TOKEN_PREFIX);
    let now = Utc::now();
    let txn = db.begin().await?;
    txn.execute(stmt(
        "UPDATE calendar_feed_tokens SET revoked_at = $2 WHERE user_id = $1 AND revoked_at IS NULL",
        vec![user_id.into(), now.into()],
    ))
    .await
    .map_err(unavailable)?;
    txn.execute(stmt(
        "INSERT INTO calendar_feed_tokens (id, user_id, token_hash, created_at) VALUES ($1, $2, $3, $4)",
        vec![
            Uuid::new_v4().into(),
            user_id.into(),
            secrets::hash_secret(&token).into(),
            now.into(),
        ],
    ))
    .await
    .map_err(unavailable)?;
    txn.commit().await?;
    Ok((token, now))
}

/// The account's active feed, if any.
pub async fn state<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<Option<FeedState>> {
    let row = db
        .query_one(stmt(
            "SELECT created_at, last_used_at FROM calendar_feed_tokens WHERE user_id = $1 AND revoked_at IS NULL",
            vec![user_id.into()],
        ))
        .await
        .map_err(unavailable)?;
    Ok(match row {
        Some(r) => Some(FeedState {
            created_at: r.try_get("", "created_at")?,
            last_used_at: r.try_get("", "last_used_at")?,
        }),
        None => None,
    })
}

/// Revokes the account's feed. Returns whether one was active.
pub async fn revoke<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<bool> {
    Ok(db
        .execute(stmt(
            "UPDATE calendar_feed_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
            vec![user_id.into()],
        ))
        .await
        .map_err(unavailable)?
        .rows_affected()
        > 0)
}

fn not_found() -> ApiError {
    ApiError::domain(ErrorCode::CalendarFeedNotFound, "Calendar feed not found")
}

/// The account a feed token belongs to (active feed, active account), as the actor the
/// agenda is read for: no session, so never revocation-checked.
async fn feed_actor<C: ConnectionTrait>(db: &C, token: &str) -> ApiResult<ActorContext> {
    if !token.starts_with(TOKEN_PREFIX) || token.len() > 128 {
        return Err(not_found());
    }
    let row = db
        .query_one(stmt(
            "UPDATE calendar_feed_tokens f SET last_used_at = now()
               FROM users u
              WHERE f.token_hash = $1 AND f.revoked_at IS NULL AND u.id = f.user_id
                AND u.status::text = 'active'
          RETURNING u.id, u.email, u.email_verified_at, COALESCE(u.display_name, u.username) AS name",
            vec![secrets::hash_secret(token).into()],
        ))
        .await
        .map_err(unavailable)?
        .ok_or_else(not_found)?;
    let user_id: Uuid = row.try_get("", "id")?;
    let email: String = row.try_get("", "email")?;
    let verified: Option<DateTime<Utc>> = row.try_get("", "email_verified_at")?;
    let mut ctx = ActorContext::anonymous();
    ctx.kind = ActorKind::RegisteredUser(RegisteredActor {
        user_id,
        display_name: row.try_get("", "name")?,
        verified_email: verified.map(|_| email.to_ascii_lowercase()),
        platform_admin: false,
        revocation_checked: false,
        account_session_id: None,
        delegation: None,
    });
    Ok(ctx)
}

/// `GET /api/v2/calendar-feeds/{token}.ics`: the feed body.
pub async fn render<C: ConnectionTrait>(
    db: &C,
    calendar: &CalendarService,
    token: &str,
    app_url: &str,
) -> ApiResult<String> {
    let ctx = feed_actor(db, token).await?;
    let now = Utc::now();
    let mut events = Vec::new();
    let mut cursor = None;
    loop {
        let page = calendar
            .agenda(
                &ctx,
                AgendaQuery {
                    from: Some(now - Duration::days(PAST_DAYS)),
                    to: Some(now + Duration::days(FUTURE_DAYS)),
                    cursor: cursor.take(),
                    limit: Some(200),
                    ..Default::default()
                },
            )
            .await?;
        for e in &page.entries {
            if events.len() >= MAX_EVENTS {
                break;
            }
            let s = &e.space;
            let o = &e.occurrence;
            let url = format!("{}/{}", app_url.trim_end_matches('/'), s.meeting_code);
            let summary = if title_restricted(s) && !e.full {
                "CF Meet meeting".to_string()
            } else {
                s.title.clone()
            };
            let key: String = o
                .occurrence_key
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            events.push(IcsEvent {
                uid: format!("occurrence-{}-{key}@cfmeet.com", s.id.simple()),
                sequence: 0,
                summary,
                description: Some(format!(
                    "CF Meet meeting {}\nJoin: {url}\nAccess is checked when you join.",
                    s.meeting_code
                )),
                url: Some(url),
                start: o.start,
                end: effective_end(o.start, o.end),
                status: if o.status == ConferenceStatus::Cancelled {
                    IcsStatus::Cancelled
                } else {
                    IcsStatus::Confirmed
                },
                rrule: None,
                exdates: vec![],
                recurrence_id: None,
                created: None,
                last_modified: now,
                organizer: None,
                attendees: vec![],
            });
        }
        match page.next_cursor {
            Some(next) if events.len() < MAX_EVENTS => cursor = Some(next),
            _ => break,
        }
    }
    Ok(ics::render(
        &IcsCalendar {
            timezone: None,
            name: Some("CF Meet".into()),
            events,
            timezone_until: None,
            method: IcsMethod::Publish,
        },
        now,
    ))
}

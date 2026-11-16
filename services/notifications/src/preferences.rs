//! The preference authority (roadmap N10; Document 4 §7.1, tables 14–17): account
//! categories, the channel matrix, quiet hours, conversation and contact notification
//! levels. Absent rows mean everything enabled (today's Hub defaults); SECURITY is fixed
//! on and never stored. Every write increments `version` (optimistic concurrency via
//! `expectedVersion`) and appends to the 7-day change log used for the rollback
//! write-back to the Hub (Document 5 §6, 4.2).

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{catalogue::PrefCategory, db};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietHours {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub days: Vec<String>,
}

pub const DAYS: [&str; 7] = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];

impl QuietHours {
    pub fn validate(&self) -> Result<(), &'static str> {
        let hhmm = |s: &str| s.len() == 5 && NaiveTime::parse_from_str(s, "%H:%M").is_ok();
        if !hhmm(&self.start) || !hhmm(&self.end) {
            return Err("quietHours start and end are HH:MM");
        }
        if self.days.len() > 7 || self.days.iter().any(|d| !DAYS.contains(&d.as_str())) {
            return Err("quietHours days are MON..SUN");
        }
        let mut sorted = self.days.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != self.days.len() {
            return Err("quietHours days must be unique");
        }
        Ok(())
    }

    /// If `at` falls inside the window (in `tz`), the instant the window ends.
    pub fn window_end(&self, at: DateTime<Utc>, tz: &str) -> Option<DateTime<Utc>> {
        if !self.enabled {
            return None;
        }
        let tz: chrono_tz::Tz = tz.parse().unwrap_or(chrono_tz::UTC);
        let local = at.with_timezone(&tz);
        let start = NaiveTime::parse_from_str(&self.start, "%H:%M").ok()?;
        let end = NaiveTime::parse_from_str(&self.end, "%H:%M").ok()?;
        let now_t = local.time();
        let day_of = |d: chrono::Weekday| DAYS[d.num_days_from_monday() as usize];
        let active_day =
            |d: chrono::Weekday| self.days.is_empty() || self.days.iter().any(|x| x == day_of(d));
        let (inside, window_day) = if start <= end {
            (now_t >= start && now_t < end, local.weekday())
        } else if now_t >= start {
            (true, local.weekday())
        } else if now_t < end {
            (true, local.weekday().pred())
        } else {
            (false, local.weekday())
        };
        if !inside || !active_day(window_day) {
            return None;
        }
        let mut end_date = local.date_naive();
        if start > end && now_t >= start {
            end_date = end_date.succ_opt()?;
        }
        let end_local = end_date.and_time(end).and_local_timezone(tz).earliest()?;
        Some(end_local.with_timezone(&Utc))
    }
}

/// Per-category channel switches (`channels[<categoryKey>]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelSwitches {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_app: Option<bool>,
}

/// One account's stored preferences (defaults when no row exists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPrefs {
    pub version: i64,
    pub categories: BTreeMap<&'static str, bool>,
    pub channels: BTreeMap<String, ChannelSwitches>,
    pub quiet_hours: Option<QuietHours>,
}

impl Default for AccountPrefs {
    fn default() -> Self {
        Self {
            version: 0,
            categories: PrefCategory::USER
                .iter()
                .map(|c| (c.json_key(), true))
                .collect(),
            channels: BTreeMap::new(),
            quiet_hours: None,
        }
    }
}

impl AccountPrefs {
    pub fn category_enabled(&self, c: PrefCategory) -> bool {
        match c {
            PrefCategory::Security | PrefCategory::Operator => true,
            _ => self.categories.get(c.json_key()).copied().unwrap_or(true),
        }
    }

    pub fn channel_enabled(&self, c: PrefCategory, channel: crate::catalogue::Channel) -> bool {
        use crate::catalogue::Channel;
        if matches!(c, PrefCategory::Security | PrefCategory::Operator) {
            return true;
        }
        let Some(sw) = self.channels.get(c.json_key()) else {
            return true;
        };
        match channel {
            Channel::Push => sw.push.unwrap_or(true),
            Channel::Email => sw.email.unwrap_or(true),
            Channel::InApp => sw.in_app.unwrap_or(true),
            Channel::Operator => true,
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct PrefRow {
    version: i64,
    direct_messages: bool,
    group_messages: bool,
    incoming_calls: bool,
    missed_calls: bool,
    contact_requests: bool,
    group_invitations: bool,
    meeting_invitations: bool,
    meeting_reminders: bool,
    voicemail: bool,
    organization_updates: bool,
    developer_updates: bool,
    channel_overrides: Value,
    quiet_hours: Option<Value>,
}

pub async fn load<C: ConnectionTrait>(conn: &C, account_ref: &str) -> Result<AccountPrefs, DbErr> {
    let Some(r) = db::one::<PrefRow, _>(
        conn,
        "SELECT * FROM notif_preferences WHERE account_ref = $1",
        vec![account_ref.into()],
    )
    .await?
    else {
        return Ok(AccountPrefs::default());
    };
    let values = [
        r.direct_messages,
        r.group_messages,
        r.incoming_calls,
        r.missed_calls,
        r.contact_requests,
        r.group_invitations,
        r.meeting_invitations,
        r.meeting_reminders,
        r.voicemail,
        r.organization_updates,
        r.developer_updates,
    ];
    Ok(AccountPrefs {
        version: r.version,
        categories: PrefCategory::USER
            .iter()
            .zip(values)
            .map(|(c, v)| (c.json_key(), v))
            .collect(),
        channels: serde_json::from_value(r.channel_overrides).unwrap_or_default(),
        quiet_hours: r.quiet_hours.and_then(|q| serde_json::from_value(q).ok()),
    })
}

/// The conversation notification level (`ALL` when absent or when a mute has lapsed).
pub async fn conversation_level<C: ConnectionTrait>(
    conn: &C,
    account_ref: &str,
    conversation: Uuid,
    now: DateTime<Utc>,
) -> Result<String, DbErr> {
    #[derive(FromQueryResult)]
    struct Row {
        notifications: String,
        muted_until: Option<DateTime<Utc>>,
    }
    let row: Option<Row> = db::one(
        conn,
        "SELECT notifications, muted_until FROM notif_conversation_preferences WHERE account_ref = $1 AND conversation_id = $2",
        vec![account_ref.into(), conversation.into()],
    )
    .await?;
    Ok(match row {
        Some(r) if r.notifications == "MUTED" && r.muted_until.is_some_and(|u| u <= now) => {
            "ALL".into()
        }
        Some(r) => r.notifications,
        None => "ALL".into(),
    })
}

pub async fn contact_level<C: ConnectionTrait>(
    conn: &C,
    account_ref: &str,
    contact_ref: &str,
) -> Result<String, DbErr> {
    let row = conn
        .query_one(db::stmt(
            "SELECT notifications FROM notif_contact_preferences WHERE account_ref = $1 AND contact_ref = $2",
            vec![account_ref.into(), contact_ref.into()],
        ))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<String>("", "notifications").ok())
        .unwrap_or_else(|| "ALL".into()))
}

/// U6: the full document.
pub async fn document<C: ConnectionTrait>(
    conn: &C,
    account_ref: &str,
    time_zone: Option<&str>,
) -> Result<Value, DbErr> {
    let p = load(conn, account_ref).await?;
    #[derive(FromQueryResult)]
    struct Conv {
        conversation_id: Uuid,
        notifications: String,
        muted_until: Option<DateTime<Utc>>,
    }
    #[derive(FromQueryResult)]
    struct Contact {
        contact_ref: String,
        notifications: String,
    }
    let convs: Vec<Conv> = db::all(
        conn,
        "SELECT conversation_id, notifications, muted_until FROM notif_conversation_preferences
          WHERE account_ref = $1 ORDER BY conversation_id LIMIT 5000",
        vec![account_ref.into()],
    )
    .await?;
    let contacts: Vec<Contact> = db::all(
        conn,
        "SELECT contact_ref, notifications FROM notif_contact_preferences WHERE account_ref = $1 ORDER BY contact_ref LIMIT 5000",
        vec![account_ref.into()],
    )
    .await?;
    let mut conversations = Map::new();
    for c in convs {
        conversations.insert(
            c.conversation_id.to_string(),
            json!({ "notifications": c.notifications, "mutedUntil": c.muted_until }),
        );
    }
    let mut contact_map = Map::new();
    for c in contacts {
        contact_map.insert(c.contact_ref, json!({ "notifications": c.notifications }));
    }
    Ok(json!({
        "schemaVersion": 1,
        "version": p.version,
        "categories": p.categories,
        "channels": p.channels,
        "quietHours": p.quiet_hours,
        "conversations": conversations,
        "contacts": contact_map,
        "securityEvents": true,
        "timeZone": time_zone.unwrap_or("UTC"),
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationPreference {
    pub notifications: String,
    #[serde(default)]
    pub muted_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactPreference {
    pub notifications: String,
}

/// U7 body (partial; deny unknown fields).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferencesUpdate {
    pub expected_version: i64,
    #[serde(default)]
    pub categories: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    pub channels: Option<BTreeMap<String, ChannelSwitches>>,
    #[serde(default, deserialize_with = "double_option")]
    pub quiet_hours: Option<Option<QuietHours>>,
    #[serde(default)]
    pub conversations: Option<BTreeMap<Uuid, Option<ConversationPreference>>>,
    #[serde(default)]
    pub contacts: Option<BTreeMap<String, Option<ContactPreference>>>,
}

fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("version conflict")]
    VersionConflict,
    #[error(transparent)]
    Db(#[from] DbErrWrap),
}

#[derive(Debug, thiserror::Error)]
#[error("database error")]
pub struct DbErrWrap(pub DbErr);

impl PartialEq for DbErrWrap {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}
impl Eq for DbErrWrap {}

impl From<DbErr> for UpdateError {
    fn from(e: DbErr) -> Self {
        UpdateError::Db(DbErrWrap(e))
    }
}

fn valid_level(l: &str) -> bool {
    matches!(l, "ALL" | "MENTIONS" | "MUTED")
}

impl PreferencesUpdate {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), UpdateError> {
        if self.categories.is_none()
            && self.channels.is_none()
            && self.quiet_hours.is_none()
            && self.conversations.is_none()
            && self.contacts.is_none()
        {
            return Err(UpdateError::Invalid(
                "at least one preference must be changed",
            ));
        }
        let keys: Vec<&str> = PrefCategory::USER.iter().map(|c| c.json_key()).collect();
        if let Some(c) = &self.categories {
            if c.keys().any(|k| !keys.contains(&k.as_str())) {
                return Err(UpdateError::Invalid("unknown category"));
            }
        }
        if let Some(c) = &self.channels {
            if c.keys().any(|k| !keys.contains(&k.as_str())) {
                return Err(UpdateError::Invalid("unknown channel category"));
            }
        }
        if let Some(Some(q)) = &self.quiet_hours {
            q.validate().map_err(UpdateError::Invalid)?;
        }
        if let Some(c) = &self.conversations {
            if c.len() > 100 {
                return Err(UpdateError::Invalid("at most 100 conversations per update"));
            }
            for v in c.values().flatten() {
                if !valid_level(&v.notifications) {
                    return Err(UpdateError::Invalid(
                        "notifications is ALL, MENTIONS or MUTED",
                    ));
                }
                if let Some(u) = v.muted_until {
                    if v.notifications != "MUTED" {
                        return Err(UpdateError::Invalid(
                            "mutedUntil is allowed only with MUTED",
                        ));
                    }
                    if u <= now || u > now + chrono::Duration::days(366) {
                        return Err(UpdateError::Invalid(
                            "mutedUntil must be in the next 366 days",
                        ));
                    }
                }
            }
        }
        if let Some(c) = &self.contacts {
            if c.len() > 100 {
                return Err(UpdateError::Invalid("at most 100 contacts per update"));
            }
            for (k, v) in c {
                if k.is_empty()
                    || k.len() > 64
                    || !k
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
                {
                    return Err(UpdateError::Invalid("invalid contact reference"));
                }
                if v.as_ref().is_some_and(|v| !valid_level(&v.notifications)) {
                    return Err(UpdateError::Invalid(
                        "notifications is ALL, MENTIONS or MUTED",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// U7: applies a partial update atomically. Returns the new version.
pub async fn update<C: TransactionTrait + ConnectionTrait>(
    conn: &C,
    account_ref: &str,
    u: &PreferencesUpdate,
    now: DateTime<Utc>,
) -> Result<i64, UpdateError> {
    u.validate(now)?;
    let txn = conn.begin().await?;
    db::exec(&txn, "INSERT INTO notif_preferences (account_ref, version) VALUES ($1, 0) ON CONFLICT DO NOTHING", vec![account_ref.into()]).await?;
    let current = load_locked(&txn, account_ref).await?;
    if current.version != u.expected_version {
        return Err(UpdateError::VersionConflict);
    }
    let mut next = current.clone();
    if let Some(c) = &u.categories {
        for (k, v) in c {
            if let Some(key) = PrefCategory::USER
                .iter()
                .map(|c| c.json_key())
                .find(|x| x == k)
            {
                next.categories.insert(key, *v);
            }
        }
    }
    if let Some(c) = &u.channels {
        for (k, v) in c {
            next.channels.insert(k.clone(), *v);
        }
    }
    if let Some(q) = &u.quiet_hours {
        next.quiet_hours = q.clone();
    }
    let version = current.version + 1;
    let col = |c: PrefCategory| next.categories.get(c.json_key()).copied().unwrap_or(true);
    db::exec(
        &txn,
        "UPDATE notif_preferences SET version = $2, direct_messages = $3, group_messages = $4, incoming_calls = $5,
                missed_calls = $6, contact_requests = $7, group_invitations = $8, meeting_invitations = $9,
                meeting_reminders = $10, voicemail = $11, organization_updates = $12, developer_updates = $13,
                channel_overrides = $14, quiet_hours = $15, updated_at = now()
          WHERE account_ref = $1",
        vec![
            account_ref.into(),
            version.into(),
            col(PrefCategory::DirectMessages).into(),
            col(PrefCategory::GroupMessages).into(),
            col(PrefCategory::IncomingCalls).into(),
            col(PrefCategory::MissedCalls).into(),
            col(PrefCategory::ContactRequests).into(),
            col(PrefCategory::GroupInvitations).into(),
            col(PrefCategory::MeetingInvitations).into(),
            col(PrefCategory::MeetingReminders).into(),
            col(PrefCategory::Voicemail).into(),
            col(PrefCategory::OrganizationUpdates).into(),
            col(PrefCategory::DeveloperUpdates).into(),
            serde_json::to_value(&next.channels).unwrap_or(json!({})).into(),
            next.quiet_hours.as_ref().map(|q| serde_json::to_value(q).unwrap_or(Value::Null)).into(),
        ],
    )
    .await?;
    if u.categories.is_some() || u.channels.is_some() || u.quiet_hours.is_some() {
        db::exec(
            &txn,
            "INSERT INTO notif_preference_change_log (account_ref, scope, scope_ref, new_value, version) VALUES ($1, 'ACCOUNT', '', $2, $3)",
            vec![
                account_ref.into(),
                json!({ "categories": next.categories, "channels": next.channels, "quietHours": next.quiet_hours }).into(),
                version.into(),
            ],
        )
        .await?;
    }
    if let Some(convs) = &u.conversations {
        for (id, v) in convs {
            match v {
                None => {
                    db::exec(&txn, "DELETE FROM notif_conversation_preferences WHERE account_ref = $1 AND conversation_id = $2", vec![account_ref.into(), (*id).into()]).await?;
                }
                Some(v) => {
                    db::exec(
                        &txn,
                        "INSERT INTO notif_conversation_preferences (account_ref, conversation_id, notifications, muted_until)
                         VALUES ($1, $2, $3, $4)
                         ON CONFLICT (account_ref, conversation_id) DO UPDATE SET notifications = EXCLUDED.notifications,
                             muted_until = EXCLUDED.muted_until, version = notif_conversation_preferences.version + 1, updated_at = now()",
                        vec![account_ref.into(), (*id).into(), v.notifications.clone().into(), v.muted_until.into()],
                    )
                    .await?;
                }
            }
            db::exec(
                &txn,
                "INSERT INTO notif_preference_change_log (account_ref, scope, scope_ref, new_value, version) VALUES ($1, 'CONVERSATION', $2, $3, $4)",
                vec![
                    account_ref.into(),
                    id.to_string().into(),
                    v.as_ref().map(|v| json!({"notifications": v.notifications, "mutedUntil": v.muted_until})).unwrap_or(json!({"notifications": "ALL"})).into(),
                    version.into(),
                ],
            )
            .await?;
        }
    }
    if let Some(contacts) = &u.contacts {
        for (k, v) in contacts {
            match v {
                None => {
                    db::exec(&txn, "DELETE FROM notif_contact_preferences WHERE account_ref = $1 AND contact_ref = $2", vec![account_ref.into(), k.clone().into()]).await?;
                }
                Some(v) => {
                    db::exec(
                        &txn,
                        "INSERT INTO notif_contact_preferences (account_ref, contact_ref, notifications) VALUES ($1, $2, $3)
                         ON CONFLICT (account_ref, contact_ref) DO UPDATE SET notifications = EXCLUDED.notifications,
                             version = notif_contact_preferences.version + 1, updated_at = now()",
                        vec![account_ref.into(), k.clone().into(), v.notifications.clone().into()],
                    )
                    .await?;
                }
            }
            db::exec(
                &txn,
                "INSERT INTO notif_preference_change_log (account_ref, scope, scope_ref, new_value, version) VALUES ($1, 'CONTACT', $2, $3, $4)",
                vec![
                    account_ref.into(),
                    k.clone().into(),
                    json!({"notifications": v.as_ref().map(|v| v.notifications.as_str()).unwrap_or("ALL")}).into(),
                    version.into(),
                ],
            )
            .await?;
        }
    }
    txn.commit().await?;
    Ok(version)
}

async fn load_locked<C: ConnectionTrait>(
    conn: &C,
    account_ref: &str,
) -> Result<AccountPrefs, DbErr> {
    conn.execute(db::stmt(
        "SELECT 1 FROM notif_preferences WHERE account_ref = $1 FOR UPDATE",
        vec![account_ref.into()],
    ))
    .await?;
    load(conn, account_ref).await
}

/// Copy of the Hub's 7-boolean account preferences (Phase 4.2 copy / catch-up): writes
/// only when the Hub version is newer than the copied one, never over a service-side
/// edit made after the copy (`source_version` records the Hub version).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubPreferences {
    pub account_ref: String,
    pub hub_version: i64,
    pub direct_messages: bool,
    pub group_messages: bool,
    pub incoming_calls: bool,
    pub missed_calls: bool,
    pub contact_requests: bool,
    pub group_invitations: bool,
    pub meeting_invitations: bool,
}

/// Returns `true` when the row was written.
pub async fn copy_from_hub<C: ConnectionTrait>(
    conn: &C,
    p: &HubPreferences,
) -> Result<bool, DbErr> {
    let n = db::exec(
        conn,
        "INSERT INTO notif_preferences (account_ref, version, direct_messages, group_messages, incoming_calls,
                missed_calls, contact_requests, group_invitations, meeting_invitations, source_version)
         VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (account_ref) DO UPDATE SET
                direct_messages = EXCLUDED.direct_messages, group_messages = EXCLUDED.group_messages,
                incoming_calls = EXCLUDED.incoming_calls, missed_calls = EXCLUDED.missed_calls,
                contact_requests = EXCLUDED.contact_requests, group_invitations = EXCLUDED.group_invitations,
                meeting_invitations = EXCLUDED.meeting_invitations, source_version = EXCLUDED.source_version,
                version = notif_preferences.version + 1, updated_at = now()
          WHERE notif_preferences.source_version IS NULL OR notif_preferences.source_version < EXCLUDED.source_version",
        vec![
            p.account_ref.clone().into(),
            p.direct_messages.into(),
            p.group_messages.into(),
            p.incoming_calls.into(),
            p.missed_calls.into(),
            p.contact_requests.into(),
            p.group_invitations.into(),
            p.meeting_invitations.into(),
            p.hub_version.into(),
        ],
    )
    .await?;
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn quiet_hours_cross_midnight_and_respect_days() {
        let q = QuietHours {
            enabled: true,
            start: "22:00".into(),
            end: "07:00".into(),
            days: vec![],
        };
        // 23:30 UTC on a Monday → ends 07:00 Tuesday.
        assert_eq!(
            q.window_end(at("2026-11-16T23:30:00Z"), "UTC"),
            Some(at("2026-11-17T07:00:00Z"))
        );
        assert_eq!(
            q.window_end(at("2026-11-17T06:00:00Z"), "UTC"),
            Some(at("2026-11-17T07:00:00Z"))
        );
        assert_eq!(q.window_end(at("2026-11-17T12:00:00Z"), "UTC"), None);
        let weekdays = QuietHours {
            days: vec!["SAT".into(), "SUN".into()],
            ..q.clone()
        };
        assert_eq!(weekdays.window_end(at("2026-11-16T23:30:00Z"), "UTC"), None);
        // Local time zone: 23:30 in Berlin is 22:30 UTC.
        assert_eq!(
            q.window_end(at("2026-11-16T22:30:00Z"), "Europe/Berlin"),
            Some(at("2026-11-17T06:00:00Z"))
        );
        assert!(!QuietHours {
            enabled: false,
            ..q
        }
        .window_end(at("2026-11-16T23:30:00Z"), "UTC")
        .is_some());
    }

    #[test]
    fn update_validation_follows_the_contract() {
        let now = at("2026-11-16T12:00:00Z");
        let base = PreferencesUpdate {
            expected_version: 0,
            categories: None,
            channels: None,
            quiet_hours: None,
            conversations: None,
            contacts: None,
        };
        assert!(base.validate(now).is_err());
        let mut u = base.clone();
        u.categories = Some(BTreeMap::from([("directMessages".into(), false)]));
        assert!(u.validate(now).is_ok());
        u.categories = Some(BTreeMap::from([("security".into(), false)]));
        assert!(u.validate(now).is_err());
        let mut c = base.clone();
        c.conversations = Some(BTreeMap::from([(
            Uuid::new_v4(),
            Some(ConversationPreference {
                notifications: "ALL".into(),
                muted_until: Some(now + chrono::Duration::days(1)),
            }),
        )]));
        assert!(c.validate(now).is_err());
        c.conversations = Some(BTreeMap::from([(
            Uuid::new_v4(),
            Some(ConversationPreference {
                notifications: "MUTED".into(),
                muted_until: Some(now + chrono::Duration::days(400)),
            }),
        )]));
        assert!(c.validate(now).is_err());
        let parsed: Result<PreferencesUpdate, _> =
            serde_json::from_value(json!({"expectedVersion": 0, "theme": "dark"}));
        assert!(parsed.is_err());
        let parsed: PreferencesUpdate =
            serde_json::from_value(json!({"expectedVersion": 3, "quietHours": null})).unwrap();
        assert_eq!(parsed.quiet_hours, Some(None));
    }

    #[test]
    fn defaults_enable_everything_and_security_is_fixed() {
        let p = AccountPrefs::default();
        for c in PrefCategory::USER {
            assert!(p.category_enabled(c));
        }
        let mut off = p.clone();
        off.categories.insert("meetingReminders", false);
        assert!(!off.category_enabled(PrefCategory::MeetingReminders));
        assert!(off.category_enabled(PrefCategory::Security));
        off.channels.insert(
            "directMessages".into(),
            ChannelSwitches {
                push: Some(false),
                ..Default::default()
            },
        );
        assert!(!off.channel_enabled(
            PrefCategory::DirectMessages,
            crate::catalogue::Channel::Push
        ));
        assert!(off.channel_enabled(
            PrefCategory::DirectMessages,
            crate::catalogue::Channel::InApp
        ));
    }
}

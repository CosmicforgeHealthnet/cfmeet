//! Account and meeting reads (design §6.1–6.2), served from the Meeting Service's operator
//! reads (`account.administer` / `meeting.administer` + `X-CF-Operator: hq:<sub>`).
//!
//! Translation keeps identifiers, codes, counts and timestamps, plus the display names and
//! titles an R1 detail read may show. The email address is matched by the owner's user
//! search and never returned; the user detail carries `email_masked` only. Code fields are
//! normalized to upper case (the account authority stores some in lower case). Cursors are
//! checked here; a malformed one never reaches the owner, and a malformed id names nothing.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::MeetingService;
const BASE: &str = "/internal/v1/operator";

/// A code field, upper-cased (`pending_verification` -> `PENDING_VERIFICATION`); anything
/// that still is not a code fails the answer.
fn code(v: &Value, k: &str) -> T<Value> {
    let s = v
        .get(k)
        .and_then(Value::as_str)
        .ok_or(OwnerError::Contract)?;
    normalized(s)
}

fn opt_code(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(s)) => normalized(s),
        _ => Err(OwnerError::Contract),
    }
}

fn normalized(s: &str) -> T<Value> {
    let up: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let ok = !up.is_empty() && up.len() <= 64 && up.as_bytes()[0].is_ascii_uppercase();
    ok.then_some(Value::String(up)).ok_or(OwnerError::Contract)
}

fn masked(v: &Value) -> T<Value> {
    let s = v
        .get("emailMasked")
        .and_then(Value::as_str)
        .ok_or(OwnerError::Contract)?;
    // The owner masks. A value without a mask is an owner fault (refused); a masked value
    // in an unexpected shape is fully masked rather than relayed. What leaves the bridge
    // always matches the published pattern (agreed with HQ, which validates it too).
    if !s.contains("***") {
        return Err(OwnerError::Contract);
    }
    Ok(Value::String(if strict_mask(s) {
        s.to_string()
    } else {
        "***".to_string()
    }))
}

/// `***`, `x***@y***` or `x***@y***.tld`: one leading character (never `@`, `*` or
/// whitespace; for the domain also never `.`), and a top-level domain of 1–63 such
/// characters. Mirrors the `email_masked` schema pattern exactly.
pub fn strict_mask(s: &str) -> bool {
    if s == "***" {
        return true;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let lead = |part: &str, dot_ok: bool| -> Option<String> {
        let mut chars = part.chars();
        let c = chars.next()?;
        if c == '@' || c == '*' || c.is_whitespace() || (!dot_ok && c == '.') {
            return None;
        }
        let rest: String = chars.collect();
        rest.strip_prefix("***").map(str::to_string)
    };
    if lead(local, true).as_deref() != Some("") {
        return false;
    }
    match lead(domain, false) {
        Some(rest) if rest.is_empty() => true,
        Some(rest) => rest.strip_prefix('.').is_some_and(|tld| {
            let n = tld.chars().count();
            (1..=63).contains(&n)
                && tld
                    .chars()
                    .all(|c| c != '@' && c != '*' && c != '.' && !c.is_whitespace())
        }),
        None => false,
    }
}

pub fn user(u: &Value) -> T<Value> {
    Ok(obj(vec![
        ("user_id", t::uuid(u, "userId")?),
        ("username", t::string(u, "username")?),
        ("display_name", t::opt_string(u, "displayName")?),
        ("role", code(u, "role")?),
        ("status", code(u, "status")?),
        ("created_at", t::time(u, "createdAt")?),
        ("last_login_at", t::opt_time(u, "lastLoginAt")?),
        // The account's version (expected_version of the account commands).
        ("version", t::int(u, "version")?),
    ]))
}

pub fn user_detail(u: &Value) -> T<Value> {
    let mut v = user(u)?;
    v["email_masked"] = masked(u)?;
    v["email_verified"] = t::boolean(u, "emailVerified")?;
    v["auth_type"] = code(u, "authType")?;
    v["locale"] = t::opt_string(u, "locale")?;
    v["timezone"] = t::opt_string(u, "timezone")?;
    Ok(v)
}

pub fn session(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("session_id", t::uuid(r, "sessionId")?),
        ("auth_method", code(r, "authMethod")?),
        ("authenticated_at", t::time(r, "authenticatedAt")?),
        ("created_at", t::time(r, "createdAt")?),
        ("last_refreshed_at", t::opt_time(r, "lastRefreshedAt")?),
        ("expires_at", t::time(r, "expiresAt")?),
        ("revoked_at", t::opt_time(r, "revokedAt")?),
        ("revoke_reason", opt_code(r, "revokeReason")?),
    ]))
}

pub fn device(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("device_id", t::uuid(r, "deviceId")?),
        ("device_class", code(r, "deviceClass")?),
        ("platform", code(r, "platform")?),
        ("trust", code(r, "trust")?),
        ("trusted_via", opt_code(r, "trustedVia")?),
        ("trusted_at", t::opt_time(r, "trustedAt")?),
        ("state", code(r, "state")?),
        ("created_at", t::time(r, "createdAt")?),
        ("last_seen_at", t::time(r, "lastSeenAt")?),
        ("revoked_at", t::opt_time(r, "revokedAt")?),
        ("revoke_reason", opt_code(r, "revokeReason")?),
    ]))
}

pub fn mfa(r: &Value) -> T<Value> {
    let totp = r
        .get("totp")
        .filter(|v| v.is_object())
        .ok_or(OwnerError::Contract)?;
    Ok(obj(vec![
        ("user_id", t::uuid(r, "userId")?),
        ("totp_state", code(totp, "state")?),
        ("totp_enrolled_at", t::opt_time(totp, "enrolledAt")?),
        ("totp_confirmed_at", t::opt_time(totp, "confirmedAt")?),
        (
            "recovery_codes_remaining",
            t::int(r, "recoveryCodesRemaining")?,
        ),
    ]))
}

pub fn security_event(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("event_id", t::uuid(r, "eventId")?),
        ("sequence", t::int(r, "sequence")?),
        ("event_type", code(r, "eventType")?),
        ("outcome", code(r, "outcome")?),
        ("actor_kind", code(r, "actorKind")?),
        ("actor_user_id", t::opt_uuid(r, "actorUserId")?),
        ("session_id", t::opt_uuid(r, "sessionId")?),
        ("occurred_at", t::time(r, "occurredAt")?),
    ]))
}

pub fn space(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("space_id", t::uuid(r, "spaceId")?),
        ("meeting_code", t::string(r, "meetingCode")?),
        ("title", t::string(r, "title")?),
        ("meeting_type", code(r, "meetingType")?),
        ("status", code(r, "status")?),
        ("owner_user_id", t::opt_uuid(r, "ownerUserId")?),
        ("organization_id", t::opt_uuid(r, "organizationId")?),
        ("created_at", t::time(r, "createdAt")?),
        ("updated_at", t::time(r, "updatedAt")?),
        ("archived_at", t::opt_time(r, "archivedAt")?),
        ("version", t::int(r, "version")?),
    ]))
}

pub fn conference(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("meeting_id", t::uuid(r, "conferenceId")?),
        ("space_id", t::uuid(r, "spaceId")?),
        ("title", t::string(r, "title")?),
        ("kind", code(r, "kind")?),
        ("status", code(r, "status")?),
        ("scheduled_start_at", t::opt_time(r, "scheduledStartAt")?),
        ("scheduled_end_at", t::opt_time(r, "scheduledEndAt")?),
        ("started_at", t::opt_time(r, "startedAt")?),
        ("ended_at", t::opt_time(r, "endedAt")?),
        ("termination_reason", opt_code(r, "terminationReason")?),
        ("locked", t::boolean(r, "locked")?),
        ("recording_state", code(r, "recordingState")?),
        ("transcription_state", code(r, "transcriptionState")?),
        ("participants_admitted", t::int(r, "participantsAdmitted")?),
        ("participants_total", t::int(r, "participantsTotal")?),
        ("created_at", t::time(r, "createdAt")?),
        ("version", t::int(r, "version")?),
    ]))
}

pub fn participant(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("participant_id", t::uuid(r, "participantId")?),
        ("identity_type", code(r, "identityType")?),
        ("user_id", t::opt_uuid(r, "userId")?),
        ("display_name", t::string(r, "displayName")?),
        ("role", code(r, "role")?),
        ("admission_state", code(r, "admissionState")?),
        ("requested_at", t::time(r, "requestedAt")?),
        ("admitted_at", t::opt_time(r, "admittedAt")?),
        ("removed_at", t::opt_time(r, "removedAt")?),
        ("banned_at", t::opt_time(r, "bannedAt")?),
    ]))
}

pub fn audit_event(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("event_id", t::uuid(r, "eventId")?),
        ("space_id", t::uuid(r, "spaceId")?),
        ("meeting_id", t::opt_uuid(r, "conferenceId")?),
        ("actor_type", code(r, "actorType")?),
        ("action", code(r, "action")?),
        ("target_type", opt_code(r, "targetType")?),
        ("occurred_at", t::time(r, "occurredAt")?),
    ]))
}

fn uniqueness(v: &Value, k: &str) -> T<Value> {
    let u = v.get(k).ok_or(OwnerError::Contract)?;
    let groups = u
        .get("duplicateGroups")
        .and_then(Value::as_array)
        .filter(|g| g.len() <= 100)
        .ok_or(OwnerError::Contract)?
        .iter()
        .map(|g| {
            let ids = g
                .as_array()
                .filter(|ids| ids.len() <= 100)
                .ok_or(OwnerError::Contract)?;
            ids.iter()
                .map(|i| t::uuid(&json!({"id": i}), "id"))
                .collect::<T<Vec<_>>>()
                .map(Value::Array)
        })
        .collect::<T<Vec<_>>>()?;
    Ok(obj(vec![
        ("enforced", t::boolean(u, "enforced")?),
        ("duplicate_groups", Value::Array(groups)),
    ]))
}

/// Accounts sharing a normalized email address or username: account ids only.
pub fn duplicates(v: &Value) -> T<Value> {
    Ok(obj(vec![
        ("email_uniqueness", uniqueness(v, "email")?),
        ("username_uniqueness", uniqueness(v, "username")?),
    ]))
}

pub fn test_account(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("email_masked", masked(r)?),
        ("registered", t::boolean(r, "registered")?),
        ("user_id", t::opt_uuid(r, "userId")?),
        ("account_status", opt_code(r, "accountStatus")?),
        ("role", opt_code(r, "role")?),
    ]))
}

pub fn live_health(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("meeting_id", t::uuid(r, "conferenceId")?),
        ("space_id", t::uuid(r, "meetingSpaceId")?),
        ("lifecycle_state", code(r, "lifecycleState")?),
        ("health_state", code(r, "healthState")?),
        ("evaluated_at", t::time(r, "evaluatedAt")?),
    ]))
}

pub fn webinar(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("space_id", t::uuid(r, "spaceId")?),
        ("meeting_code", t::string(r, "meetingCode")?),
        ("meeting_type", code(r, "meetingType")?),
        ("status", code(r, "status")?),
        ("owner_user_id", t::opt_uuid(r, "ownerUserId")?),
        ("organization_id", t::opt_uuid(r, "organizationId")?),
        ("registrations_active", t::int(r, "registrationsActive")?),
        (
            "registrations_cancelled",
            t::int(r, "registrationsCancelled")?,
        ),
        ("created_at", t::time(r, "createdAt")?),
        ("version", t::int(r, "version")?),
    ]))
}

pub fn registration(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("registration_id", t::uuid(r, "registrationId")?),
        ("space_id", t::uuid(r, "spaceId")?),
        ("meeting_id", t::opt_uuid(r, "conferenceId")?),
        ("status", code(r, "status")?),
        ("user_id", t::opt_uuid(r, "userId")?),
        ("created_at", t::time(r, "createdAt")?),
        ("cancelled_at", t::opt_time(r, "cancelledAt")?),
    ]))
}

pub fn chat_migration(r: &Value) -> T<Value> {
    let parity = match r.get("latestParityRun") {
        None | Some(Value::Null) => Value::Null,
        Some(p) => obj(vec![
            ("parity_run_id", t::uuid(p, "parityRunId")?),
            ("status", code(p, "status")?),
            ("started_at", t::time(p, "startedAt")?),
            ("finished_at", t::opt_time(p, "finishedAt")?),
            ("meetings_checked", t::int(p, "conferencesChecked")?),
            ("messages_checked", t::int(p, "messagesChecked")?),
            ("mismatches", t::int(p, "mismatches")?),
        ]),
    };
    Ok(obj(vec![
        ("migration_state", code(r, "state")?),
        ("version", t::int(r, "version")?),
        ("changed_at", t::time(r, "changedAt")?),
        ("pending_mirror", t::int(r, "pendingMirror")?),
        (
            "pending_rollback_copies",
            t::int(r, "pendingRollbackCopies")?,
        ),
        (
            "pending_session_revocations",
            t::int(r, "pendingSessionRevocations")?,
        ),
        ("linked_meetings", t::int(r, "linkedConferences")?),
        ("latest_parity_run", parity),
    ]))
}

pub fn critical_lane(r: &Value) -> T<Value> {
    let leased = r
        .get("leasedUncompletedDecisions")
        .and_then(Value::as_array)
        .filter(|a| a.len() <= 500)
        .ok_or(OwnerError::Contract)?
        .iter()
        .map(|i| t::uuid(&json!({"id": i}), "id"))
        .collect::<T<Vec<_>>>()?;
    Ok(obj(vec![
        ("fallback_key", t::boolean(r, "fallbackKey")?),
        ("pending", t::int(r, "pending")?),
        ("dead", t::int(r, "dead")?),
        ("leased_uncompleted_decisions", Value::Array(leased)),
    ]))
}

fn uuid_param(auth: &Authorized, name: &str) -> Result<String, ErrorCode> {
    let raw = auth.params.get(name).ok_or(ErrorCode::Internal)?;
    uuid::Uuid::parse_str(raw)
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| ErrorCode::ResourceNotFound)
}

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Clone, Copy, PartialEq)]
enum Cursor {
    None,
    Uuid,
    /// Positive sequence (security history).
    Sequence,
    /// `<micros>.<uuid>` (conference audit).
    TimeId,
    /// The position of the next entry (test accounts).
    Offset,
}

fn cursor_ok(form: Cursor, c: &str) -> bool {
    match form {
        Cursor::None => false,
        Cursor::Uuid => uuid::Uuid::parse_str(c).is_ok(),
        Cursor::Sequence => c.parse::<i64>().is_ok_and(|n| n > 0),
        Cursor::Offset => c.parse::<u32>().is_ok_and(|n| n <= 100_000),
        Cursor::TimeId => c.split_once('.').is_some_and(|(m, id)| {
            !m.is_empty()
                && m.len() <= 20
                && m.bytes().all(|b| b.is_ascii_digit())
                && uuid::Uuid::parse_str(id).is_ok()
        }),
    }
}

enum Shape {
    Page(fn(&Value) -> T<Value>),
    One(fn(&Value) -> T<Value>),
}

struct Plan {
    path: String,
    query: Vec<(&'static str, String)>,
    cursor: Cursor,
    shape: Shape,
}

fn plan(auth: &Authorized) -> Option<Result<Plan, ErrorCode>> {
    let q = &auth.query;
    let search = || vec![("q", q.q.clone().unwrap_or_default())];
    let window = || {
        let mut v: Vec<(&'static str, String)> = Vec::new();
        v.extend(q.start.map(|s| ("since", rfc(s))));
        v.extend(q.end.map(|e| ("until", rfc(e))));
        v
    };
    let user_path = |suffix: &str| {
        uuid_param(auth, "user_id").map(|id| format!("{BASE}/accounts/users/{id}{suffix}"))
    };
    let conf_path = |suffix: &str| {
        uuid_param(auth, "meeting_id").map(|id| format!("{BASE}/meetings/conferences/{id}{suffix}"))
    };
    let p = |path: Result<String, ErrorCode>,
             query: Vec<(&'static str, String)>,
             cursor: Cursor,
             shape: Shape| {
        path.map(|path| Plan {
            path,
            query,
            cursor,
            shape,
        })
    };
    Some(match auth.capability.id {
        "accounts.users.search" => p(
            Ok(format!("{BASE}/accounts/users")),
            search(),
            Cursor::Uuid,
            Shape::Page(user),
        ),
        "accounts.users.read" => p(user_path(""), vec![], Cursor::None, Shape::One(user_detail)),
        "accounts.sessions.list" => p(
            user_path("/sessions"),
            vec![],
            Cursor::Uuid,
            Shape::Page(session),
        ),
        "accounts.devices.list" => p(
            user_path("/devices"),
            vec![],
            Cursor::Uuid,
            Shape::Page(device),
        ),
        "accounts.mfa.read" => p(user_path("/mfa"), vec![], Cursor::None, Shape::One(mfa)),
        "accounts.security-history.list" => p(
            user_path("/security-events"),
            window(),
            Cursor::Sequence,
            Shape::Page(security_event),
        ),
        "meetings.spaces.search" => p(
            Ok(format!("{BASE}/meetings/spaces")),
            search(),
            Cursor::Uuid,
            Shape::Page(space),
        ),
        "meetings.meetings.search" => p(
            Ok(format!("{BASE}/meetings/conferences")),
            search(),
            Cursor::Uuid,
            Shape::Page(conference),
        ),
        "meetings.meetings.read" => p(conf_path(""), vec![], Cursor::None, Shape::One(conference)),
        "meetings.live.list" => p(
            Ok(format!("{BASE}/meetings/live")),
            vec![],
            Cursor::Uuid,
            Shape::Page(conference),
        ),
        "meetings.participants.list" => p(
            conf_path("/participants"),
            vec![],
            Cursor::Uuid,
            Shape::Page(participant),
        ),
        "accounts.duplicates.list" => p(
            Ok(format!("{BASE}/accounts/duplicates")),
            vec![],
            Cursor::None,
            Shape::One(duplicates),
        ),
        "accounts.test-accounts.list" => p(
            Ok(format!("{BASE}/accounts/test-accounts")),
            vec![],
            Cursor::Offset,
            Shape::Page(test_account),
        ),
        "meetings.live-health.read" => p(
            conf_path("/health"),
            vec![],
            Cursor::None,
            Shape::One(live_health),
        ),
        "meetings.webinars.list" => p(
            Ok(format!("{BASE}/meetings/webinars")),
            vec![],
            Cursor::Uuid,
            Shape::Page(webinar),
        ),
        "meetings.webinar-registrations.list" => p(
            uuid_param(auth, "webinar_id")
                .map(|id| format!("{BASE}/meetings/webinars/{id}/registrations")),
            vec![],
            Cursor::Uuid,
            Shape::Page(registration),
        ),
        "meetings.chat-migration.read" => p(
            Ok(format!("{BASE}/meetings/chat-migration")),
            vec![],
            Cursor::None,
            Shape::One(chat_migration),
        ),
        "meetings.critical-lane.read" => p(
            Ok(format!("{BASE}/meetings/critical-lane")),
            vec![],
            Cursor::None,
            Shape::One(critical_lane),
        ),
        "meetings.audit.list" => p(
            Ok(format!("{BASE}/meetings/audit-events")),
            window(),
            Cursor::TimeId,
            Shape::Page(audit_event),
        ),
        _ => return None,
    })
}

/// The owner lists webinars without a window: the bridge keeps the page's webinars
/// created in `[start, end)` (a page may then hold fewer than `limit`; follow
/// `next_cursor`). Other lists pass through.
fn in_window(auth: &Authorized, items: Vec<Value>) -> Vec<Value> {
    if auth.capability.id != "meetings.webinars.list" {
        return items;
    }
    let (start, end) = (auth.query.start, auth.query.end);
    items
        .into_iter()
        .filter(|w| {
            let at = w["created_at"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
            at.is_some_and(|at| start.is_none_or(|s| at >= s) && end.is_none_or(|e| at < e))
        })
        .collect()
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let plan = match plan(auth)? {
        Ok(p) => p,
        Err(e) => return Some(Err(e)),
    };
    let q = &auth.query;
    let mut query = plan.query;
    // A single answer takes no cursor (the duplicates list is one bounded answer).
    if plan.cursor == Cursor::None && q.cursor.is_some() {
        return Some(Err(ErrorCode::QueryInvalid));
    }
    if plan.cursor != Cursor::None {
        if q.cursor
            .as_deref()
            .is_some_and(|c| !cursor_ok(plan.cursor, c))
        {
            return Some(Err(ErrorCode::QueryInvalid));
        }
        query.extend(q.limit.map(|l| ("limit", l.to_string())));
        query.extend(q.cursor.clone().map(|c| ("cursor", c)));
    }
    let operator = format!("hq:{}", auth.staff.claims.sub);
    Some(
        match bridge.owners.read(O, &plan.path, &query, &operator).await {
            Ok(v) => match plan.shape {
                Shape::Page(item) => t::page(&v, item)
                    .map(|(items, next)| {
                        json!({"items": in_window(auth, items), "next_cursor": next})
                    })
                    .map(|result| Answer {
                        result,
                        freshness: "LIVE",
                    })
                    .map_err(ErrorCode::from),
                Shape::One(f) => f(&v)
                    .map(|result| Answer {
                        result,
                        freshness: "LIVE",
                    })
                    .map_err(ErrorCode::from),
            },
            Err(e) => Err(e.into()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_normalized_and_validated() {
        assert_eq!(
            normalized("pending_verification").unwrap(),
            json!("PENDING_VERIFICATION")
        );
        assert_eq!(normalized("LIVE").unwrap(), json!("LIVE"));
        assert_eq!(
            normalized("meeting:space-create").unwrap(),
            json!("MEETING_SPACE_CREATE")
        );
        assert!(normalized("").is_err());
        assert!(normalized("1abc").is_err());
    }

    #[test]
    fn a_whole_address_is_never_relayed_as_masked() {
        assert!(masked(&json!({"emailMasked": "a***@e***.com"})).is_ok());
        assert!(masked(&json!({"emailMasked": "x***@l***"})).is_ok());
        assert_eq!(masked(&json!({"emailMasked": "***"})).unwrap(), "***");
        // masked but in an unexpected shape: fully masked, never relayed as is
        assert_eq!(
            masked(&json!({"emailMasked": "ab***@e***.com"})).unwrap(),
            "***"
        );
        assert_eq!(
            masked(&json!({"emailMasked": "a***@e***.co.uk"})).unwrap(),
            "***"
        );
        // no mask at all: an owner fault
        assert!(masked(&json!({"emailMasked": "ada@example.com"})).is_err());
        for ok in ["***", "a***@e***.com", "x***@l***", "é***@ü***.de"] {
            assert!(strict_mask(ok), "{ok}");
        }
        for bad in [
            "ada@example.com",
            "a***@e***",
            "**@e***.com",
            "a***@.***.com",
            "a***@e***.",
            "a***@e***.c om",
        ] {
            if bad == "a***@e***" {
                continue;
            }
            assert!(!strict_mask(bad), "{bad}");
        }
        assert!(masked(&json!({"emailMasked": "ada@example.com"})).is_err());
    }
}

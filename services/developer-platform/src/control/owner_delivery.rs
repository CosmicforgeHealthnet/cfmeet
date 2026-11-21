//! ADR 0068 A4 — owner-delivered secrets (owner decision with HQ, 2026-11-21).
//!
//! HQ staff never see a secret, and HQ never stores or relays one. When a command relayed
//! by the admin bridge produces a secret, the Developer Platform delivers it DIRECTLY to
//! its rightful recipient and answers the bridge with a reference only.
//!
//! Today one secret: the one-time OWNER invitation of a first-party workspace
//! (`partners.workspace.register-first-party`), delivered as a single-use claim link to
//! the invited address only (`CLAIM_LINK_EMAIL`), through the Notification Service
//! (`developer-platform.workspace.owner-invitation.email-requested`, contract
//! `cfmeet.notifications.event` 1.6.0 #41):
//!
//! * the invitation keeps only the token's SHA-256 digest; accepting it consumes it
//!   (single use) and needs a CF Meet sign-in whose VERIFIED email is the invited address;
//!   it expires with the invitation (7 days) and is never resent — HQ re-issues the
//!   command after expiry;
//! * the event is built ONCE, in the command's transaction, with the token and the address
//!   sealed (HPKE) to the Notification Service's key (`CF_NOTIFICATIONS_SEALING_KEY`); the
//!   row keeps that sealed envelope only while it is `PENDING` and drops it when the
//!   service accepts or refuses it, or when the link expires unsent;
//! * the bridge's answer, the operation ledger and the logs carry the reference only:
//!   `deliveryRef`, the masked address (`a***@e***.com`), the state and the expiry —
//!   never the token, the link or the address.
//!
//! The worker relays pending deliveries to the Notification Service's I1 with the
//! publication sink ([`crate::notification_publications`]): `ACCEPTED` / `DUPLICATE` /
//! `HELD` → `SENT`; `REJECTED` or a contract refusal → `FAILED`; anything else retries
//! with backoff until the link expires (`EXPIRED`). Settled rows are purged after
//! [`RETENTION`].

use cf_notification_client::{
    EnvelopeBuilder, Producer, RouteRef, SealedPlaintext, SealingKey, ROUTING_PROTOCOL,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::stmt;
use crate::{
    http::error::{ApiError, ErrorCode},
    notification_publications::{
        backoff, decision_id, event_id, IngestResult, NotificationEventSink, PublishFailure,
        ROUTING_VERSION,
    },
};

/// The notification event of a workspace owner invitation (catalogue #41).
pub const OWNER_INVITATION_EMAIL: &str =
    "developer-platform.workspace.owner-invitation.email-requested";
/// The delivery channel of a claim link sent to the recipient's verified address.
pub const CLAIM_LINK_EMAIL: &str = "CLAIM_LINK_EMAIL";
/// How long a settled delivery is kept (HQ's read of its state).
pub const RETENTION: Duration = Duration::days(30);
/// Deliveries relayed per worker pass.
pub const BATCH: i64 = 100;

/// What an owner delivery needs: the Notification Service's public sealing key (the
/// relay runs in the worker, which needs `CF_NOTIFICATIONS_BASE_URL`).
#[derive(Clone)]
pub struct OwnerDelivery {
    pub sealing_key: SealingKey,
}

impl std::fmt::Debug for OwnerDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerDelivery").finish_non_exhaustive()
    }
}

/// `a***@e***.com`: the first character of the local part and of the domain label, the
/// top-level domain kept; anything unusual masks completely (`***`). Only a masked value
/// can come out (the admin bridge's `email_masked` pattern).
pub fn mask_email(address: &str) -> String {
    let Some((local, domain)) = address.trim().rsplit_once('@') else {
        return "***".into();
    };
    let (label, tld) = domain.rsplit_once('.').unwrap_or((domain, ""));
    let ok = |c: char| !c.is_whitespace() && c != '@' && c != '*';
    let (Some(l), Some(d)) = (local.chars().next(), label.chars().next()) else {
        return "***".into();
    };
    if !ok(l)
        || !ok(d)
        || d == '.'
        || tld.chars().count() > 63
        || !tld.chars().all(|c| ok(c) && c != '.')
    {
        return "***".into();
    }
    if tld.is_empty() {
        format!("{l}***@{d}***")
    } else {
        format!("{l}***@{d}***.{tld}")
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The sealed event of one owner invitation: `(event id, envelope)`. The token and the
/// address leave this function only sealed.
#[allow(clippy::too_many_arguments)]
pub fn seal_owner_invitation(
    key: &SealingKey,
    tenant_id: Uuid,
    invitation_id: Uuid,
    expires_at: DateTime<Utc>,
    token: &str,
    address: &str,
    correlation_id: &str,
    now: DateTime<Utc>,
) -> Result<(String, Value), ApiError> {
    let invitation = invitation_id.to_string();
    let event = event_id(OWNER_INVITATION_EMAIL, &[&invitation]);
    let route = RouteRef {
        decision_id: decision_id(&event),
        routing_version: ROUTING_VERSION,
        protocol: ROUTING_PROTOCOL,
    };
    let envelope = EnvelopeBuilder::new(
        Producer::DeveloperPlatform,
        OWNER_INVITATION_EMAIL,
        &rfc3339(now),
        route,
    )
    .map_err(|_| ApiError(ErrorCode::InternalError))?
    .event_id(event.clone())
    .correlation_id(correlation_id.to_string())
    .subject(invitation_id, Some(1))
    .recipient(format!("meeting-email-principal:{invitation}"))
    .dedupe_key(format!("{OWNER_INVITATION_EMAIL}:{invitation}"))
    .expires_at(rfc3339(expires_at))
    .data("invitationId", invitation.clone())
    .data("organizationId", tenant_id.to_string())
    .data("expiresAt", rfc3339(expires_at))
    .secret(
        SealedPlaintext::default()
            .with_token(token)
            .with_address(address),
    )
    .build_sealed(key)
    .map_err(|_| ApiError(ErrorCode::InternalError))?;
    let v = serde_json::to_value(envelope).map_err(|_| ApiError(ErrorCode::InternalError))?;
    Ok((event, v))
}

/// One delivery to record (in the command's transaction).
pub struct NewDelivery<'a> {
    pub id: Uuid,
    pub admin_key: Uuid,
    pub tenant_id: Uuid,
    pub invitation_id: Uuid,
    pub recipient_masked: &'a str,
    pub event_id: &'a str,
    pub envelope: Value,
    pub expires_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
}

pub async fn insert<C: ConnectionTrait>(db: &C, d: NewDelivery<'_>) -> Result<(), ApiError> {
    db.execute(stmt(
        "INSERT INTO devplat_owner_deliveries (id, admin_key, tenant_id, invitation_id, channel, recipient_masked, \
           event_id, envelope, next_attempt_at, expires_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9)",
        vec![
            d.id.into(),
            d.admin_key.into(),
            d.tenant_id.into(),
            d.invitation_id.into(),
            CLAIM_LINK_EMAIL.into(),
            d.recipient_masked.into(),
            d.event_id.into(),
            d.envelope.into(),
            d.now.into(),
            d.expires_at.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// The reference HQ may see: ids, states, the masked address and times. `claimState`
/// follows the invitation (`UNCLAIMED` | `CLAIMED` | `EXPIRED` | `REVOKED`).
const VIEW: &str = "jsonb_build_object( \
    'delivery', 'OWNER_DELIVERED', 'deliveryChannel', d.channel, 'deliveryRef', d.id, \
    'recipientMasked', d.recipient_masked, 'deliveryState', d.delivery_state, \
    'claimState', CASE WHEN i.status = 'ACCEPTED' THEN 'CLAIMED' \
                       WHEN i.status = 'REVOKED' THEN 'REVOKED' \
                       WHEN i.status = 'EXPIRED' OR i.expires_at <= now() THEN 'EXPIRED' \
                       ELSE 'UNCLAIMED' END, \
    'expiresAt', d.expires_at, 'sentAt', d.sent_at, 'createdAt', d.created_at, \
    'organizationId', d.tenant_id, 'ownerInvitationId', d.invitation_id)";

/// A delivery's reference by its id (`None`: unknown or purged).
pub async fn view<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Value>, ApiError> {
    Ok(db
        .query_one(stmt(
            &format!(
                "SELECT {VIEW} AS v FROM devplat_owner_deliveries d \
                 JOIN devplat_invitations i ON i.id = d.invitation_id WHERE d.id = $1"
            ),
            vec![id.into()],
        ))
        .await?
        .map(|r| r.try_get::<Value>("", "v"))
        .transpose()?)
}

/// The delivery a bridge command (its Idempotency-Key) produced.
pub async fn view_of_command<C: ConnectionTrait>(
    db: &C,
    admin_key: Uuid,
) -> Result<Option<Value>, ApiError> {
    Ok(db
        .query_one(stmt(
            &format!(
                "SELECT {VIEW} AS v FROM devplat_owner_deliveries d \
                 JOIN devplat_invitations i ON i.id = d.invitation_id WHERE d.admin_key = $1"
            ),
            vec![admin_key.into()],
        ))
        .await?
        .map(|r| r.try_get::<Value>("", "v"))
        .transpose()?)
}

/// What one relay pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReport {
    pub sent: usize,
    pub failed: usize,
    pub retried: usize,
    pub expired: u64,
}

async fn expire_unsent<C: ConnectionTrait>(db: &C, now: DateTime<Utc>) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "UPDATE devplat_owner_deliveries SET delivery_state = 'EXPIRED', envelope = NULL, \
               last_error = 'EXPIRED', completed_at = $1 \
             WHERE delivery_state = 'PENDING' AND expires_at <= $1",
            vec![now.into()],
        ))
        .await?
        .rows_affected())
}

/// One relay pass at `now`: the due PENDING deliveries (≤ [`BATCH`], locked
/// `FOR UPDATE SKIP LOCKED` for the attempt) go to I1 as one batch; each row settles in
/// the same transaction. A crash mid-attempt leaves them PENDING; the re-post carries the
/// same event id and is answered `DUPLICATE`.
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
    now: DateTime<Utc>,
) -> Result<DeliveryReport, DbErr> {
    let mut report = DeliveryReport {
        expired: expire_unsent(db, now).await?,
        ..Default::default()
    };
    let txn = db.begin().await?;
    let rows = txn
        .query_all(stmt(
            "SELECT id, event_id, envelope, attempts FROM devplat_owner_deliveries \
             WHERE delivery_state = 'PENDING' AND next_attempt_at <= $1 AND expires_at > $1 \
             ORDER BY next_attempt_at, created_at LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), BATCH.into()],
        ))
        .await?;
    if rows.is_empty() {
        txn.rollback().await?;
        return Ok(report);
    }
    let mut due: Vec<(Uuid, String, i32)> = Vec::with_capacity(rows.len());
    let mut events = Vec::with_capacity(rows.len());
    for r in &rows {
        due.push((
            r.try_get("", "id")?,
            r.try_get("", "event_id")?,
            r.try_get("", "attempts")?,
        ));
        events.push(r.try_get::<Value>("", "envelope")?);
    }
    let settle = |state: &'static str, error: Option<String>| {
        move |id: Uuid| {
            stmt(
                "UPDATE devplat_owner_deliveries SET delivery_state = $2, envelope = NULL, last_error = $3, \
                   attempts = attempts + 1, completed_at = $4, \
                   sent_at = CASE WHEN $2 = 'SENT' THEN $4 ELSE NULL END WHERE id = $1",
                vec![
                    id.into(),
                    state.into(),
                    error.clone().into(),
                    now.into(),
                ],
            )
        }
    };
    let retry = |id: Uuid, attempts: i32, error: &str| {
        stmt(
            "UPDATE devplat_owner_deliveries SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
            vec![
                id.into(),
                (attempts + 1).into(),
                error.into(),
                (now + backoff(attempts + 1)).into(),
            ],
        )
    };
    match sink.ingest(&events).await {
        Ok(results) => {
            for (id, event, attempts) in &due {
                let r: Option<&IngestResult> = results.iter().find(|r| &r.event_id == event);
                match r.map(|r| r.outcome.as_str()) {
                    Some("ACCEPTED" | "DUPLICATE" | "HELD") => {
                        txn.execute(settle("SENT", None)(*id)).await?;
                        report.sent += 1;
                    }
                    Some("REJECTED") => {
                        txn.execute(settle("FAILED", Some("REJECTED".into()))(*id))
                            .await?;
                        report.failed += 1;
                    }
                    _ => {
                        txn.execute(retry(*id, *attempts, "NO_RESULT")).await?;
                        report.retried += 1;
                    }
                }
            }
        }
        Err(PublishFailure::Rejected) => {
            for (id, _, _) in &due {
                txn.execute(settle("FAILED", Some("CONTRACT_REJECTED".into()))(*id))
                    .await?;
                report.failed += 1;
            }
        }
        Err(PublishFailure::Unavailable) | Err(PublishFailure::NotConfigured) => {
            for (id, _, attempts) in &due {
                txn.execute(retry(*id, *attempts, "UNAVAILABLE")).await?;
                report.retried += 1;
            }
        }
    }
    txn.commit().await?;
    Ok(report)
}

/// Maintenance at `now`: expires unsent links and purges at most `batch` settled rows
/// older than [`RETENTION`]. Returns (expired, purged).
pub async fn maintain(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    batch: i64,
) -> Result<(u64, u64), DbErr> {
    let expired = expire_unsent(db, now).await?;
    let purged = db
        .execute(stmt(
            "DELETE FROM devplat_owner_deliveries WHERE id IN (SELECT id FROM devplat_owner_deliveries \
              WHERE delivery_state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected();
    Ok((expired, purged))
}

/// The resource fields of a delivery inside a command answer.
pub fn resource_fields(target: &mut Value, delivery: &Value) {
    for k in [
        "delivery",
        "deliveryChannel",
        "deliveryRef",
        "recipientMasked",
        "deliveryState",
        "claimState",
        "expiresAt",
        "ownerInvitationId",
    ] {
        target[k] = delivery.get(k).cloned().unwrap_or(Value::Null);
    }
}

/// An empty resource of a command whose delivery is unknown (never expected).
pub fn no_delivery() -> Value {
    json!({ "delivery": "OWNER_DELIVERED", "deliveryChannel": CLAIM_LINK_EMAIL })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_masked_to_the_bridge_pattern_only() {
        assert_eq!(mask_email("ada@example.com"), "a***@e***.com");
        assert_eq!(mask_email("ada.l@mail.example.co.uk"), "a***@m***.uk");
        assert_eq!(mask_email("ada@localhost"), "a***@l***");
        for odd in ["", "no-at", "@example.com", "ada@", "ada@.com", "*a@b.com"] {
            assert_eq!(mask_email(odd), "***", "{odd}");
        }
        // Never the address itself.
        assert!(!mask_email("owner@example.com").contains("owner"));
    }

    #[test]
    fn the_owner_invitation_is_sealed_and_ids_only_in_clear() {
        let (public, _private) =
            cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
        let key = SealingKey::parse(&public).unwrap();
        let (tenant, invitation) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Utc::now();
        let token = "cfinv_fixture_0123456789abcdefghijABCDEF";
        let (event, v) = seal_owner_invitation(
            &key,
            tenant,
            invitation,
            now + Duration::days(7),
            token,
            "owner@example.com",
            "corr_0123456789abcdef0123456789abcdef",
            now,
        )
        .unwrap();
        assert_eq!(v["eventId"], event.as_str());
        assert_eq!(v["eventType"], OWNER_INVITATION_EMAIL);
        assert_eq!(
            v["recipients"][0]["ref"],
            format!("meeting-email-principal:{invitation}")
        );
        assert!(v.get("sealed").is_some());
        let text = v.to_string();
        assert!(!text.contains(token) && !text.contains("owner@example.com"));
        // The same invitation always has the same event (a re-post is a duplicate).
        assert_eq!(
            event,
            event_id(OWNER_INVITATION_EMAIL, &[&invitation.to_string()])
        );
    }
}

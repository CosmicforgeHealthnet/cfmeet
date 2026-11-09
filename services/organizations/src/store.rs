//! Rows, the trusted actor of a change and the writers every mutation uses in its own
//! transaction: audit event, membership history and outbox event.

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    domain::{effective_capabilities_with_policy, LifecycleStatus, MembershipStatus, Role},
    policy::PolicyDocument,
    validation::{LegalValues, ProfileValues},
};

/// The record destination every event is written to (read by reconciliation, never sent).
pub const RECONCILIATION_DESTINATION: &str = crate::outbox::RECONCILIATION;

pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn opt_time(t: Option<DateTime<Utc>>) -> Value {
    t.map(rfc3339).map(Value::String).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    User,
    Operator,
    System,
}

impl ActorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorKind::User => "USER",
            ActorKind::Operator => "OPERATOR",
            ActorKind::System => "SYSTEM",
        }
    }
}

/// Who causes a change: always from the signed request (or the local operator CLI).
#[derive(Debug, Clone)]
pub struct OpContext {
    pub kind: ActorKind,
    pub user_id: Option<Uuid>,
    /// The authenticated calling service (`cfmeet.meeting-service`, …).
    pub service: String,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
}

impl OpContext {
    pub fn system(service: &str) -> Self {
        Self {
            kind: ActorKind::System,
            user_id: None,
            service: service.to_string(),
            request_id: None,
            correlation_id: None,
        }
    }

    pub fn actor_json(&self) -> Value {
        let mut actor = json!({ "kind": self.kind.as_str(), "serviceId": self.service });
        if let Some(u) = self.user_id {
            actor["userId"] = json!(u);
        }
        actor
    }
}

/// `req_<32 hex>` / `corr_<32 hex>` as a UUID (the event envelope's id type).
fn hop_uuid(id: Option<&str>) -> Uuid {
    id.and_then(|v| v.split_once('_'))
        .and_then(|(_, hex)| Uuid::try_parse(hex).ok())
        .unwrap_or_else(Uuid::new_v4)
}

// ---------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------

pub const ORG_COLUMNS: &str = "id, name, slug, lifecycle_status, lifecycle_reason, provisioning_state, creation_source, created_by, created_at, updated_at, version, archived_at, profile_policy";

#[derive(Debug, Clone, FromQueryResult)]
pub struct OrgRow {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub lifecycle_status: String,
    pub lifecycle_reason: Option<String>,
    pub provisioning_state: String,
    pub creation_source: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
    pub archived_at: Option<DateTime<Utc>>,
    pub profile_policy: Value,
}

impl OrgRow {
    pub fn status(&self) -> LifecycleStatus {
        LifecycleStatus::parse(&self.lifecycle_status).unwrap_or(LifecycleStatus::Suspended)
    }
}

pub const MEMBERSHIP_COLUMNS: &str = "id, organization_id, user_id, role, status, joined_via, created_by, created_at, updated_at, removed_at, version";

#[derive(Debug, Clone, FromQueryResult)]
pub struct MembershipRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub status: String,
    pub joined_via: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl MembershipRow {
    /// Unknown stored values fail closed (GUEST / SUSPENDED grant nothing privileged).
    pub fn role(&self) -> Role {
        Role::parse(&self.role).unwrap_or(Role::Guest)
    }

    pub fn status(&self) -> MembershipStatus {
        MembershipStatus::parse(&self.status).unwrap_or(MembershipStatus::Suspended)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "organizationId": self.organization_id,
            "userId": self.user_id,
            "role": self.role,
            "status": self.status,
            "joinedVia": self.joined_via,
            "createdBy": self.created_by,
            "createdAt": rfc3339(self.created_at),
            "updatedAt": rfc3339(self.updated_at),
            "removedAt": opt_time(self.removed_at),
            "version": self.version,
        })
    }
}

pub const INVITATION_COLUMNS: &str = "id, organization_id, email_normalized, email_hint, role, status, invited_by, invitee_user_id, created_at, expires_at, decided_at, version";

#[derive(Debug, Clone, FromQueryResult)]
pub struct InvitationRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub email_normalized: String,
    pub email_hint: String,
    pub role: String,
    pub status: String,
    pub invited_by: Uuid,
    pub invitee_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl InvitationRow {
    /// The status as observed at `now` (a PENDING invitation past its expiry is EXPIRED
    /// even before the sweeper records it).
    pub fn effective_status(&self, now: DateTime<Utc>) -> &str {
        if self.status == "PENDING" && self.expires_at <= now {
            "EXPIRED"
        } else {
            &self.status
        }
    }

    pub fn to_json(&self, now: DateTime<Utc>) -> Value {
        json!({
            "id": self.id,
            "organizationId": self.organization_id,
            "role": self.role,
            "status": self.effective_status(now),
            "inviteeEmailHint": self.email_hint,
            "invitedBy": self.invited_by,
            "createdAt": rfc3339(self.created_at),
            "expiresAt": rfc3339(self.expires_at),
            "decidedAt": opt_time(self.decided_at),
        })
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ProfileRow {
    pub description: Option<String>,
    pub organization_type: Option<String>,
    pub industry: Option<String>,
    pub country_code: Option<String>,
    pub city: Option<String>,
    pub operating_address: Option<String>,
    pub logo_ref: Option<String>,
    pub public_contact_email: Option<String>,
    pub legal_registered_name: Option<String>,
    pub legal_registration_number: Option<String>,
    pub legal_incorporated_on: Option<NaiveDate>,
    pub legal_verification: Option<String>,
}

impl ProfileRow {
    pub fn values(&self) -> ProfileValues {
        ProfileValues {
            description: self.description.clone(),
            organization_type: self.organization_type.clone(),
            industry: self.industry.clone(),
            country_code: self.country_code.clone(),
            city: self.city.clone(),
            operating_address: self.operating_address.clone(),
            logo_ref: self.logo_ref.clone(),
            public_contact_email: self.public_contact_email.clone(),
            legal: self.legal_verification.as_ref().map(|v| LegalValues {
                registered_name: self.legal_registered_name.clone(),
                registration_number: self.legal_registration_number.clone(),
                incorporated_on: self.legal_incorporated_on,
                verification: v.clone(),
            }),
        }
    }
}

pub fn profile_json(p: &ProfileValues) -> Value {
    json!({
        "description": p.description,
        "organizationType": p.organization_type,
        "industry": p.industry,
        "countryCode": p.country_code,
        "city": p.city,
        "operatingAddress": p.operating_address,
        "logoRef": p.logo_ref,
        "publicContactEmail": p.public_contact_email,
        "legalRegistration": p.legal.as_ref().map(|l| json!({
            "registeredName": l.registered_name,
            "registrationNumber": l.registration_number,
            "incorporatedOn": l.incorporated_on.map(|d| d.format("%Y-%m-%d").to_string()),
            "verification": l.verification,
        })),
    })
}

// ---------------------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------------------

pub async fn org<C: ConnectionTrait>(c: &C, id: Uuid, lock: bool) -> Result<Option<OrgRow>, DbErr> {
    let sql = format!(
        "SELECT {ORG_COLUMNS} FROM organizations WHERE id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    db::one(c, &sql, vec![id.into()]).await
}

/// The non-removed membership of `user` in `organization`.
pub async fn current_membership<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    user: Uuid,
    lock: bool,
) -> Result<Option<MembershipRow>, DbErr> {
    let sql = format!(
        "SELECT {MEMBERSHIP_COLUMNS} FROM organization_memberships WHERE organization_id = $1 AND user_id = $2 AND status <> 'REMOVED'{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    db::one(c, &sql, vec![organization.into(), user.into()]).await
}

pub async fn membership<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<Option<MembershipRow>, DbErr> {
    let sql = format!(
        "SELECT {MEMBERSHIP_COLUMNS} FROM organization_memberships WHERE organization_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    db::one(c, &sql, vec![organization.into(), id.into()]).await
}

pub async fn profile<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
) -> Result<ProfileValues, DbErr> {
    let row: Option<ProfileRow> = db::one(
        c,
        "SELECT description, organization_type, industry, country_code, city, operating_address, logo_ref, public_contact_email, legal_registered_name, legal_registration_number, legal_incorporated_on, legal_verification FROM organization_profiles WHERE organization_id = $1",
        vec![organization.into()],
    )
    .await?;
    Ok(row.map(|r| r.values()).unwrap_or_default())
}

/// Inserts or replaces the profile row.
pub async fn save_profile<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    p: &ProfileValues,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let legal = p.legal.as_ref();
    db::exec(
        c,
        "INSERT INTO organization_profiles (organization_id, description, organization_type, industry, country_code, city, operating_address, logo_ref, public_contact_email, legal_registered_name, legal_registration_number, legal_incorporated_on, legal_verification, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         ON CONFLICT (organization_id) DO UPDATE SET description = EXCLUDED.description, organization_type = EXCLUDED.organization_type, industry = EXCLUDED.industry, country_code = EXCLUDED.country_code, city = EXCLUDED.city, operating_address = EXCLUDED.operating_address, logo_ref = EXCLUDED.logo_ref, public_contact_email = EXCLUDED.public_contact_email, legal_registered_name = EXCLUDED.legal_registered_name, legal_registration_number = EXCLUDED.legal_registration_number, legal_incorporated_on = EXCLUDED.legal_incorporated_on, legal_verification = EXCLUDED.legal_verification, updated_at = EXCLUDED.updated_at",
        vec![
            organization.into(),
            p.description.clone().into(),
            p.organization_type.clone().into(),
            p.industry.clone().into(),
            p.country_code.clone().into(),
            p.city.clone().into(),
            p.operating_address.clone().into(),
            p.logo_ref.clone().into(),
            p.public_contact_email.clone().into(),
            legal.and_then(|l| l.registered_name.clone()).into(),
            legal.and_then(|l| l.registration_number.clone()).into(),
            legal.and_then(|l| l.incorporated_on).into(),
            legal.map(|l| l.verification.clone()).into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

pub async fn active_member_count<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
) -> Result<i64, DbErr> {
    db::scalar_i64(
        c,
        "SELECT count(*)::bigint AS n FROM organization_memberships WHERE organization_id = $1 AND status = 'ACTIVE'",
        vec![organization.into()],
    )
    .await
}

/// Active OWNER memberships of `organization` other than `except`.
pub async fn other_active_owners<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    except: Uuid,
) -> Result<i64, DbErr> {
    db::scalar_i64(
        c,
        "SELECT count(*)::bigint AS n FROM organization_memberships WHERE organization_id = $1 AND role = 'OWNER' AND status = 'ACTIVE' AND id <> $2",
        vec![organization.into(), except.into()],
    )
    .await
}

/// The stored policy of an organization (absent = revision 0, defaults).
#[derive(Debug, Clone, FromQueryResult)]
pub struct PolicyRow {
    pub revision: i64,
    pub document: Value,
    pub updated_by: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

pub async fn policy_row<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    lock: bool,
) -> Result<Option<PolicyRow>, DbErr> {
    let sql = format!(
        "SELECT revision, document, updated_by, updated_at FROM organization_policies WHERE organization_id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    db::one(c, &sql, vec![organization.into()]).await
}

/// Parses a stored policy document; an absent one is the defaults. A stored document
/// that no longer parses fails the request (fail closed: never fall back to defaults,
/// which may be more permissive).
pub fn parse_policy(document: Option<&Value>) -> Result<PolicyDocument, DbErr> {
    match document {
        None | Some(Value::Null) => Ok(PolicyDocument::default()),
        Some(v) => PolicyDocument::parse(v)
            .map_err(|_| DbErr::Custom("stored organization policy is invalid".into())),
    }
}

/// The effective policy of an organization.
pub async fn policy<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
) -> Result<PolicyDocument, DbErr> {
    let row = policy_row(c, organization, false).await?;
    parse_policy(row.as_ref().map(|r| &r.document))
}

/// The Organization resource; `viewer` is the actor's membership (detail for an actor).
pub async fn organization_json<C: ConnectionTrait>(
    c: &C,
    org: &OrgRow,
    viewer: Option<&MembershipRow>,
) -> Result<Value, DbErr> {
    let profile = profile(c, org.id).await?;
    let member_count = active_member_count(c, org.id).await?;
    let mut doc = json!({
        "id": org.id,
        "name": org.name,
        "slug": org.slug,
        "lifecycleStatus": org.lifecycle_status,
        "provisioningState": org.provisioning_state,
        "creationSource": org.creation_source,
        "createdBy": org.created_by,
        "createdAt": rfc3339(org.created_at),
        "updatedAt": rfc3339(org.updated_at),
        "version": org.version,
        "archivedAt": opt_time(org.archived_at),
        "profile": profile_json(&profile),
        "memberCount": member_count,
    });
    if let Some(m) = viewer {
        let policy = policy(c, org.id).await?;
        doc["viewer"] = json!({
            "membershipId": m.id,
            "role": m.role,
            "status": m.status,
            "capabilities": effective_capabilities_with_policy(m.role(), m.status(), org.status(), &policy),
        });
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------------------
// Writers (always inside the change's transaction)
// ---------------------------------------------------------------------------------------

/// Appends an audit event.
#[allow(clippy::too_many_arguments)]
pub async fn audit<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    organization: Option<Uuid>,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
    summary: Value,
) -> Result<(), DbErr> {
    db::exec(
        c,
        "INSERT INTO organization_audit_events (id, organization_id, actor_kind, actor_user_id, actor_service, action, target_type, target_id, summary, request_id, correlation_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            Uuid::new_v4().into(),
            organization.into(),
            ctx.kind.as_str().into(),
            ctx.user_id.into(),
            ctx.service.clone().into(),
            action.into(),
            target_type.into(),
            target_id.into(),
            summary.into(),
            ctx.request_id.clone().into(),
            ctx.correlation_id.clone().into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Appends a membership history row describing `before` → `after`.
pub async fn history<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    before: Option<&MembershipRow>,
    after: &MembershipRow,
    change_kind: &str,
) -> Result<(), DbErr> {
    db::exec(
        c,
        "INSERT INTO organization_membership_history (id, membership_id, organization_id, user_id, change_kind, from_role, to_role, from_status, to_status, membership_version, actor_kind, actor_user_id, actor_service, request_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        vec![
            Uuid::new_v4().into(),
            after.id.into(),
            after.organization_id.into(),
            after.user_id.into(),
            change_kind.into(),
            before.map(|b| b.role.clone()).into(),
            after.role.clone().into(),
            before.map(|b| b.status.clone()).into(),
            after.status.clone().into(),
            after.version.into(),
            ctx.kind.as_str().into(),
            ctx.user_id.into(),
            ctx.service.clone().into(),
            ctx.request_id.clone().into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One outbox event (envelope of `docs/events/organization-event-contracts.md` §1).
pub struct Event<'a> {
    pub organization_id: Uuid,
    pub aggregate_type: &'a str,
    pub aggregate_id: Uuid,
    pub aggregate_version: i64,
    pub event_type: &'a str,
    pub payload: Value,
}

/// Records `event` once for the reconciliation record and once per subscribed consumer
/// ([`crate::outbox::subscribers`]), in the caller's transaction.
pub async fn emit<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    event: Event<'_>,
) -> Result<Uuid, DbErr> {
    let event_id = Uuid::new_v4();
    let envelope = json!({
        "eventId": event_id,
        "eventType": event.event_type,
        "schemaVersion": 1,
        "organizationId": event.organization_id,
        "aggregateType": event.aggregate_type,
        "aggregateId": event.aggregate_id,
        "aggregateVersion": event.aggregate_version,
        "occurredAt": rfc3339(now),
        "actor": ctx.actor_json(),
        "correlationId": hop_uuid(ctx.correlation_id.as_deref()),
        "causationId": hop_uuid(ctx.request_id.as_deref()),
        "payload": event.payload,
    });
    let destinations = std::iter::once(RECONCILIATION_DESTINATION)
        .chain(crate::outbox::subscribers(event.event_type).iter().copied());
    for destination in destinations {
        db::exec(
            c,
            "INSERT INTO organization_outbox (event_id, destination, organization_id, aggregate_type, aggregate_id, aggregate_version, event_type, payload, created_at, next_attempt_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
            vec![
                event_id.into(),
                destination.into(),
                event.organization_id.into(),
                event.aggregate_type.into(),
                event.aggregate_id.into(),
                event.aggregate_version.into(),
                event.event_type.into(),
                envelope.clone().into(),
                now.into(),
            ],
        )
        .await?;
    }
    Ok(event_id)
}

/// `membership.activated` for `m`.
pub async fn emit_membership_activated<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    m: &MembershipRow,
) -> Result<(), DbErr> {
    emit(
        c,
        ctx,
        now,
        Event {
            organization_id: m.organization_id,
            aggregate_type: "membership",
            aggregate_id: m.id,
            aggregate_version: m.version,
            event_type: "membership.activated",
            payload: json!({ "userId": m.user_id, "role": m.role, "membershipVersion": m.version }),
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_ids_become_envelope_uuids() {
        let u = hop_uuid(Some("corr_0123456789abcdef0123456789abcdef"));
        assert_eq!(u.to_string(), "01234567-89ab-cdef-0123-456789abcdef");
        // Missing / malformed ids still give a UUID (never a panic).
        let _ = hop_uuid(None);
        let _ = hop_uuid(Some("corr_nothex"));
    }

    #[test]
    fn timestamps_are_rfc3339_utc_with_microseconds() {
        let t = DateTime::parse_from_rfc3339("2026-10-04T12:00:00.123456Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(rfc3339(t), "2026-10-04T12:00:00.123456Z");
    }
}

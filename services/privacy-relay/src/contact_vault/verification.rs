//! Contact verification (§33).
//!
//! States: UNVERIFIED (on creation and after every telephone-value change), PENDING,
//! VERIFIED (optionally until `validUntil`), EXPIRED (derived at READ time from
//! `validUntil`, materialized later by the worker), INVALID and BLOCKED (never resolved,
//! whatever earlier events said).
//!
//! Only a trusted verification integration (`privacy.contacts.verify`) asserts a state,
//! with a bounded trust source and an opaque evidence reference — never arbitrary
//! evidence JSON; ordinary writers cannot (create / update bodies refuse verification
//! members). Every change appends `contact_verification_events` and the access audit.
//! Restrictive assertions (BLOCKED, INVALID, UNVERIFIED) keep working while the Contact
//! Vault is disabled.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    audit::{record, record_refusal, AccessEntry},
    model::{ContactRow, VerificationState, CONTACT_COLUMNS},
    repository, ContactActor, ContactOperation, ContactVault,
};
use crate::{
    db,
    error::{ErrorCode, RelayError, RelayResult},
    relay::authorization::ContactUse,
    scope::RelayScope,
};

/// Policy version recorded with every verification event.
pub const VERIFICATION_POLICY: &str = "CONTACT_VERIFICATION_V1";
/// Longest accepted verification validity.
const MAX_VALIDITY_DAYS: i64 = 366;

/// `POST /v1/privacy-relay/contacts/{ref}/verification`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationAssertion {
    pub expected_version: i64,
    pub target_state: String,
    pub trust_source: String,
    #[serde(default)]
    pub evidence_reference: Option<String>,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
}

fn trust_source_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[derive(sea_orm::FromQueryResult)]
struct CountryRow {
    id: Uuid,
    country_iso2: Option<String>,
}

/// The routing country of protected contacts of `scope` (never a number), by reference.
pub async fn contact_countries<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    ids: &[Uuid],
) -> RelayResult<Vec<(Uuid, Option<String>)>> {
    let rows: Vec<CountryRow> = db::all(
        conn,
        "SELECT id, country_iso2 FROM protected_contacts \
         WHERE id = ANY($1) AND tenant_id = $2 AND application_id = $3 AND environment_id = $4 \
         ORDER BY id",
        vec![
            ids.to_vec().into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
        ],
    )
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.country_iso2)).collect())
}

/// Usability of a contact for an authorization decision (share-locked).
pub async fn contact_use<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    id: Uuid,
    now: DateTime<Utc>,
) -> RelayResult<ContactUse> {
    let row: Option<ContactRow> = db::one(
        conn,
        &format!(
            "SELECT {CONTACT_COLUMNS} FROM protected_contacts \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4 FOR SHARE"
        ),
        vec![
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            id.into(),
        ],
    )
    .await?;
    let Some(row) = row else {
        return Ok(ContactUse::Unavailable);
    };
    if row.is_disabled() {
        return Ok(ContactUse::Unavailable);
    }
    Ok(match row.effective_state(now)? {
        VerificationState::Verified => ContactUse::Verified,
        VerificationState::Expired => ContactUse::Expired,
        VerificationState::Invalid | VerificationState::Blocked => ContactUse::Blocked,
        VerificationState::Unverified | VerificationState::Pending => ContactUse::NotVerified,
    })
}

/// The public-network verification guard: an enabled contact whose EFFECTIVE state is
/// VERIFIED. BLOCKED / INVALID are refused whatever their history; expiry is evaluated
/// at `now`, not by a worker.
pub fn require_verified(row: &ContactRow, now: DateTime<Utc>) -> RelayResult<()> {
    if row.is_erased() {
        return Err(super::erased());
    }
    if row.is_disabled() {
        return Err(RelayError::Refused(ErrorCode::ContactDisabled));
    }
    match row.effective_state(now)? {
        VerificationState::Verified => Ok(()),
        VerificationState::Invalid | VerificationState::Blocked => {
            Err(RelayError::Refused(ErrorCode::ContactBlocked))
        }
        VerificationState::Expired => {
            Err(RelayError::Refused(ErrorCode::ContactVerificationExpired))
        }
        VerificationState::Unverified | VerificationState::Pending => {
            Err(RelayError::Refused(ErrorCode::ContactNotVerified))
        }
    }
}

impl ContactVault {
    /// Reusable guard for every future public-network route: the contact of `scope`,
    /// enabled and effectively VERIFIED now. Does NOT originate anything.
    pub async fn require_public_network_contact_verified(
        &self,
        scope: RelayScope,
        id: Uuid,
    ) -> RelayResult<ContactRow> {
        let row = repository::find(&self.db, scope, id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::ContactNotFound))?;
        require_verified(&row, self.now())?;
        Ok(row)
    }

    /// A trusted verification assertion.
    pub async fn assert_verification(
        &self,
        actor: &ContactActor,
        id: Uuid,
        request: VerificationAssertion,
    ) -> RelayResult<Value> {
        let entry =
            AccessEntry::new(ContactOperation::Verify, Some(id)).purpose("CONTACT_VERIFICATION");
        match self.assert_inner(actor, id, request).await {
            Ok(v) => Ok(v),
            Err(e) => {
                record_refusal(&self.db, actor, entry, &e, self.now()).await;
                Err(e)
            }
        }
    }

    async fn assert_inner(
        &self,
        actor: &ContactActor,
        id: Uuid,
        request: VerificationAssertion,
    ) -> RelayResult<Value> {
        if request.expected_version < 1 {
            return Err(RelayError::Validation(
                "expectedVersion must be a positive integer.",
            ));
        }
        let target = match VerificationState::parse(&request.target_state) {
            Some(VerificationState::Expired) | None => {
                return Err(RelayError::Validation(
                    "targetState must be UNVERIFIED, PENDING, VERIFIED, INVALID or BLOCKED.",
                ))
            }
            Some(s) => s,
        };
        if !trust_source_ok(&request.trust_source) {
            return Err(RelayError::Validation(
                "trustSource must be an upper-case code of at most 64 characters.",
            ));
        }
        if let Some(evidence) = &request.evidence_reference {
            crate::context::state::validate_external_reference(evidence).map_err(|_| {
                RelayError::Validation("evidenceReference must be an opaque, safe reference (never a number, address or URI).")
            })?;
        }
        let now = self.now();
        let valid_until = match (target, request.valid_until) {
            (VerificationState::Verified, Some(until)) => {
                if until <= now || until > now + Duration::days(MAX_VALIDITY_DAYS) {
                    return Err(RelayError::Validation(
                        "validUntil must be in the future and at most 366 days away.",
                    ));
                }
                Some(until)
            }
            (VerificationState::Verified, None) => None,
            (_, Some(_)) => {
                return Err(RelayError::Validation(
                    "validUntil is only accepted with targetState VERIFIED.",
                ))
            }
            (_, None) => None,
        };
        let restrictive = matches!(
            target,
            VerificationState::Blocked | VerificationState::Invalid | VerificationState::Unverified
        );
        if !restrictive && !self.enabled.enabled() {
            return Err(RelayError::Refused(ErrorCode::ContactVaultDisabled));
        }
        let txn = self.db.begin().await?;
        let row = repository::find(&txn, actor.scope, id, true)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::ContactNotFound))?;
        if row.version != request.expected_version {
            return Err(RelayError::Refused(ErrorCode::ContactVersionConflict));
        }
        if row.is_disabled() && !restrictive {
            return Err(RelayError::Refused(ErrorCode::ContactDisabled));
        }
        let from = row.effective_state(now)?;
        set_state(&txn, &row, target, valid_until, now).await?;
        append_event(
            &txn,
            actor,
            &row,
            row.version + 1,
            from,
            target,
            &request.trust_source,
            request.evidence_reference.as_deref(),
            valid_until,
            now,
        )
        .await?;
        record(
            &txn,
            actor,
            &AccessEntry::new(ContactOperation::Verify, Some(id)).purpose("CONTACT_VERIFICATION"),
            now,
        )
        .await?;
        let row = repository::find(&txn, actor.scope, id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
        txn.commit().await?;
        tracing::info!(contact_reference = %id, from = from.as_str(), to = target.as_str(), "contact verification asserted");
        Ok(json!({ "contact": row.view(now)? }))
    }

    /// Materializes EXPIRED for VERIFIED contacts whose validity has passed (authority
    /// already treats them as expired at read time). Bounded, SKIP LOCKED.
    pub async fn expire_verifications(&self, limit: u64) -> RelayResult<u64> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let rows: Vec<ContactRow> = db::all(
            &txn,
            &format!(
                "SELECT {CONTACT_COLUMNS} FROM protected_contacts \
                 WHERE verification_state = 'VERIFIED' AND verification_expires_at <= $1 AND erased_at IS NULL \
                 ORDER BY verification_expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into(), (limit.clamp(1, 1000) as i64).into()],
        )
        .await?;
        for row in &rows {
            let actor = ContactActor::system(row.scope());
            set_state(
                &txn,
                row,
                VerificationState::Expired,
                row.verification_expires_at,
                now,
            )
            .await?;
            append_event(
                &txn,
                &actor,
                row,
                row.version + 1,
                VerificationState::Verified,
                VerificationState::Expired,
                "VALIDITY_EXPIRED",
                None,
                None,
                now,
            )
            .await?;
            record(
                &txn,
                &actor,
                &AccessEntry::new(ContactOperation::ExpireVerification, Some(row.id))
                    .purpose("CONTACT_VERIFICATION"),
                now,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(rows.len() as u64)
    }
}

async fn set_state<C: ConnectionTrait>(
    conn: &C,
    row: &ContactRow,
    to: VerificationState,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        conn,
        "UPDATE protected_contacts SET verification_state = $2, verification_expires_at = $3, \
           verification_updated_at = $4, version = version + 1, updated_at = $4 WHERE id = $1",
        vec![
            row.id.into(),
            to.as_str().into(),
            expires_at.into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
async fn append_event<C: ConnectionTrait>(
    conn: &C,
    actor: &ContactActor,
    row: &ContactRow,
    contact_version: i64,
    from: VerificationState,
    to: VerificationState,
    trust_source: &str,
    evidence: Option<&str>,
    valid_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        conn,
        "INSERT INTO contact_verification_events (id, contact_reference, tenant_id, application_id, environment_id, \
           contact_version, from_state, to_state, trust_source, evidence_reference, policy_version, valid_until, \
           actor_kind, actor_service, service_account_id, request_id, correlation_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        vec![
            Uuid::new_v4().into(),
            row.id.into(),
            row.tenant_id.into(),
            row.application_id.into(),
            row.environment_id.into(),
            contact_version.into(),
            from.as_str().into(),
            to.as_str().into(),
            trust_source.into(),
            evidence.map(str::to_string).into(),
            VERIFICATION_POLICY.into(),
            valid_until.into(),
            actor.kind.as_str().into(),
            actor.service.as_str().into(),
            actor.service_account_id.into(),
            actor.request_id.as_str().into(),
            actor.correlation_id.as_str().into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

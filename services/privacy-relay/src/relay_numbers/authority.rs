//! Part IV Part P §§119–120: who decides that a relay number may be lent.
//!
//! * `RELAY_NUMBER_AUTHORITY=LOCAL` (default, and the ROLLBACK position): Privacy Relay's
//!   own inventory decides, exactly as in Part III.
//! * `RELAY_NUMBER_AUTHORITY=NUMBERING` (cutover): before a number is lent to a session,
//!   Numbering — the authority over every public number — must grant a temporary LEASE
//!   for it (`number-relay.lease`); Numbering refuses a number it took out of service,
//!   retired or already leased, and its `RELAY_LEASES` kill switch. Numbering unavailable
//!   → the assignment is refused (fail closed). Privacy Relay keeps its sessions, their
//!   ids, the assignments, their generations and the inbound mapping unchanged.
//!
//! Lease releases are durable: the assignment's end marks its lease RELEASE_PENDING in
//! the same transaction and the worker sweep releases it at Numbering (idempotent),
//! whatever the current mode — rolling back to LOCAL never strands a lease.
//!
//! The backfill ([`RelayService::backfill_numbering`]) imports every inventory row into
//! Numbering (idempotent, keyed by the inventory id) and adopts every ACTIVE assignment
//! (a lease with the assignment's own id and generation), so Numbering's compatibility
//! resolution agrees with Privacy Relay's existing mappings before cutover;
//! [`RelayService::verify_numbering_compatibility`] proves it.

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use crate::{db, error::RelayResult, service::RelayService};

/// `RELAY_NUMBER_AUTHORITY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthorityMode {
    #[default]
    Local,
    Numbering,
}

impl AuthorityMode {
    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "LOCAL" => Some(AuthorityMode::Local),
            "NUMBERING" => Some(AuthorityMode::Numbering),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AuthorityMode::Local => "LOCAL",
            AuthorityMode::Numbering => "NUMBERING",
        }
    }
}

/// Why Numbering did not grant / release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    /// Unreachable, timed out or not configured: fail closed, retry later.
    Unavailable,
    /// A definite refusal (stable rule / error code).
    Refused(String),
}

/// A granted lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedLease {
    pub lease_id: Uuid,
    pub generation: i64,
}

/// One inventory row to import.
#[derive(Debug, Clone)]
pub struct ImportRow {
    pub relay_inventory_ref: Uuid,
    pub e164: String,
    pub country: String,
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    pub binding_ref: String,
}

/// Numbering's view of one relay number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberingResolution {
    pub lifecycle_state: String,
    pub leasable: bool,
    pub lease: Option<(Uuid, Uuid, i64, String)>,
}

/// The neutral boundary to Numbering's relay-number authority.
#[async_trait]
pub trait NumberAuthority: Send + Sync {
    async fn import(&self, row: &ImportRow) -> Result<(), AuthorityError>;
    async fn lease(
        &self,
        relay_inventory_ref: Uuid,
        holder_ref: Uuid,
        generation: i64,
        expires_at: DateTime<Utc>,
    ) -> Result<GrantedLease, AuthorityError>;
    async fn release(&self, lease_id: Uuid, reason: &str) -> Result<(), AuthorityError>;
    async fn resolve(
        &self,
        relay_inventory_ref: Uuid,
    ) -> Result<NumberingResolution, AuthorityError>;
}

/// The HTTP adapter: Numbering's `/internal/v1` relay API with Privacy Relay's service
/// token (`number-relay.lease`).
pub struct HttpNumberAuthority {
    client: Arc<InternalServiceClient>,
}

impl HttpNumberAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }

    fn request() -> OutboundServiceRequest {
        OutboundServiceRequest::new_operation(ServiceId::numbering())
    }
}

fn classify(e: InternalCallError) -> AuthorityError {
    match e {
        InternalCallError::Status { status, code, .. }
            if (400..500).contains(&status) && status != 429 =>
        {
            AuthorityError::Refused(code.unwrap_or_else(|| format!("HTTP_{status}")))
        }
        _ => AuthorityError::Unavailable,
    }
}

fn reason_of(body: &Value) -> Option<String> {
    body["error"]["reason"]
        .as_str()
        .or(body["error"]["code"].as_str())
        .map(str::to_string)
}

#[async_trait]
impl NumberAuthority for HttpNumberAuthority {
    async fn import(&self, row: &ImportRow) -> Result<(), AuthorityError> {
        let mut body = json!({
            "relayInventoryRef": row.relay_inventory_ref,
            "e164": row.e164,
            "country": row.country,
            "capabilities": row.capabilities,
            "bindingRef": row.binding_ref,
        });
        if let Some(r) = &row.region {
            body["region"] = json!(r);
        }
        self.client
            .post_json(
                &Self::request(),
                "/internal/v1/relay-inventory/imports",
                &body,
            )
            .await
            .map(|_| ())
            .map_err(classify)
    }

    async fn lease(
        &self,
        relay_inventory_ref: Uuid,
        holder_ref: Uuid,
        generation: i64,
        expires_at: DateTime<Utc>,
    ) -> Result<GrantedLease, AuthorityError> {
        let body = json!({
            "relayInventoryRef": relay_inventory_ref,
            "holderRef": holder_ref,
            "generation": generation,
            "expiresAt": expires_at,
            // Idempotent per assignment: a retried request never takes a second lease.
            "idempotencyKey": format!("relay-assignment:{}", holder_ref.simple()),
        });
        let r = self
            .client
            .exchange_json(
                &Self::request(),
                axum::http::Method::POST,
                "/internal/v1/relay-leases",
                Some(&body),
            )
            .await
            .map_err(classify)?;
        let v: Value = r.json().unwrap_or(Value::Null);
        match r.status() {
            200 | 201 => {
                let id = v["lease"]["leaseId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or(AuthorityError::Unavailable)?;
                Ok(GrantedLease {
                    lease_id: id,
                    generation: v["lease"]["generation"].as_i64().unwrap_or(generation),
                })
            }
            429 | 500..=599 => Err(AuthorityError::Unavailable),
            s => Err(AuthorityError::Refused(
                reason_of(&v).unwrap_or_else(|| format!("HTTP_{s}")),
            )),
        }
    }

    async fn release(&self, lease_id: Uuid, reason: &str) -> Result<(), AuthorityError> {
        self.client
            .post_json(
                &Self::request(),
                &format!("/internal/v1/relay-leases/{lease_id}/release"),
                &json!({ "reasonCode": reason }),
            )
            .await
            .map(|_| ())
            .map_err(classify)
    }

    async fn resolve(
        &self,
        relay_inventory_ref: Uuid,
    ) -> Result<NumberingResolution, AuthorityError> {
        let r = self
            .client
            .get(
                &Self::request(),
                &format!("/internal/v1/relay-numbers/{relay_inventory_ref}/resolution"),
            )
            .await
            .map_err(classify)?;
        let v: Value = r.json().unwrap_or(Value::Null);
        let lease = v["lease"].as_object().and_then(|l| {
            Some((
                Uuid::parse_str(l.get("leaseId")?.as_str()?).ok()?,
                Uuid::parse_str(l.get("holderRef")?.as_str()?).ok()?,
                l.get("generation")?.as_i64()?,
                l.get("status")?.as_str()?.to_string(),
            ))
        });
        Ok(NumberingResolution {
            lifecycle_state: v["lifecycleState"].as_str().unwrap_or_default().to_string(),
            leasable: v["leasable"].as_bool().unwrap_or(false),
            lease,
        })
    }
}

/// The backfill result (counts only, never a number).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillReport {
    pub imported: usize,
    pub adopted: usize,
    pub already_leased: usize,
    pub failed: usize,
}

/// The pre-cutover compatibility proof (counts only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilityReport {
    pub active_assignments: usize,
    pub matching_leases: usize,
    pub mismatched: usize,
    pub unknown_to_numbering: usize,
}

impl CompatibilityReport {
    pub fn compatible(&self) -> bool {
        self.mismatched == 0
            && self.unknown_to_numbering == 0
            && self.matching_leases == self.active_assignments
    }
}

#[derive(Debug, FromQueryResult)]
struct ImportCandidate {
    id: Uuid,
    canonical_public_e164: String,
    country_iso2: String,
    region: Option<String>,
    capabilities: Vec<String>,
    provider_binding_ref: String,
}

#[derive(Debug, FromQueryResult)]
struct ActiveAssignment {
    id: Uuid,
    relay_number_id: Uuid,
    assignment_generation: i64,
    expires_at: DateTime<Utc>,
    leased: bool,
}

#[derive(Debug, FromQueryResult)]
struct PendingRelease {
    assignment_id: Uuid,
    numbering_lease_id: Uuid,
}

impl RelayService {
    fn authority(&self) -> Option<&Arc<dyn NumberAuthority>> {
        self.number_authority.as_ref()
    }

    /// Imports every inventory row into Numbering and adopts every ACTIVE assignment that
    /// has no lease yet (idempotent; safe to run again at any time).
    pub async fn backfill_numbering(&self) -> RelayResult<BackfillReport> {
        let mut report = BackfillReport::default();
        let Some(authority) = self.authority().cloned() else {
            return Ok(report);
        };
        let rows: Vec<ImportCandidate> = db::all(
            &self.db,
            "SELECT id, canonical_public_e164, country_iso2, region, capabilities, provider_binding_ref \
               FROM relay_number_inventory ORDER BY created_at, id",
            vec![],
        )
        .await?;
        for r in rows {
            let import = ImportRow {
                relay_inventory_ref: r.id,
                e164: r.canonical_public_e164,
                country: r.country_iso2,
                region: r.region,
                capabilities: r.capabilities,
                binding_ref: r.provider_binding_ref,
            };
            match authority.import(&import).await {
                Ok(()) => report.imported += 1,
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(relay_number_id = %r.id, error = ?e, "relay number import failed");
                }
            }
        }
        let active: Vec<ActiveAssignment> = db::all(
            &self.db,
            "SELECT a.id, a.relay_number_id, a.assignment_generation, a.expires_at, \
                    EXISTS (SELECT 1 FROM relay_number_leases l WHERE l.assignment_id = a.id) AS leased \
               FROM relay_number_assignments a WHERE a.status = 'ACTIVE' ORDER BY a.assigned_at, a.id",
            vec![],
        )
        .await?;
        for a in active {
            if a.leased {
                report.already_leased += 1;
                continue;
            }
            match authority
                .lease(
                    a.relay_number_id,
                    a.id,
                    a.assignment_generation,
                    a.expires_at,
                )
                .await
            {
                Ok(lease) => {
                    db::exec(
                        &self.db,
                        "INSERT INTO relay_number_leases (assignment_id, relay_number_id, numbering_lease_id, generation, \
                           status, acquired_at) VALUES ($1, $2, $3, $4, 'ACTIVE', $5) ON CONFLICT (assignment_id) DO NOTHING",
                        vec![
                            a.id.into(),
                            a.relay_number_id.into(),
                            lease.lease_id.into(),
                            a.assignment_generation.into(),
                            self.now().into(),
                        ],
                    )
                    .await?;
                    report.adopted += 1;
                }
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(assignment_id = %a.id, error = ?e, "relay assignment adoption failed");
                }
            }
        }
        crate::metrics::inc("privacy_relay_numbering_backfills_total", &[]);
        Ok(report)
    }

    /// Whether Numbering's resolution of every ACTIVE assignment's number names exactly
    /// that assignment (holder) and generation — the condition for cutover.
    pub async fn verify_numbering_compatibility(&self) -> RelayResult<CompatibilityReport> {
        let mut report = CompatibilityReport::default();
        let Some(authority) = self.authority().cloned() else {
            return Ok(report);
        };
        let active: Vec<ActiveAssignment> = db::all(
            &self.db,
            "SELECT a.id, a.relay_number_id, a.assignment_generation, a.expires_at, FALSE AS leased \
               FROM relay_number_assignments a WHERE a.status = 'ACTIVE' ORDER BY a.assigned_at, a.id",
            vec![],
        )
        .await?;
        for a in active {
            report.active_assignments += 1;
            match authority.resolve(a.relay_number_id).await {
                Ok(r) => match r.lease {
                    Some((_, holder, generation, status))
                        if holder == a.id
                            && generation == a.assignment_generation
                            && status == "ACTIVE" =>
                    {
                        report.matching_leases += 1
                    }
                    _ => report.mismatched += 1,
                },
                Err(_) => report.unknown_to_numbering += 1,
            }
        }
        Ok(report)
    }

    /// Backfill + compatibility check; in NUMBERING mode a compatible result opens the
    /// cutover (new assignments are leased from Numbering from then on). Idempotent.
    pub async fn prepare_numbering_cutover(&self) -> RelayResult<CompatibilityReport> {
        self.backfill_numbering().await?;
        let report = self.verify_numbering_compatibility().await?;
        if self.authority_mode == AuthorityMode::Numbering && self.authority().is_some() {
            let ok = report.compatible();
            self.numbering_cutover
                .store(ok, std::sync::atomic::Ordering::SeqCst);
            if ok {
                tracing::info!(
                    active = report.active_assignments,
                    "relay-number authority: Numbering cutover verified"
                );
            } else {
                tracing::error!(?report, "relay-number authority: Numbering compatibility NOT verified; new assignments stay refused");
            }
        }
        Ok(report)
    }

    /// Worker: releases at Numbering every lease whose assignment ended (idempotent; a
    /// failure is retried on the next sweep, never dropped).
    pub async fn release_numbering_leases(&self, limit: u64) -> RelayResult<usize> {
        let Some(authority) = self.authority().cloned() else {
            return Ok(0);
        };
        let due: Vec<PendingRelease> = db::all(
            &self.db,
            "SELECT assignment_id, numbering_lease_id FROM relay_number_leases WHERE status = 'RELEASE_PENDING' \
             ORDER BY release_requested_at LIMIT $1",
            vec![(limit.clamp(1, 500) as i64).into()],
        )
        .await?;
        let mut released = 0;
        for p in due {
            match authority
                .release(p.numbering_lease_id, "ASSIGNMENT_ENDED")
                .await
            {
                Ok(()) | Err(AuthorityError::Refused(_)) => {
                    // A refusal means Numbering no longer holds it for us (ended / expired).
                    db::exec(
                        &self.db,
                        "UPDATE relay_number_leases SET status = 'RELEASED', released_at = $2 \
                         WHERE assignment_id = $1 AND status = 'RELEASE_PENDING'",
                        vec![p.assignment_id.into(), self.now().into()],
                    )
                    .await?;
                    released += 1;
                }
                Err(AuthorityError::Unavailable) => {
                    db::exec(
                        &self.db,
                        "UPDATE relay_number_leases SET release_attempts = release_attempts + 1, \
                           last_error = 'NUMBERING_UNAVAILABLE' WHERE assignment_id = $1 AND status = 'RELEASE_PENDING'",
                        vec![p.assignment_id.into()],
                    )
                    .await?;
                }
            }
        }
        Ok(released)
    }
}

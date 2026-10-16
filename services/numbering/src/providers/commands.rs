//! Durable provider command execution (Part IV Workstream G §12).
//!
//! Every provider-affecting operation is a `number_provider_commands` row:
//!
//! 1. **Intent** — one transaction writes the local state the operation implies (e.g. the
//!    PENDING reservation and RESERVING, or PROVISIONING / RELEASING), the command, its
//!    dispatch job and the outbox event. A repeated request with the same idempotency key
//!    returns the existing command; a different request with it is refused.
//! 2. **Dispatch** (`provider.dispatch` job) — the command becomes DISPATCHING (committed:
//!    from here on the request may reach the provider), then the adapter is called with
//!    the command id as provider-side idempotency key.
//! 3. **Evidence and transition** — one transaction appends the attempt (transmitted?,
//!    outcome, provider reference, bounded response) and applies the canonical transition
//!    through the domain's own transition functions (the command never owns lifecycle
//!    authority). A definite not-transmitted failure is retried with backoff; a definite
//!    failure reverts the intent; UNKNOWN enters UNKNOWN_PROVIDER_STATE (where the
//!    lifecycle has it), raises an incident and schedules read-back reconciliation — the
//!    request is never sent again blindly.
//!
//! A process interruption between 2 and 3 leaves the command DISPATCHING and the job
//! (RECONCILE_ON_UNCERTAINTY) parked as UNKNOWN_OUTCOME; [`ProviderCommands::recover_interrupted`]
//! moves such commands to UNKNOWN and reconciliation takes over.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    dispatch, incidents,
    registry::{ProviderRegistry, SYNTHETIC_PROVIDER},
    simple_enum, CommandOperation, InventoryQuery, OperationKind, OperationReceipt,
    OperationRequest, ProviderFailure, ProviderOutcome,
};
use crate::{
    domain::{RequestContext, SharedClock},
    infra::{
        jobs::{self, ClaimedJob, JobHandler, JobOutcome, NewJob, RetrySafety},
        outbox::{self, NewEvent},
    },
    numbers::{
        bindings::NewBinding,
        capabilities::NewCapability,
        inventory::NewObservation,
        lifecycle::{transition_in, TransitionRequest},
        reservations::ReservationRow,
        resources::{lock, NewResource, ResourceFacts, ResourceRow},
        stmt, Actor, ActorKind, Availability, BindingChangeReason, Capability, CapabilitySource,
        ClassificationSource, DomainError, DomainResult, FailureCategory, InventorySource,
        LifecycleState as S, NumberDomain, OwnerScope, RestrictionFlag, UncertainOperation,
    },
};

pub const DISPATCH_JOB: &str = "provider.dispatch";
pub const RECONCILE_JOB: &str = "provider.reconcile";

simple_enum!(CommandStatus {
    Pending = "PENDING",
    Dispatching = "DISPATCHING",
    Succeeded = "SUCCEEDED",
    Failed = "FAILED",
    Unsupported = "UNSUPPORTED",
    Unknown = "UNKNOWN",
    ReconciledSucceeded = "RECONCILED_SUCCEEDED",
    ReconciledFailed = "RECONCILED_FAILED",
});

impl CommandStatus {
    pub fn is_final(&self) -> bool {
        !matches!(
            self,
            CommandStatus::Pending | CommandStatus::Dispatching | CommandStatus::Unknown
        )
    }
}

/// The edges of the command status (mirrored by `number_provider_command_transition_allowed`).
pub fn command_allowed(from: CommandStatus, to: CommandStatus) -> bool {
    use CommandStatus as C;
    matches!(
        (from, to),
        (C::Pending, C::Dispatching | C::Failed | C::Unsupported)
            | (
                C::Dispatching,
                C::Succeeded | C::Failed | C::Unsupported | C::Unknown | C::Pending
            )
            | (C::Unknown, C::ReconciledSucceeded | C::ReconciledFailed)
    )
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRow {
    pub id: Uuid,
    #[serde(skip)]
    pub provider_id: String,
    pub operation: String,
    pub number_resource_id: Uuid,
    pub reservation_id: Option<Uuid>,
    #[serde(skip)]
    pub idempotency_key: String,
    #[serde(skip)]
    pub request: Value,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub job_id: Option<Uuid>,
    #[serde(skip)]
    pub external_ref: Option<String>,
    pub result: Option<Value>,
    pub failure_code: Option<String>,
    pub revision: i64,
    pub correlation_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl CommandRow {
    pub fn status(&self) -> CommandStatus {
        CommandStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn operation(&self) -> CommandOperation {
        CommandOperation::parse(&self.operation).expect("CHECK-constrained operation")
    }

    pub fn request(&self) -> OperationRequest {
        serde_json::from_value(self.request.clone()).expect("commands store a canonical request")
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptRow {
    pub id: Uuid,
    pub command_id: Uuid,
    pub attempt: i32,
    pub outcome: String,
    pub transmitted: bool,
    #[serde(skip)]
    pub provider_request_ref: Option<String>,
    pub response_code: Option<String>,
    pub response: Value,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// A submitted (or replayed) command.
#[derive(Debug, Clone, PartialEq)]
pub struct Submitted {
    pub command: CommandRow,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct CommandSettings {
    /// Dispatch attempts (definite not-transmitted failures only are retried).
    pub max_attempts: i32,
    /// Delay before the first read-back after an unknown outcome.
    pub reconcile_delay: Duration,
    /// Quarantine of a released number before it can be reassigned.
    pub quarantine: Duration,
    /// Holds requested from the provider.
    pub hold: Duration,
}

impl Default for CommandSettings {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            reconcile_delay: Duration::seconds(30),
            quarantine: Duration::days(90),
            hold: Duration::minutes(30),
        }
    }
}

/// The command layer.
#[derive(Clone)]
pub struct ProviderCommands {
    pub(crate) numbers: NumberDomain,
    pub(crate) registry: Arc<ProviderRegistry>,
    pub(crate) settings: CommandSettings,
}

pub(crate) fn executor() -> Actor {
    Actor::system("provider-commands")
}

pub(crate) fn reconciler() -> Actor {
    Actor::new(ActorKind::Reconciliation, "provider-reconciler")
}

pub(crate) async fn lock_command(txn: &DatabaseTransaction, id: Uuid) -> DomainResult<CommandRow> {
    CommandRow::find_by_statement(stmt(
        "SELECT * FROM number_provider_commands WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("provider command"))
}

/// Changes a command's status (+1 revision) and emits its event.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn set_status(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    cmd: &CommandRow,
    to: CommandStatus,
    attempts: i32,
    result: Option<Value>,
    failure_code: Option<&str>,
    external_ref: Option<&str>,
    now: DateTime<Utc>,
) -> DomainResult<CommandRow> {
    let row = CommandRow::find_by_statement(stmt(
        "UPDATE number_provider_commands SET status = $3, attempts = $4, result = COALESCE($5, result), \
           failure_code = $6, external_ref = COALESCE($7, external_ref), revision = revision + 1, updated_at = $8, \
           completed_at = CASE WHEN $9 THEN $8 ELSE NULL END \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            cmd.id.into(),
            cmd.revision.into(),
            to.as_str().into(),
            attempts.into(),
            result.into(),
            failure_code.map(str::to_string).into(),
            external_ref.map(str::to_string).into(),
            now.into(),
            to.is_final().into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("provider command"))?;
    if to != CommandStatus::Dispatching && to != CommandStatus::Pending {
        command_event(txn, ctx, &row, now).await?;
    }
    crate::metrics::inc(
        "numbering_provider_commands_total",
        &[("status", to.as_str())],
    );
    Ok(row)
}

async fn command_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &CommandRow,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.provider-command.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: row.number_resource_id,
            scope: None,
            payload: json!({
                "provider_command_id": row.id,
                "number_resource_id": row.number_resource_id,
                "operation": row.operation,
                "status": row.status,
                "attempts": row.attempts,
                "failure_code": row.failure_code,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

/// Appends one attempt (evidence).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_attempt(
    txn: &DatabaseTransaction,
    command_id: Uuid,
    attempt: i32,
    outcome: &str,
    transmitted: bool,
    provider_request_ref: Option<&str>,
    response_code: Option<&str>,
    response: Value,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> DomainResult<AttemptRow> {
    Ok(AttemptRow::find_by_statement(stmt(
        "INSERT INTO number_provider_command_attempts (id, command_id, attempt, outcome, transmitted, provider_request_ref, \
           response_code, response, started_at, finished_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *",
        vec![
            Uuid::new_v4().into(),
            command_id.into(),
            attempt.into(),
            outcome.into(),
            transmitted.into(),
            provider_request_ref.map(str::to_string).into(),
            response_code.map(str::to_string).into(),
            response.into(),
            started_at.into(),
            finished_at.into(),
        ],
    ))
    .one(txn)
    .await?
    .expect("insert returns the row"))
}

fn receipt_summary(receipt: &OperationReceipt) -> Value {
    match &receipt.state {
        Some(s) => json!({
            "status": s.status.as_str(),
            "sequence": s.sequence,
            "capabilities": s.capabilities,
            "voiceRouteRef": s.voice_route,
            "smsRouteRef": s.sms_route,
        }),
        None => json!({}),
    }
}

/// What a request asks for.
struct Intent {
    provider_id: String,
    external_ref: String,
    operation: OperationKind,
    reservation_id: Option<Uuid>,
}

impl ProviderCommands {
    pub fn new(
        numbers: NumberDomain,
        registry: Arc<ProviderRegistry>,
        settings: CommandSettings,
    ) -> Self {
        Self {
            numbers,
            registry,
            settings,
        }
    }

    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    pub fn numbers(&self) -> &NumberDomain {
        &self.numbers
    }

    pub(crate) fn clock(&self) -> &SharedClock {
        self.numbers.clock()
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock().now()
    }

    // -----------------------------------------------------------------------------------
    // Inventory
    // -----------------------------------------------------------------------------------

    /// Searches a provider and records the offers as canonical inventory: one Number
    /// Resource per provider object (stable across searches), a fresh observation, the
    /// provider's classification, offered-capability evidence (`SYNTHETIC` for synthetic
    /// inventory, never a carrier confirmation) and AVAILABLE.
    pub async fn ingest_inventory(
        &self,
        ctx: &RequestContext,
        provider_id: &str,
        query: &InventoryQuery,
    ) -> DomainResult<Vec<ResourceRow>> {
        let provider = self.registry.get(provider_id).ok_or_else(|| {
            DomainError::precondition("PROVIDER_NOT_REGISTERED", "no such provider")
        })?;
        let synthetic = provider.descriptor().synthetic;
        let source = if synthetic {
            InventorySource::SyntheticTest
        } else {
            InventorySource::LiveProvider
        };
        let offers = match provider.search(query).await {
            ProviderOutcome::Succeeded(offers) => offers,
            ProviderOutcome::Unsupported => {
                return Err(DomainError::precondition(
                    "OPERATION_UNSUPPORTED",
                    "the provider does not search",
                ))
            }
            _ => {
                return Err(DomainError::precondition(
                    "PROVIDER_UNAVAILABLE",
                    "the provider search failed",
                ))
            }
        };
        let mut out = Vec::new();
        for offer in offers {
            let mut observation =
                NewObservation::offered(source, offer.observed_at, offer.fresh_for);
            observation.provider_id = if synthetic {
                None
            } else {
                Some(provider_id.to_string())
            };
            observation.availability = Availability::Available;
            observation.offered_capabilities = offer.capabilities.clone();
            observation.observed_number_type = Some(offer.number_type);
            observation.monthly_cost_minor = offer.monthly_cost_minor;
            observation.currency = offer.currency.clone();
            let existing = self
                .resource_for_ref(provider_id, &offer.external_ref)
                .await?;
            let resource = match existing {
                Some(id) => self.numbers.observe(ctx, id, observation).await?,
                None => {
                    let discovered = self
                        .numbers
                        .discover(
                            ctx,
                            executor(),
                            NewResource {
                                number: offer.e164.clone(),
                                country: offer.country.clone(),
                                inventory_source: source,
                                classification: Some((
                                    offer.number_type,
                                    if synthetic {
                                        ClassificationSource::Synthetic
                                    } else {
                                        ClassificationSource::ProviderInventory
                                    },
                                )),
                                facts: ResourceFacts {
                                    region: offer.region.clone(),
                                    restriction_flags: if offer.kyc_required {
                                        vec![RestrictionFlag::KycRequired]
                                    } else {
                                        vec![]
                                    },
                                    ..Default::default()
                                },
                            },
                            observation,
                        )
                        .await;
                    let r = match discovered {
                        Ok(r) => r,
                        Err(DomainError::Conflict {
                            code: "NUMBER_EXISTS",
                            ..
                        }) => {
                            self.numbers
                                .resource_by_number(&offer.e164, synthetic)
                                .await?
                        }
                        Err(e) => return Err(e),
                    };
                    self.numbers
                        .db()
                        .execute(stmt(
                            "INSERT INTO number_provider_inventory_refs (id, number_resource_id, provider_id, external_ref, first_seen_at) \
                             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                            vec![Uuid::new_v4().into(), r.id.into(), provider_id.into(), offer.external_ref.clone().into(), self.now().into()],
                        ))
                        .await?;
                    for c in &offer.capabilities {
                        let new = if synthetic {
                            NewCapability::declared(*c, CapabilitySource::Synthetic)
                        } else {
                            NewCapability::declared(*c, CapabilitySource::ProviderInventory)
                        };
                        self.numbers.record_capability(ctx, r.id, new).await?;
                    }
                    let r = self.numbers.resource(r.id).await?;
                    if r.lifecycle_state() == S::Discovered {
                        self.numbers
                            .transition(
                                ctx,
                                r.id,
                                TransitionRequest::new(
                                    S::Available,
                                    r.revision,
                                    executor(),
                                    "INVENTORY_OFFERED",
                                ),
                            )
                            .await?
                    } else {
                        r
                    }
                }
            };
            out.push(resource);
        }
        Ok(out)
    }

    async fn resource_for_ref(
        &self,
        provider_id: &str,
        external_ref: &str,
    ) -> DomainResult<Option<Uuid>> {
        #[derive(FromQueryResult)]
        struct Id {
            number_resource_id: Uuid,
        }
        Ok(Id::find_by_statement(stmt(
            "SELECT number_resource_id FROM number_provider_inventory_refs WHERE provider_id = $1 AND external_ref = $2",
            vec![provider_id.into(), external_ref.into()],
        ))
        .one(self.numbers.db())
        .await?
        .map(|r| r.number_resource_id))
    }

    /// The provider object serving a resource: its active binding, else its inventory ref.
    pub(crate) async fn provider_object<C: ConnectionTrait>(
        db: &C,
        resource_id: Uuid,
    ) -> DomainResult<(String, String)> {
        #[derive(FromQueryResult)]
        struct Obj {
            provider_id: String,
            external_ref: String,
        }
        let found = Obj::find_by_statement(stmt(
            "SELECT provider_id, external_ref FROM ( \
               SELECT provider_id, external_ref, 0 AS rank, binding_revision AS ord FROM number_provider_bindings \
                WHERE number_resource_id = $1 AND status = 'ACTIVE' \
               UNION ALL \
               SELECT provider_id, external_ref, 1 AS rank, 0 AS ord FROM number_provider_inventory_refs \
                WHERE number_resource_id = $1) o ORDER BY rank, ord DESC LIMIT 1",
            vec![resource_id.into()],
        ))
        .one(db)
        .await?
        .ok_or_else(|| DomainError::precondition("PROVIDER_OBJECT_UNKNOWN", "no provider object is known for this number"))?;
        Ok((found.provider_id, found.external_ref))
    }

    // -----------------------------------------------------------------------------------
    // Intent
    // -----------------------------------------------------------------------------------

    /// Replay check: an existing command with the key (same intent) or a refusal.
    async fn existing(
        &self,
        txn: &DatabaseTransaction,
        provider_id: &str,
        key: &str,
        resource_id: Uuid,
        operation: &OperationKind,
    ) -> DomainResult<Option<CommandRow>> {
        if !crate::infra::idempotency::valid_key(key) && !(8..=160).contains(&key.len()) {
            return Err(DomainError::Validation(
                "an idempotency key is 8–160 of [A-Za-z0-9._:-]".into(),
            ));
        }
        let found = CommandRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_commands WHERE provider_id = $1 AND idempotency_key = $2",
            vec![provider_id.into(), key.into()],
        ))
        .one(txn)
        .await?;
        match found {
            Some(c)
                if c.number_resource_id == resource_id && &c.request().operation == operation =>
            {
                Ok(Some(c))
            }
            Some(_) => Err(DomainError::conflict(
                "IDEMPOTENCY_KEY_REUSED",
                "the idempotency key was used for a different provider command",
            )),
            None => Ok(None),
        }
    }

    /// Writes the command and its dispatch job (inside the intent transaction).
    async fn insert(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        resource_id: Uuid,
        key: &str,
        intent: Intent,
        now: DateTime<Utc>,
    ) -> DomainResult<CommandRow> {
        let id = Uuid::new_v4();
        let request = OperationRequest {
            request_id: id,
            external_ref: intent.external_ref.clone(),
            operation: intent.operation,
        };
        let (job_id, _) = jobs::enqueue(
            txn,
            NewJob {
                kind: DISPATCH_JOB,
                dedupe_key: Some(id.to_string()),
                payload: json!({ "command_id": id }),
                retry_safety: RetrySafety::ReconcileOnUncertainty,
                max_attempts: self.settings.max_attempts,
                run_at: now,
            },
            ctx,
            now,
        )
        .await?;
        let inserted = CommandRow::find_by_statement(stmt(
            "INSERT INTO number_provider_commands (id, provider_id, operation, number_resource_id, reservation_id, \
               idempotency_key, request, status, max_attempts, job_id, external_ref, correlation_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'PENDING', $8, $9, $10, $11, $12, $12) RETURNING *",
            vec![
                id.into(),
                intent.provider_id.into(),
                request.operation.operation().as_str().into(),
                resource_id.into(),
                intent.reservation_id.into(),
                key.into(),
                serde_json::to_value(&request).expect("serializable").into(),
                self.settings.max_attempts.into(),
                job_id.into(),
                intent.external_ref.into(),
                ctx.correlation_id.as_str().into(),
                now.into(),
            ],
        ))
        .one(txn)
        .await;
        let row = match inserted {
            Ok(r) => r.expect("insert returns the row"),
            Err(e) => {
                let e: DomainError = e.into();
                return Err(match &e {
                    DomainError::Database { summary, .. }
                        if summary.contains("uq_number_provider_commands_in_flight")
                            || summary.contains("uq_number_provider_commands_config_in_flight") =>
                    {
                        DomainError::conflict(
                            "COMMAND_IN_FLIGHT",
                            "another provider command for this number is in flight",
                        )
                    }
                    _ => e,
                });
            }
        };
        command_event(txn, ctx, &row, now).await?;
        Ok(row)
    }

    /// Requests a provider hold: the PENDING reservation (RESERVING) and the RESERVE
    /// command commit together.
    pub async fn request_hold(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        resource_id: Uuid,
        ttl: Duration,
        idempotency_key: &str,
    ) -> DomainResult<(ReservationRow, Submitted)> {
        let now = self.now();
        let (provider_id, external_ref) =
            Self::provider_object(self.numbers.db(), resource_id).await?;
        let operation = OperationKind::Reserve {
            hold_seconds: ttl.num_seconds().min(self.settings.hold.num_seconds()),
        };
        let txn = self.numbers.db().begin().await?;
        if let Some(c) = self
            .existing(&txn, &provider_id, idempotency_key, resource_id, &operation)
            .await?
        {
            txn.commit().await?;
            let reservation = self
                .numbers
                .reservation(scope, c.reservation_id.expect("RESERVE has one"))
                .await?;
            return Ok((
                reservation,
                Submitted {
                    command: c,
                    replayed: true,
                },
            ));
        }
        let reservation = self
            .numbers
            .reserve_in(&txn, ctx, scope, actor, resource_id, ttl, None, now)
            .await?;
        let command = self
            .insert(
                &txn,
                ctx,
                resource_id,
                idempotency_key,
                Intent {
                    provider_id,
                    external_ref,
                    operation,
                    reservation_id: Some(reservation.id),
                },
                now,
            )
            .await?;
        txn.commit().await?;
        Ok((
            reservation,
            Submitted {
                command,
                replayed: false,
            },
        ))
    }

    /// Requests provisioning of a RESERVED number (→ PROVISIONING with the command).
    pub async fn request_provision(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        resource_id: Uuid,
        capabilities: Vec<Capability>,
        idempotency_key: &str,
    ) -> DomainResult<Submitted> {
        if capabilities.is_empty() || capabilities.iter().any(|c| !c.is_routable()) {
            return Err(DomainError::Validation(
                "provisioning names the routable capabilities to serve".into(),
            ));
        }
        self.lifecycle_command(
            ctx,
            resource_id,
            idempotency_key,
            OperationKind::Provision { capabilities },
            |row| match row.lifecycle_state() {
                S::Reserved => Ok(Some(TransitionRequest::new(
                    S::Provisioning,
                    row.revision,
                    actor.clone(),
                    "PROVISION_REQUESTED",
                ))),
                s => Err(DomainError::InvalidTransition {
                    entity: "number resource",
                    from: s.as_str().into(),
                    to: "PROVISIONING".into(),
                }),
            },
        )
        .await
    }

    /// Requests suspension of an ACTIVE number at the provider.
    pub async fn request_suspend(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        reason_code: &str,
        idempotency_key: &str,
    ) -> DomainResult<Submitted> {
        crate::numbers::types::reason_code(reason_code)?;
        self.lifecycle_command(
            ctx,
            resource_id,
            idempotency_key,
            OperationKind::Suspend {
                reason_code: reason_code.into(),
            },
            |row| expect_state(row, S::Active, "SUSPENDED").map(|_| None),
        )
        .await
    }

    /// Requests restoration of a SUSPENDED number.
    pub async fn request_restore(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        idempotency_key: &str,
    ) -> DomainResult<Submitted> {
        self.lifecycle_command(
            ctx,
            resource_id,
            idempotency_key,
            OperationKind::Restore,
            |row| expect_state(row, S::Suspended, "ACTIVE").map(|_| None),
        )
        .await
    }

    /// Requests the provider release. The assignment must be released first; the number
    /// is RELEASING (entered here, or already by the assignment's routing removal) or
    /// already QUARANTINED locally.
    pub async fn request_release(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        resource_id: Uuid,
        idempotency_key: &str,
    ) -> DomainResult<Submitted> {
        self.lifecycle_command(
            ctx,
            resource_id,
            idempotency_key,
            OperationKind::Release,
            |row| {
                Ok(match row.lifecycle_state() {
                    S::Releasing | S::Quarantined => None,
                    _ => Some(TransitionRequest::new(
                        S::Releasing,
                        row.revision,
                        actor.clone(),
                        "RELEASE_REQUESTED",
                    )),
                })
            },
        )
        .await
    }

    /// Requests porting in (optional capability: unsupported providers answer so).
    pub async fn request_port_in(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        losing_carrier_ref: &str,
        idempotency_key: &str,
    ) -> DomainResult<Submitted> {
        self.lifecycle_command(
            ctx,
            resource_id,
            idempotency_key,
            OperationKind::PortIn {
                losing_carrier_ref: losing_carrier_ref.into(),
            },
            |_| Ok(None),
        )
        .await
    }

    /// Requests voice / SMS route configuration (no lifecycle change). The serving binding
    /// must cover the capability family, else `CAPABILITY_NOT_SUPPORTED`.
    pub async fn request_route(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        voice: bool,
        route_ref: Uuid,
        idempotency_key: &str,
    ) -> DomainResult<Submitted> {
        let operation = if voice {
            OperationKind::ConfigureVoice { route_ref }
        } else {
            OperationKind::ConfigureSms { route_ref }
        };
        let family: &[Capability] = if voice {
            &[Capability::VoiceInbound, Capability::VoiceOutbound]
        } else {
            &[Capability::SmsInbound, Capability::SmsOutbound]
        };
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = lock(&txn, resource_id).await?;
        let (provider_id, external_ref) = Self::provider_object(&txn, resource_id).await?;
        if let Some(c) = self
            .existing(&txn, &provider_id, idempotency_key, resource_id, &operation)
            .await?
        {
            txn.commit().await?;
            return Ok(Submitted {
                command: c,
                replayed: true,
            });
        }
        if !matches!(
            row.lifecycle_state(),
            S::Provisioned | S::Active | S::Suspended
        ) {
            return Err(DomainError::precondition(
                "PROVISIONING_REQUIRED",
                "routes are configured on a provisioned number",
            ));
        }
        let served = self
            .numbers
            .bindings(resource_id)
            .await?
            .into_iter()
            .filter(|b| b.status == "ACTIVE")
            .any(|b| {
                b.capabilities
                    .iter()
                    .any(|c| family.iter().any(|f| f.as_str() == c))
            });
        if !served {
            return Err(DomainError::precondition(
                "CAPABILITY_NOT_SUPPORTED",
                "no active provider binding serves this capability",
            ));
        }
        let command = self
            .insert(
                &txn,
                ctx,
                resource_id,
                idempotency_key,
                Intent {
                    provider_id,
                    external_ref,
                    operation,
                    reservation_id: None,
                },
                now,
            )
            .await?;
        txn.commit().await?;
        Ok(Submitted {
            command,
            replayed: false,
        })
    }

    async fn lifecycle_command(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        idempotency_key: &str,
        operation: OperationKind,
        intent: impl FnOnce(&ResourceRow) -> DomainResult<Option<TransitionRequest>>,
    ) -> DomainResult<Submitted> {
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = lock(&txn, resource_id).await?;
        let (provider_id, external_ref) = Self::provider_object(&txn, resource_id).await?;
        if let Some(c) = self
            .existing(&txn, &provider_id, idempotency_key, resource_id, &operation)
            .await?
        {
            txn.commit().await?;
            return Ok(Submitted {
                command: c,
                replayed: true,
            });
        }
        if let Some(t) = intent(&row)? {
            transition_in(&txn, ctx, resource_id, t, now).await?;
        }
        let command = self
            .insert(
                &txn,
                ctx,
                resource_id,
                idempotency_key,
                Intent {
                    provider_id,
                    external_ref,
                    operation,
                    reservation_id: None,
                },
                now,
            )
            .await?;
        txn.commit().await?;
        Ok(Submitted {
            command,
            replayed: false,
        })
    }

    // -----------------------------------------------------------------------------------
    // Dispatch
    // -----------------------------------------------------------------------------------

    /// One dispatch attempt (the `provider.dispatch` job).
    pub async fn run_dispatch(
        &self,
        ctx: &RequestContext,
        command_id: Uuid,
    ) -> DomainResult<JobOutcome> {
        // Phase 1: DISPATCHING (committed before anything is transmitted).
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let cmd = lock_command(&txn, command_id).await?;
        match cmd.status() {
            CommandStatus::Pending => {}
            CommandStatus::Dispatching => {
                // A previous attempt was interrupted after it may have transmitted.
                self.enter_unknown(&txn, ctx, &cmd, "INTERRUPTED_DURING_DISPATCH", now)
                    .await?;
                txn.commit().await?;
                return Ok(JobOutcome::Succeeded);
            }
            _ => {
                txn.commit().await?;
                return Ok(JobOutcome::Succeeded);
            }
        }
        let resource = self.numbers.resource(cmd.number_resource_id).await?;
        let Some(provider) = self
            .registry
            .for_inventory(&cmd.provider_id, resource.inventory())
        else {
            self.apply_failure(&txn, ctx, &cmd, FailureCategory::ProviderUnavailable, now)
                .await?;
            set_status(
                &txn,
                ctx,
                &cmd,
                CommandStatus::Failed,
                cmd.attempts,
                None,
                Some("PROVIDER_NOT_REGISTERED"),
                None,
                now,
            )
            .await?;
            txn.commit().await?;
            return Ok(JobOutcome::Failed {
                code: "PROVIDER_NOT_REGISTERED",
                message: "no adapter serves this command".into(),
            });
        };
        let attempt = cmd.attempts + 1;
        let cmd = set_status(
            &txn,
            ctx,
            &cmd,
            CommandStatus::Dispatching,
            attempt,
            None,
            None,
            None,
            now,
        )
        .await?;
        txn.commit().await?;

        // Phase 2: the provider call (idempotent on the command id).
        let request = cmd.request();
        let outcome = dispatch(provider.as_ref(), &request).await;
        let finished = self.now();

        // Phase 3: evidence + canonical transition.
        let txn = self.numbers.db().begin().await?;
        let cmd = lock_command(&txn, command_id).await?;
        if cmd.status() != CommandStatus::Dispatching || cmd.attempts != attempt {
            txn.commit().await?;
            return Ok(JobOutcome::Succeeded);
        }
        let label = outcome.label();
        let result = match outcome {
            ProviderOutcome::Succeeded(receipt) => {
                let evidence = insert_attempt(
                    &txn,
                    cmd.id,
                    attempt,
                    label,
                    true,
                    Some(&receipt.provider_request_ref),
                    None,
                    receipt_summary(&receipt),
                    now,
                    finished,
                )
                .await?;
                self.complete_success(
                    &txn,
                    ctx,
                    &cmd,
                    &receipt,
                    evidence.id,
                    CommandStatus::Succeeded,
                    finished,
                )
                .await?;
                JobOutcome::Succeeded
            }
            ProviderOutcome::Failed(f) if !f.transmitted && attempt < cmd.max_attempts => {
                insert_attempt(
                    &txn,
                    cmd.id,
                    attempt,
                    label,
                    false,
                    None,
                    Some(&f.code),
                    json!({}),
                    now,
                    finished,
                )
                .await?;
                set_status(
                    &txn,
                    ctx,
                    &cmd,
                    CommandStatus::Pending,
                    attempt,
                    None,
                    None,
                    None,
                    finished,
                )
                .await?;
                txn.commit().await?;
                return Ok(JobOutcome::Retry {
                    code: "PROVIDER_NOT_REACHED",
                    message: f.code,
                });
            }
            ProviderOutcome::Failed(f) => {
                insert_attempt(
                    &txn,
                    cmd.id,
                    attempt,
                    label,
                    f.transmitted,
                    None,
                    Some(&f.code),
                    json!({}),
                    now,
                    finished,
                )
                .await?;
                self.complete_failure(&txn, ctx, &cmd, &f, CommandStatus::Failed, finished)
                    .await?;
                JobOutcome::Succeeded
            }
            ProviderOutcome::Unsupported => {
                insert_attempt(
                    &txn,
                    cmd.id,
                    attempt,
                    label,
                    true,
                    None,
                    Some("OPERATION_UNSUPPORTED"),
                    json!({}),
                    now,
                    finished,
                )
                .await?;
                self.apply_failure(&txn, ctx, &cmd, FailureCategory::ProviderRejected, finished)
                    .await?;
                set_status(
                    &txn,
                    ctx,
                    &cmd,
                    CommandStatus::Unsupported,
                    attempt,
                    None,
                    Some("OPERATION_UNSUPPORTED"),
                    None,
                    finished,
                )
                .await?;
                JobOutcome::Succeeded
            }
            ProviderOutcome::Unknown { .. } => {
                insert_attempt(
                    &txn,
                    cmd.id,
                    attempt,
                    label,
                    true,
                    None,
                    Some("OUTCOME_UNKNOWN"),
                    json!({}),
                    now,
                    finished,
                )
                .await?;
                self.enter_unknown(&txn, ctx, &cmd, "PROVIDER_OUTCOME_UNKNOWN", finished)
                    .await?;
                JobOutcome::Succeeded
            }
        };
        txn.commit().await?;
        if label == "SUCCEEDED" {
            self.after_success(ctx, command_id).await;
        }
        Ok(result)
    }

    /// UNKNOWN: local uncertainty where the lifecycle models it, an incident and a
    /// scheduled read-back.
    pub(crate) async fn enter_unknown(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        cmd: &CommandRow,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<CommandRow> {
        let uncertain = match cmd.operation() {
            CommandOperation::Reserve => {
                if let Some(rid) = cmd.reservation_id {
                    let current = ReservationRow::find_by_statement(stmt(
                        "SELECT * FROM number_reservations WHERE id = $1",
                        vec![rid.into()],
                    ))
                    .one(txn)
                    .await?;
                    if current.is_some_and(|r| r.status == "PENDING") {
                        self.numbers
                            .mark_reservation_unknown_in(txn, ctx, executor(), rid, now)
                            .await?;
                    }
                }
                None
            }
            CommandOperation::Provision => Some((S::Provisioning, UncertainOperation::Provision)),
            CommandOperation::Release => Some((S::Releasing, UncertainOperation::Release)),
            _ => None,
        };
        if let Some((from, op)) = uncertain {
            let row = lock(txn, cmd.number_resource_id).await?;
            if row.lifecycle_state() == from {
                transition_in(
                    txn,
                    ctx,
                    row.id,
                    TransitionRequest::new(
                        S::UnknownProviderState,
                        row.revision,
                        executor(),
                        "PROVIDER_OUTCOME_UNKNOWN",
                    )
                    .uncertain(op),
                    now,
                )
                .await?;
            }
        }
        jobs::enqueue(
            txn,
            NewJob {
                kind: RECONCILE_JOB,
                dedupe_key: Some(format!("reconcile:{}", cmd.id)),
                payload: json!({ "command_id": cmd.id }),
                retry_safety: RetrySafety::Idempotent,
                max_attempts: 20,
                run_at: now + self.settings.reconcile_delay,
            },
            ctx,
            now,
        )
        .await?;
        incidents::raise(
            txn,
            incidents::Category::UnknownProviderOutcome,
            &format!("command:{}", cmd.id),
            incidents::Subject {
                provider_id: Some(cmd.provider_id.clone()),
                number_resource_id: Some(cmd.number_resource_id),
                command_id: Some(cmd.id),
            },
            json!({ "operation": cmd.operation, "reason": reason }),
            now,
        )
        .await?;
        let cmd = lock_command(txn, cmd.id).await?;
        set_status(
            txn,
            ctx,
            &cmd,
            CommandStatus::Unknown,
            cmd.attempts,
            None,
            None,
            None,
            now,
        )
        .await
    }

    /// Applies a definite (or reconciled) success; a local refusal (state moved on) is a
    /// conflict incident, never a silent overwrite.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn complete_success(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        cmd: &CommandRow,
        receipt: &OperationReceipt,
        evidence: Uuid,
        status: CommandStatus,
        now: DateTime<Utc>,
    ) -> DomainResult<CommandRow> {
        let actor = if status == CommandStatus::ReconciledSucceeded {
            reconciler()
        } else {
            executor()
        };
        let savepoint = txn.begin().await?;
        let applied = self
            .apply_success(&savepoint, ctx, cmd, receipt, evidence, actor, now)
            .await;
        let mut result =
            json!({ "providerState": receipt_summary(receipt), "evidenceRef": evidence });
        match applied {
            Ok(()) => savepoint.commit().await?,
            Err(e) => {
                savepoint.rollback().await?;
                result["localApplied"] = json!(false);
                result["localRefusal"] = json!(e.code());
                incidents::raise(
                    txn,
                    incidents::Category::ReconciliationConflict,
                    &format!("command-conflict:{}", cmd.id),
                    incidents::Subject {
                        provider_id: Some(cmd.provider_id.clone()),
                        number_resource_id: Some(cmd.number_resource_id),
                        command_id: Some(cmd.id),
                    },
                    json!({ "operation": cmd.operation, "refusal": e.code() }),
                    now,
                )
                .await?;
            }
        }
        let cmd = lock_command(txn, cmd.id).await?;
        set_status(
            txn,
            ctx,
            &cmd,
            status,
            cmd.attempts,
            Some(result),
            None,
            Some(&receipt.external_ref),
            now,
        )
        .await
    }

    /// Applies a definite (or reconciled) failure: the intent is reverted.
    pub(crate) async fn complete_failure(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        cmd: &CommandRow,
        failure: &ProviderFailure,
        status: CommandStatus,
        now: DateTime<Utc>,
    ) -> DomainResult<CommandRow> {
        self.apply_failure(txn, ctx, cmd, failure.category, now)
            .await?;
        let cmd = lock_command(txn, cmd.id).await?;
        set_status(
            txn,
            ctx,
            &cmd,
            status,
            cmd.attempts,
            None,
            Some(&failure.code),
            None,
            now,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_success(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        cmd: &CommandRow,
        receipt: &OperationReceipt,
        evidence: Uuid,
        actor: Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let id = cmd.number_resource_id;
        let reconciling = actor.kind == ActorKind::Reconciliation;
        match cmd.operation() {
            CommandOperation::Reserve => {
                let rid = cmd.reservation_id.expect("RESERVE has a reservation");
                if reconciling {
                    self.numbers
                        .resolve_unknown_reservation_in(txn, ctx, rid, true, evidence, now)
                        .await?;
                    let row = lock(txn, id).await?;
                    transition_in(
                        txn,
                        ctx,
                        id,
                        TransitionRequest::new(
                            S::Reserved,
                            row.revision,
                            actor,
                            "RECONCILED_HOLD_CONFIRMED",
                        )
                        .with_evidence(evidence),
                        now,
                    )
                    .await?;
                } else {
                    self.numbers
                        .confirm_hold_in(txn, ctx, actor, rid, evidence, now)
                        .await?;
                }
            }
            CommandOperation::Provision => {
                let OperationKind::Provision { capabilities } = cmd.request().operation else {
                    unreachable!()
                };
                let served: Vec<Capability> = receipt
                    .state
                    .as_ref()
                    .map(|s| {
                        s.capabilities
                            .iter()
                            .copied()
                            .filter(|c| c.is_routable() && capabilities.contains(c))
                            .collect()
                    })
                    .unwrap_or(capabilities);
                if served.is_empty() {
                    return Err(DomainError::precondition(
                        "BINDING_REQUIRED",
                        "the provider serves none of the capabilities",
                    ));
                }
                self.numbers
                    .activate_binding_in(
                        txn,
                        ctx,
                        id,
                        NewBinding {
                            provider_id: cmd.provider_id.clone(),
                            external_ref: receipt.external_ref.clone(),
                            capabilities: served,
                            change_reason: BindingChangeReason::Initial,
                        },
                        now,
                    )
                    .await?;
                let row = lock(txn, id).await?;
                transition_in(
                    txn,
                    ctx,
                    id,
                    TransitionRequest::new(
                        S::Provisioned,
                        row.revision,
                        actor,
                        if reconciling {
                            "RECONCILED_PROVISIONED"
                        } else {
                            "PROVISIONED"
                        },
                    )
                    .with_evidence(evidence),
                    now,
                )
                .await?;
            }
            CommandOperation::Suspend => {
                let OperationKind::Suspend { reason_code } = cmd.request().operation else {
                    unreachable!()
                };
                let row = lock(txn, id).await?;
                if row.lifecycle_state() == S::Active {
                    transition_in(
                        txn,
                        ctx,
                        id,
                        TransitionRequest::new(
                            S::Suspended,
                            row.revision,
                            actor,
                            "PROVIDER_SUSPENDED",
                        )
                        .with_suspension(&reason_code)
                        .with_evidence(evidence),
                        now,
                    )
                    .await?;
                }
            }
            CommandOperation::Restore => {
                let row = lock(txn, id).await?;
                if row.lifecycle_state() == S::Suspended {
                    transition_in(
                        txn,
                        ctx,
                        id,
                        TransitionRequest::new(S::Active, row.revision, actor, "PROVIDER_RESTORED")
                            .with_evidence(evidence),
                        now,
                    )
                    .await?;
                }
            }
            CommandOperation::Release => {
                for b in self
                    .numbers
                    .bindings(id)
                    .await?
                    .into_iter()
                    .filter(|b| b.status == "ACTIVE")
                {
                    self.numbers.end_binding_in(txn, ctx, b.id, now).await?;
                }
                let row = lock(txn, id).await?;
                if row.lifecycle_state() == S::Quarantined {
                    // Already quarantined locally (assignment path): the provider release
                    // is the missing evidence, nothing else changes.
                    return Ok(());
                }
                transition_in(
                    txn,
                    ctx,
                    id,
                    TransitionRequest::new(
                        S::Quarantined,
                        row.revision,
                        actor,
                        if reconciling {
                            "RECONCILED_RELEASED"
                        } else {
                            "PROVIDER_RELEASED"
                        },
                    )
                    .with_evidence(evidence)
                    .with_quarantine_until(now + self.settings.quarantine),
                    now,
                )
                .await?;
            }
            CommandOperation::ConfigureVoice
            | CommandOperation::ConfigureSms
            | CommandOperation::PortIn => {}
        }
        Ok(())
    }

    async fn apply_failure(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        cmd: &CommandRow,
        category: FailureCategory,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let id = cmd.number_resource_id;
        let row = lock(txn, id).await?;
        let actor = if row.lifecycle_state() == S::UnknownProviderState {
            reconciler()
        } else {
            executor()
        };
        match cmd.operation() {
            CommandOperation::Reserve => {
                let rid = cmd.reservation_id.expect("RESERVE has a reservation");
                let current = ReservationRow::find_by_statement(stmt(
                    "SELECT * FROM number_reservations WHERE id = $1",
                    vec![rid.into()],
                ))
                .one(txn)
                .await?
                .ok_or(DomainError::NotFound("reservation"))?;
                match current.status.as_str() {
                    "PENDING" => {
                        self.numbers
                            .fail_reservation_in(txn, ctx, executor(), rid, now)
                            .await?;
                    }
                    "UNKNOWN" => {
                        // Reconciliation: no hold exists.
                        let evidence = Uuid::new_v4();
                        self.numbers
                            .resolve_unknown_reservation_in(txn, ctx, rid, false, evidence, now)
                            .await?;
                        let to = if category == FailureCategory::NotObtainable {
                            S::Failed
                        } else {
                            S::Available
                        };
                        let mut t = TransitionRequest::new(
                            to,
                            row.revision,
                            reconciler(),
                            "RECONCILED_HOLD_ABSENT",
                        )
                        .with_evidence(evidence);
                        if to == S::Failed {
                            t = t.with_failure(FailureCategory::ReconciledFailed);
                        }
                        transition_in(txn, ctx, id, t, now).await?;
                    }
                    _ => {}
                }
            }
            CommandOperation::Provision => {
                if matches!(
                    row.lifecycle_state(),
                    S::Provisioning | S::UnknownProviderState
                ) {
                    let mut t = TransitionRequest::new(
                        S::Reserved,
                        row.revision,
                        actor,
                        "PROVISION_REFUSED",
                    );
                    if row.lifecycle_state() == S::UnknownProviderState {
                        t = t.with_evidence(Uuid::new_v4());
                    }
                    transition_in(txn, ctx, id, t, now).await?;
                }
            }
            CommandOperation::Release => match row.lifecycle_state() {
                S::Releasing => {
                    transition_in(
                        txn,
                        ctx,
                        id,
                        TransitionRequest::new(S::Failed, row.revision, actor, "RELEASE_REFUSED")
                            .with_failure(category),
                        now,
                    )
                    .await?;
                }
                S::UnknownProviderState => {
                    // Still ours at the provider: back to PROVISIONED (release may be retried).
                    transition_in(
                        txn,
                        ctx,
                        id,
                        TransitionRequest::new(
                            S::Provisioned,
                            row.revision,
                            reconciler(),
                            "RECONCILED_NOT_RELEASED",
                        )
                        .with_evidence(Uuid::new_v4()),
                        now,
                    )
                    .await?;
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    /// Post-commit: capability evidence for what the provider now serves (best effort; a
    /// failure leaves the binding authoritative and is logged).
    async fn after_success(&self, ctx: &RequestContext, command_id: Uuid) {
        let Ok(Some(cmd)) = CommandRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_commands WHERE id = $1",
            vec![command_id.into()],
        ))
        .one(self.numbers.db())
        .await
        else {
            return;
        };
        if cmd.operation() != CommandOperation::Provision {
            return;
        }
        let Ok(resource) = self.numbers.resource(cmd.number_resource_id).await else {
            return;
        };
        let Ok(bindings) = self.numbers.bindings(resource.id).await else {
            return;
        };
        let synthetic = resource.inventory() == InventorySource::SyntheticTest;
        for b in bindings.into_iter().filter(|b| b.status == "ACTIVE") {
            for c in b.capabilities.iter().filter_map(|c| Capability::parse(c)) {
                let mut new = if synthetic {
                    NewCapability::declared(c, CapabilitySource::Synthetic)
                } else {
                    NewCapability::verified(
                        c,
                        CapabilitySource::ProviderConfirmation,
                        command_id,
                        self.now(),
                    )
                };
                if !synthetic {
                    new.binding_id = Some(b.id);
                }
                if let Err(e) = self.numbers.record_capability(ctx, resource.id, new).await {
                    tracing::warn!(error = %e.code(), "capability evidence after provisioning not recorded");
                }
            }
        }
    }

    /// Commands DISPATCHING whose job was parked UNKNOWN_OUTCOME (process interruption)
    /// become UNKNOWN and are reconciled.
    pub async fn recover_interrupted(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<usize> {
        #[derive(FromQueryResult)]
        struct Id {
            id: Uuid,
        }
        let ids = Id::find_by_statement(stmt(
            "SELECT c.id FROM number_provider_commands c JOIN numbering_jobs j ON j.id = c.job_id \
              WHERE c.status = 'DISPATCHING' AND j.status = 'UNKNOWN_OUTCOME' ORDER BY c.updated_at LIMIT $1",
            vec![(limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for i in ids {
            let now = self.now();
            let txn = self.numbers.db().begin().await?;
            let cmd = lock_command(&txn, i.id).await?;
            if cmd.status() == CommandStatus::Dispatching {
                self.enter_unknown(&txn, ctx, &cmd, "PROCESS_INTERRUPTED", now)
                    .await?;
                n += 1;
            }
            txn.commit().await?;
        }
        Ok(n)
    }

    /// The provider object currently serving a resource (internal).
    pub async fn provider_object_of(&self, resource_id: Uuid) -> DomainResult<(String, String)> {
        Self::provider_object(self.numbers.db(), resource_id).await
    }

    pub async fn command(&self, id: Uuid) -> DomainResult<CommandRow> {
        CommandRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_commands WHERE id = $1",
            vec![id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or(DomainError::NotFound("provider command"))
    }

    pub async fn attempts(&self, command_id: Uuid) -> DomainResult<Vec<AttemptRow>> {
        Ok(AttemptRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_command_attempts WHERE command_id = $1 ORDER BY attempt, started_at",
            vec![command_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }

    pub async fn commands_for(&self, resource_id: Uuid) -> DomainResult<Vec<CommandRow>> {
        Ok(CommandRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_commands WHERE number_resource_id = $1 ORDER BY created_at, id",
            vec![resource_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }

    /// Whether `provider_id` is the synthetic test provider.
    pub fn is_synthetic(provider_id: &str) -> bool {
        provider_id == SYNTHETIC_PROVIDER
    }
}

fn expect_state(
    row: &ResourceRow,
    state: crate::numbers::LifecycleState,
    to: &str,
) -> DomainResult<()> {
    if row.lifecycle_state() == state {
        Ok(())
    } else {
        Err(DomainError::InvalidTransition {
            entity: "number resource",
            from: row.state.clone(),
            to: to.into(),
        })
    }
}

/// The `provider.dispatch` job handler.
pub struct DispatchHandler {
    pub commands: ProviderCommands,
}

fn command_id(job: &ClaimedJob) -> Option<Uuid> {
    job.payload
        .get("command_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

#[async_trait::async_trait]
impl JobHandler for DispatchHandler {
    fn kind(&self) -> &'static str {
        DISPATCH_JOB
    }

    async fn run(&self, job: &ClaimedJob) -> JobOutcome {
        let Some(id) = command_id(job) else {
            return JobOutcome::Failed {
                code: "MALFORMED_JOB",
                message: "no command id".into(),
            };
        };
        match self.commands.run_dispatch(&job.context(), id).await {
            Ok(outcome) => outcome,
            // A database failure after DISPATCHING was committed may follow a transmission:
            // never retried blindly (the runner parks it; recovery reconciles).
            Err(DomainError::Database { summary, .. }) => JobOutcome::Unknown {
                code: "DATABASE",
                message: summary,
            },
            Err(e) => JobOutcome::Failed {
                code: "DISPATCH_REFUSED",
                message: e.code().to_string(),
            },
        }
    }
}

/// The `provider.reconcile` job handler.
pub struct ReconcileHandler {
    pub commands: ProviderCommands,
}

#[async_trait::async_trait]
impl JobHandler for ReconcileHandler {
    fn kind(&self) -> &'static str {
        RECONCILE_JOB
    }

    async fn run(&self, job: &ClaimedJob) -> JobOutcome {
        let Some(id) = command_id(job) else {
            return JobOutcome::Failed {
                code: "MALFORMED_JOB",
                message: "no command id".into(),
            };
        };
        match self.commands.reconcile_command(&job.context(), id).await {
            Ok(super::reconcile::Reconciled::Pending) => JobOutcome::Retry {
                code: "READ_BACK_INCONCLUSIVE",
                message: "authoritative provider evidence not yet available".into(),
            },
            Ok(_) => JobOutcome::Succeeded,
            Err(DomainError::Database { summary, .. }) => JobOutcome::Retry {
                code: "DATABASE",
                message: summary,
            },
            Err(e) => JobOutcome::Failed {
                code: "RECONCILE_REFUSED",
                message: e.code().to_string(),
            },
        }
    }
}

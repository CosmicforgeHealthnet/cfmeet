//! ADR 0068 A6 — pricing and plans (design §8 / §6.11 / §6.12), administered ONLY by
//! the HQ admin bridge (`pricing.administer`, bridge-only; every route needs the bridge's
//! `hq:<staff>` attribution).
//!
//! * **Price books** (one lineage per product line, currency and market) and their
//!   **versions**: DRAFT → SUBMITTED → APPROVED | REJECTED → SUPERSEDED. Rates are frozen
//!   once a version leaves DRAFT (database trigger) and their digest is kept at
//!   submission; approval needs a second person (the approving staff member is never the
//!   submitter, and HQ's own approver is a third check) and expires after 12 hours.
//! * **Activations** (append-only): scheduled (`effective_from` in the future), activated
//!   (now) or rolled back to an older approved version. The effective version of a
//!   lineage is its latest activation already effective.
//! * **Market freezes**: a frozen market refuses submit, approve, schedule, activate and
//!   rollback; unfreezing is a two-person decision like freezing.
//! * **CF Meet user plans** (`CONSUMER` lineages: Free / Pro / Business to start, HQ may
//!   rename, re-price or add tiers), **plan assignments** of a user or a business
//!   organization (opaque ids; none: Free) with their history, and **overrides** of a
//!   holder's limits.
//! * **Charge simulation** (a read: no side effect) and the **finance reports** (usage,
//!   estimated charges, plan spread). Figures are in minor units of one currency, never
//!   converted, never called revenue.
//!
//! Nothing here charges anyone: there is no card, invoice, tax or balance until a
//! payment provider is decided. Enforcement of plan limits belongs to the Meeting Service
//! and Messaging (they will read the holder's effective plan); this module only owns the
//! data.
//!
//! Every command follows the bridge command core ([`crate::control::bridge_command`]):
//! `Idempotency-Key`, `expectedVersion` locked `FOR UPDATE`, HQ's approver for every
//! pricing command (R3 / R4), the record in the same transaction, the standard answer.

pub mod rates;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    bridge_events::{self, BridgeEvents, Source},
    control::{
        bridge_command::{self as bc, BridgeCommand, Subject},
        operator::{audit_command, valid_reason},
        stmt,
    },
    domain::RequestContext,
    http::error::{ApiError, ErrorCode},
};

/// How long an approval stays usable (design §8: 12 hours).
pub const APPROVAL_TTL_HOURS: i64 = 12;
/// How far ahead an activation may be scheduled.
pub const MAX_SCHEDULE_DAYS: i64 = 366;

fn invalid() -> ApiError {
    ApiError(ErrorCode::ValidationError)
}

fn not_found() -> ApiError {
    ApiError(ErrorCode::NotFound)
}

/// A market code: `^[A-Z][A-Z0-9_]{1,15}$`.
pub fn market_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=16).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

fn reason(code: Option<String>, default: &str) -> Result<String, ApiError> {
    let r = code.unwrap_or_else(|| default.to_string());
    valid_reason(&r).then_some(r).ok_or_else(invalid)
}

// ---------------------------------------------------------------------------------------
// Price books and versions
// ---------------------------------------------------------------------------------------

struct Book {
    id: Uuid,
    product_line: String,
    market_code: String,
}

async fn book<C: ConnectionTrait>(db: &C, id: Uuid, lock: bool) -> Result<Book, ApiError> {
    let r = db
        .query_one(stmt(
            &format!(
                "SELECT id, product_line, market_code FROM devplat_price_books WHERE id = $1{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            vec![id.into()],
        ))
        .await?
        .ok_or_else(not_found)?;
    Ok(Book {
        id: r.try_get("", "id")?,
        product_line: r.try_get("", "product_line")?,
        market_code: r.try_get("", "market_code")?,
    })
}

struct Version {
    id: Uuid,
    book_id: Uuid,
    state: String,
    rates: Value,
    rates_digest: String,
    submitted_digest: Option<String>,
    submitted_by: Option<String>,
    approval_expires_at: Option<DateTime<Utc>>,
}

async fn version<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Version, ApiError> {
    let r = db
        .query_one(stmt(
            "SELECT id, price_book_id, state, rates, rates_digest, submitted_digest, submitted_by, \
                    approval_expires_at FROM devplat_price_versions WHERE id = $1",
            vec![id.into()],
        ))
        .await?
        .ok_or_else(not_found)?;
    Ok(Version {
        id: r.try_get("", "id")?,
        book_id: r.try_get("", "price_book_id")?,
        state: r.try_get("", "state")?,
        rates: r.try_get("", "rates")?,
        rates_digest: r.try_get("", "rates_digest")?,
        submitted_digest: r.try_get("", "submitted_digest")?,
        submitted_by: r.try_get("", "submitted_by")?,
        approval_expires_at: r.try_get("", "approval_expires_at")?,
    })
}

/// The effective version of a lineage at `at`: its latest activation already effective.
pub async fn effective_version<C: ConnectionTrait>(
    db: &C,
    book_id: Uuid,
    at: DateTime<Utc>,
) -> Result<Option<Uuid>, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT version_id FROM devplat_price_activations \
             WHERE price_book_id = $1 AND effective_from <= $2 \
             ORDER BY effective_from DESC, seq DESC LIMIT 1",
            vec![book_id.into(), at.into()],
        ))
        .await?
        .map(|r| r.try_get::<Uuid>("", "version_id"))
        .transpose()?)
}

async fn frozen<C: ConnectionTrait>(db: &C, market: &str) -> Result<bool, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT 1 AS x FROM devplat_market_freezes WHERE market_code = $1 AND state = 'FROZEN'",
            vec![market.into()],
        ))
        .await?
        .is_some())
}

async fn refuse_if_frozen<C: ConnectionTrait>(db: &C, market: &str) -> Result<(), ApiError> {
    if frozen(db, market).await? {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    Ok(())
}

const VERSION_SUMMARY: &str = "jsonb_build_object( \
    'versionId', v.id, 'priceBookId', v.price_book_id, 'seq', v.seq, 'state', v.state, \
    'ratesDigest', v.rates_digest, 'submittedAt', v.submitted_at, 'approvedAt', v.approved_at, \
    'approvalExpiresAt', v.approval_expires_at, 'rejectedReason', v.rejected_reason, \
    'decidedAt', v.decided_at, 'createdAt', v.created_at, 'updatedAt', v.updated_at, \
    'version', v.version)";

/// A version as the command answer shows it (no rates).
async fn version_resource<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<(Value, i64), ApiError> {
    let r = db
        .query_one(stmt(
            &format!(
                "SELECT {VERSION_SUMMARY} AS v, v.version AS n FROM devplat_price_versions v WHERE v.id = $1"
            ),
            vec![id.into()],
        ))
        .await?
        .ok_or_else(not_found)?;
    Ok((r.try_get("", "v")?, r.try_get("", "n")?))
}

/// `GET /operator/pricing/versions/{versionId}`: the version with its rates, who drafted,
/// submitted and approved it (HQ staff references) and whether it is effective.
pub async fn read_version(db: &DatabaseConnection, id: Uuid) -> Result<Value, ApiError> {
    let r = db
        .query_one(stmt(
            &format!(
                "SELECT {VERSION_SUMMARY} AS v, v.rates, v.price_book_id, v.drafted_by, v.submitted_by, \
                        v.approved_by, v.approver_ref, v.submitted_digest \
                 FROM devplat_price_versions v WHERE v.id = $1"
            ),
            vec![id.into()],
        ))
        .await?
        .ok_or_else(not_found)?;
    let mut v: Value = r.try_get("", "v")?;
    let book_id: Uuid = r.try_get("", "price_book_id")?;
    let b = book(db, book_id, false).await?;
    v["productLine"] = json!(b.product_line);
    v["marketCode"] = json!(b.market_code);
    v["rates"] = r.try_get::<Value>("", "rates")?;
    v["submittedDigest"] = json!(r.try_get::<Option<String>>("", "submitted_digest")?);
    v["draftedBy"] = json!(r.try_get::<String>("", "drafted_by")?);
    v["submittedBy"] = json!(r.try_get::<Option<String>>("", "submitted_by")?);
    v["approvedBy"] = json!(r.try_get::<Option<String>>("", "approved_by")?);
    v["approverRef"] = json!(r.try_get::<Option<String>>("", "approver_ref")?);
    v["effective"] = json!(effective_version(db, book_id, Utc::now()).await? == Some(id));
    Ok(v)
}

/// The result of a version command, from a replay or the transaction.
fn replayed(prior: Option<(String, Option<i64>)>, resource: Value) -> Option<Value> {
    prior.map(|(st, v)| bc::answer(&st, v, resource))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftBody {
    pub expected_version: Option<i64>,
    /// The content; omitted: a copy of the lineage's effective (else latest) version.
    #[serde(default)]
    pub rates: Option<Value>,
}

/// `pricing.version.draft` (R3): a new DRAFT version of a lineage, at the lineage's
/// version. Its id is the command's Idempotency-Key.
pub async fn draft(
    db: &DatabaseConnection,
    ev: BridgeEvents,
    request: &RequestContext,
    book_id: Uuid,
    body: DraftBody,
    cmd: &BridgeCommand,
) -> Result<Value, ApiError> {
    const COMMAND: &str = "pricing.version.draft";
    let b = book(db, book_id, false).await?;
    let id = cmd.key;
    if let Some((st, v)) = bc::prior(db, cmd, COMMAND, Subject::PriceVersion, id).await? {
        return Ok(bc::answer(&st, v, version_resource(db, id).await?.0));
    }
    let txn = db.begin().await?;
    bc::lock_version(&txn, cmd, Subject::PriceBook, book_id).await?;
    let content = match body.rates {
        Some(r) => r,
        None => {
            let base = match effective_version(&txn, book_id, Utc::now()).await? {
                Some(v) => Some(v),
                None => txn
                    .query_one(stmt(
                        "SELECT id FROM devplat_price_versions WHERE price_book_id = $1 ORDER BY seq DESC LIMIT 1",
                        vec![book_id.into()],
                    ))
                    .await?
                    .map(|r| r.try_get::<Uuid>("", "id"))
                    .transpose()?,
            };
            match base {
                Some(v) => version(&txn, v).await?.rates,
                None => rates::empty(&b.product_line),
            }
        }
    };
    rates::validate(&b.product_line, &content, false)?;
    let now = Utc::now();
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_price_versions (id, price_book_id, seq, state, rates, rates_digest, drafted_by, \
               created_at, updated_at) \
             SELECT $1, $2, COALESCE(MAX(seq), 0) + 1, 'DRAFT', $3, $4, $5, $6, $6 \
               FROM devplat_price_versions WHERE price_book_id = $2 \
             ON CONFLICT (id) DO NOTHING",
            vec![
                id.into(),
                book_id.into(),
                content.clone().into(),
                rates::digest(&content).into(),
                cmd.operator.clone().into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted != 1 {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    bridge_events::record(&txn, ev, Source::PriceVersion(id)).await?;
    bump_book(&txn, book_id, now).await?;
    finish(
        &txn,
        request,
        cmd,
        COMMAND,
        Subject::PriceVersion,
        id,
        "DRAFT",
        "PRICE_VERSION",
    )
    .await?;
    let (resource, n) = version_resource(&txn, id).await?;
    txn.commit().await?;
    Ok(bc::answer("DRAFT", Some(n), resource))
}

async fn bump_book<C: ConnectionTrait>(
    txn: &C,
    book_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    txn.execute(stmt(
        "UPDATE devplat_price_books SET version = version + 1, updated_at = $2 WHERE id = $1",
        vec![book_id.into(), now.into()],
    ))
    .await?;
    Ok(())
}

/// The audit row (platform-level operator audit) and the bridge record of a pricing
/// command, in its transaction.
#[allow(clippy::too_many_arguments)]
async fn finish<C: ConnectionTrait>(
    txn: &C,
    request: &RequestContext,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
    state: &str,
    target_type: &str,
) -> Result<(), ApiError> {
    let version = match subject {
        Subject::PriceBook => Some(row_version(txn, "devplat_price_books", subject_id).await?),
        Subject::PriceVersion => {
            Some(row_version(txn, "devplat_price_versions", subject_id).await?)
        }
        Subject::Market => Some(row_version(txn, "devplat_market_freezes", subject_id).await?),
        Subject::PlanHolder => Some(row_version(txn, "devplat_plan_holders", subject_id).await?),
        Subject::PlanOverride => {
            Some(row_version(txn, "devplat_plan_overrides", subject_id).await?)
        }
        _ => None,
    };
    audit_command(
        txn,
        &cmd.operator,
        request,
        &command.replace('-', "_"),
        target_type,
        &subject_id.to_string(),
        json!({ "state": state, "approverRef": cmd.approval.as_ref().map(|a| a.0.clone()) }),
    )
    .await?;
    bc::record_result(
        txn,
        cmd,
        command,
        subject,
        subject_id,
        Uuid::nil(),
        state,
        version,
    )
    .await
}

async fn row_version<C: ConnectionTrait>(db: &C, table: &str, id: Uuid) -> Result<i64, ApiError> {
    Ok(db
        .query_one(stmt(
            &format!("SELECT version FROM {table} WHERE id = $1"),
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?
        .try_get("", "version")?)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditBody {
    pub expected_version: Option<i64>,
    pub rates: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionBody {
    pub expected_version: Option<i64>,
    #[serde(default)]
    pub reason_code: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleBody {
    pub expected_version: Option<i64>,
    pub effective_from: DateTime<Utc>,
}

/// A command on one version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCommand {
    Edit(Value),
    Submit,
    Approve,
    Reject(String),
    Schedule(DateTime<Utc>),
    Activate,
}

impl VersionCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            VersionCommand::Edit(_) => "pricing.version.edit",
            VersionCommand::Submit => "pricing.version.submit",
            VersionCommand::Approve => "pricing.version.approve",
            VersionCommand::Reject(_) => "pricing.version.reject",
            VersionCommand::Schedule(_) => "pricing.version.schedule",
            VersionCommand::Activate => "pricing.version.activate",
        }
    }

    /// Every pricing command carries HQ's approver (R3 / R4); approve, reject, schedule
    /// and activate are the R4 decisions.
    pub const fn two_person(&self) -> bool {
        true
    }
}

/// Runs a version command at the version's row version.
pub async fn version_command(
    db: &DatabaseConnection,
    ev: BridgeEvents,
    request: &RequestContext,
    id: Uuid,
    command: VersionCommand,
    cmd: &BridgeCommand,
) -> Result<Value, ApiError> {
    let name = command.name();
    let current = version(db, id).await?;
    if let Some(v) = replayed(
        bc::prior(db, cmd, name, Subject::PriceVersion, id).await?,
        version_resource(db, id).await?.0,
    ) {
        return Ok(v);
    }
    let b = book(db, current.book_id, false).await?;
    let now = Utc::now();
    let txn = db.begin().await?;
    // Serializes every change of a lineage (activations above all).
    book(&txn, b.id, true).await?;
    bc::lock_version(&txn, cmd, Subject::PriceVersion, id).await?;
    let v = version(&txn, id).await?;
    let state_is = |s: &[&str]| -> Result<(), ApiError> {
        if s.contains(&v.state.as_str()) {
            Ok(())
        } else {
            Err(ApiError(ErrorCode::InvalidState))
        }
    };
    let result: &str = match &command {
        VersionCommand::Edit(content) => {
            state_is(&["DRAFT"])?;
            rates::validate(&b.product_line, content, false)?;
            txn.execute(stmt(
                "UPDATE devplat_price_versions SET rates = $2, rates_digest = $3, updated_at = $4, \
                     version = version + 1 WHERE id = $1",
                vec![
                    id.into(),
                    content.clone().into(),
                    rates::digest(content).into(),
                    now.into(),
                ],
            ))
            .await?;
            "DRAFT"
        }
        VersionCommand::Submit => {
            state_is(&["DRAFT"])?;
            refuse_if_frozen(&txn, &b.market_code).await?;
            rates::validate(&b.product_line, &v.rates, true)?;
            txn.execute(stmt(
                "UPDATE devplat_price_versions SET state = 'SUBMITTED', submitted_digest = rates_digest, \
                     submitted_by = $2, submitted_at = $3, updated_at = $3, version = version + 1 WHERE id = $1",
                vec![id.into(), cmd.operator.clone().into(), now.into()],
            ))
            .await?;
            "SUBMITTED"
        }
        VersionCommand::Approve => {
            state_is(&["SUBMITTED"])?;
            refuse_if_frozen(&txn, &b.market_code).await?;
            // A second person: never the staff member who submitted it.
            if v.submitted_by.as_deref() == Some(cmd.operator.as_str())
                || cmd.approval.as_ref().map(|a| a.0.as_str()) == v.submitted_by.as_deref()
            {
                return Err(ApiError(ErrorCode::SelfApprovalForbidden));
            }
            // Any change of the content since submission cancels the approval.
            if v.submitted_digest.as_deref() != Some(v.rates_digest.as_str())
                || rates::digest(&v.rates) != v.rates_digest
            {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            let (approver, approval) = cmd.approval.clone().unzip();
            txn.execute(stmt(
                "UPDATE devplat_price_versions SET state = 'APPROVED', approved_by = $2, approver_ref = $3, \
                     approval_ref = $4, approved_at = $5, approval_expires_at = $6, decided_at = $5, \
                     updated_at = $5, version = version + 1 WHERE id = $1",
                vec![
                    id.into(),
                    cmd.operator.clone().into(),
                    approver.into(),
                    approval.into(),
                    now.into(),
                    (now + Duration::hours(APPROVAL_TTL_HOURS)).into(),
                ],
            ))
            .await?;
            "APPROVED"
        }
        VersionCommand::Reject(code) => {
            state_is(&["SUBMITTED"])?;
            txn.execute(stmt(
                "UPDATE devplat_price_versions SET state = 'REJECTED', rejected_reason = $2, decided_at = $3, \
                     updated_at = $3, version = version + 1 WHERE id = $1",
                vec![id.into(), code.clone().into(), now.into()],
            ))
            .await?;
            "REJECTED"
        }
        VersionCommand::Schedule(_) | VersionCommand::Activate => {
            state_is(&["APPROVED"])?;
            refuse_if_frozen(&txn, &b.market_code).await?;
            if v.approval_expires_at.is_none_or(|e| e <= now) {
                return Err(ApiError(ErrorCode::ApprovalExpired));
            }
            if rates::digest(&v.rates) != v.rates_digest {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            let (effective_from, kind) = match &command {
                VersionCommand::Schedule(at) => {
                    if *at <= now + Duration::minutes(1)
                        || *at > now + Duration::days(MAX_SCHEDULE_DAYS)
                    {
                        return Err(invalid());
                    }
                    (*at, "SCHEDULED")
                }
                _ => {
                    if effective_version(&txn, b.id, now).await? == Some(id) {
                        return Err(ApiError(ErrorCode::InvalidState));
                    }
                    (now, "ACTIVATED")
                }
            };
            activation(&txn, cmd, b.id, id, kind, effective_from, now).await?;
            if kind == "ACTIVATED" {
                supersede_others(&txn, b.id, id, now).await?;
            }
            txn.execute(stmt(
                "UPDATE devplat_price_versions SET updated_at = $2, version = version + 1 WHERE id = $1",
                vec![id.into(), now.into()],
            ))
            .await?;
            if kind == "SCHEDULED" {
                "SCHEDULED"
            } else {
                "ACTIVE"
            }
        }
    };
    // ADR 0068 A7: the version (and any version it superseded) changed.
    bridge_events::record_price_versions_at(&txn, ev, b.id, now).await?;
    bump_book(&txn, b.id, now).await?;
    finish(
        &txn,
        request,
        cmd,
        name,
        Subject::PriceVersion,
        id,
        result,
        "PRICE_VERSION",
    )
    .await?;
    let (resource, n) = version_resource(&txn, id).await?;
    txn.commit().await?;
    Ok(bc::answer(result, Some(n), resource))
}

async fn activation<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    book_id: Uuid,
    version_id: Uuid,
    kind: &str,
    effective_from: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    txn.execute(stmt(
        "INSERT INTO devplat_price_activations (id, price_book_id, version_id, kind, effective_from, \
           operator_ref, approver_ref, recorded_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            Uuid::new_v4().into(),
            book_id.into(),
            version_id.into(),
            kind.into(),
            effective_from.into(),
            cmd.operator.clone().into(),
            cmd.approval.as_ref().map(|a| a.0.clone()).into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Every other approved version of the lineage that was ever activated is SUPERSEDED.
async fn supersede_others<C: ConnectionTrait>(
    txn: &C,
    book_id: Uuid,
    keep: Uuid,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    txn.execute(stmt(
        "UPDATE devplat_price_versions SET state = 'SUPERSEDED', updated_at = $3, version = version + 1 \
         WHERE price_book_id = $1 AND id <> $2 AND state = 'APPROVED' \
           AND id IN (SELECT version_id FROM devplat_price_activations WHERE price_book_id = $1 AND effective_from <= $3)",
        vec![book_id.into(), keep.into(), now.into()],
    ))
    .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackBody {
    pub expected_version: Option<i64>,
    pub version_id: Uuid,
}

/// The lineage as a command answer shows it.
async fn book_resource<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<(Value, i64), ApiError> {
    let r = db
        .query_one(stmt(
            "SELECT jsonb_build_object('priceBookId', id, 'productLine', product_line, 'currency', currency, \
                    'marketCode', market_code, 'updatedAt', updated_at, 'version', version) AS v, version \
             FROM devplat_price_books WHERE id = $1",
            vec![id.into()],
        ))
        .await?
        .ok_or_else(not_found)?;
    let mut v: Value = r.try_get("", "v")?;
    v["effectiveVersionId"] = json!(effective_version(db, id, Utc::now()).await?);
    Ok((v, r.try_get("", "version")?))
}

/// `pricing.price-book.rollback` (R4): makes an older version of the lineage — one that
/// was activated before and has been superseded — effective again now, at the lineage's
/// version. The history stays append-only (a ROLLED_BACK activation).
pub async fn rollback(
    db: &DatabaseConnection,
    ev: BridgeEvents,
    request: &RequestContext,
    book_id: Uuid,
    body: RollbackBody,
    cmd: &BridgeCommand,
) -> Result<Value, ApiError> {
    const COMMAND: &str = "pricing.price-book.rollback";
    let b = book(db, book_id, false).await?;
    if let Some(v) = replayed(
        bc::prior(db, cmd, COMMAND, Subject::PriceBook, book_id).await?,
        book_resource(db, book_id).await?.0,
    ) {
        return Ok(v);
    }
    let now = Utc::now();
    let txn = db.begin().await?;
    bc::lock_version(&txn, cmd, Subject::PriceBook, book_id).await?;
    refuse_if_frozen(&txn, &b.market_code).await?;
    let target = version(&txn, body.version_id).await?;
    let was_active = txn
        .query_one(stmt(
            "SELECT 1 AS x FROM devplat_price_activations WHERE price_book_id = $1 AND version_id = $2 \
               AND kind IN ('ACTIVATED', 'ROLLED_BACK') AND effective_from <= $3",
            vec![book_id.into(), target.id.into(), now.into()],
        ))
        .await?
        .is_some();
    if target.book_id != book_id
        || target.state != "SUPERSEDED"
        || !was_active
        || effective_version(&txn, book_id, now).await? == Some(target.id)
    {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    activation(&txn, cmd, book_id, target.id, "ROLLED_BACK", now, now).await?;
    // The version taken back is approved again; the one it replaces is superseded.
    txn.execute(stmt(
        "UPDATE devplat_price_versions SET state = 'APPROVED', updated_at = $2, version = version + 1 WHERE id = $1",
        vec![target.id.into(), now.into()],
    ))
    .await?;
    supersede_others(&txn, book_id, target.id, now).await?;
    bridge_events::record_price_versions_at(&txn, ev, book_id, now).await?;
    bump_book(&txn, book_id, now).await?;
    finish(
        &txn,
        request,
        cmd,
        COMMAND,
        Subject::PriceBook,
        book_id,
        "ROLLED_BACK",
        "PRICE_BOOK",
    )
    .await?;
    let (resource, n) = book_resource(&txn, book_id).await?;
    txn.commit().await?;
    Ok(bc::answer("ROLLED_BACK", Some(n), resource))
}

// ---------------------------------------------------------------------------------------
// Market freezes
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeBody {
    pub expected_version: Option<i64>,
    pub reason_code: String,
}

async fn freeze_resource<C: ConnectionTrait>(db: &C, market: &str) -> Result<Value, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT jsonb_build_object('marketCode', market_code, 'state', state, 'reasonCode', reason_code, \
                    'frozenAt', frozen_at, 'updatedAt', updated_at, 'version', version) AS v \
             FROM devplat_market_freezes WHERE market_code = $1",
            vec![market.into()],
        ))
        .await?
        .map(|r| r.try_get::<Value>("", "v"))
        .transpose()?
        .unwrap_or_else(|| json!({ "marketCode": market, "state": "UNFROZEN", "version": 0 })))
}

/// Locks the row of a holder-like subject found by a natural key (`FOR UPDATE`) and checks
/// the expected version (0: no row yet). `Some(id)` of the existing row.
async fn lock_natural<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Option<Uuid>, ApiError> {
    let expected = cmd.version()?;
    match txn.query_one(stmt(sql, values)).await? {
        Some(r) => {
            let v: i64 = r.try_get("", "version")?;
            if v != expected {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            Ok(Some(r.try_get("", "id")?))
        }
        None if expected == 0 => Ok(None),
        None => Err(not_found()),
    }
}

/// `pricing.market.freeze` / `pricing.market.unfreeze` (R4), at the market's freeze
/// version (0: the market was never frozen).
pub async fn set_freeze(
    db: &DatabaseConnection,
    request: &RequestContext,
    market: &str,
    freeze: bool,
    body: FreezeBody,
    cmd: &BridgeCommand,
) -> Result<Value, ApiError> {
    let command = if freeze {
        "pricing.market.freeze"
    } else {
        "pricing.market.unfreeze"
    };
    if !market_code(market) || !valid_reason(&body.reason_code) {
        return Err(invalid());
    }
    let existing: Option<Uuid> = db
        .query_one(stmt(
            "SELECT id FROM devplat_market_freezes WHERE market_code = $1",
            vec![market.into()],
        ))
        .await?
        .map(|r| r.try_get("", "id"))
        .transpose()?;
    let subject = existing.unwrap_or(cmd.key);
    if let Some(v) = replayed(
        bc::prior(db, cmd, command, Subject::Market, subject).await?,
        freeze_resource(db, market).await?,
    ) {
        return Ok(v);
    }
    let now = Utc::now();
    let txn = db.begin().await?;
    let row = lock_natural(
        &txn,
        cmd,
        "SELECT id, version, state FROM devplat_market_freezes WHERE market_code = $1 FOR UPDATE",
        vec![market.into()],
    )
    .await?;
    let state = if freeze { "FROZEN" } else { "UNFROZEN" };
    let id = match row {
        Some(id) => {
            let n = txn
                .execute(stmt(
                    "UPDATE devplat_market_freezes SET state = $2, reason_code = $3, changed_by = $4, \
                         frozen_at = CASE WHEN $2 = 'FROZEN' THEN $5 ELSE NULL END, updated_at = $5, \
                         version = version + 1 WHERE id = $1 AND state <> $2",
                    vec![
                        id.into(),
                        state.into(),
                        body.reason_code.clone().into(),
                        cmd.operator.clone().into(),
                        now.into(),
                    ],
                ))
                .await?
                .rows_affected();
            if n != 1 {
                return Err(ApiError(ErrorCode::InvalidState));
            }
            id
        }
        None if freeze => {
            let n = txn
                .execute(stmt(
                    "INSERT INTO devplat_market_freezes (id, market_code, state, reason_code, changed_by, \
                       frozen_at, created_at, updated_at) VALUES ($1, $2, 'FROZEN', $3, $4, $5, $5, $5) \
                     ON CONFLICT DO NOTHING",
                    vec![
                        cmd.key.into(),
                        market.into(),
                        body.reason_code.clone().into(),
                        cmd.operator.clone().into(),
                        now.into(),
                    ],
                ))
                .await?
                .rows_affected();
            if n != 1 {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            cmd.key
        }
        // A market never frozen cannot be unfrozen.
        None => return Err(ApiError(ErrorCode::InvalidState)),
    };
    finish(
        &txn,
        request,
        cmd,
        command,
        Subject::Market,
        id,
        state,
        "MARKET",
    )
    .await?;
    let resource = freeze_resource(&txn, market).await?;
    txn.commit().await?;
    let n = resource["version"].as_i64();
    Ok(bc::answer(state, n, resource))
}

// ---------------------------------------------------------------------------------------
// CF Meet user plans: assignments and overrides
// ---------------------------------------------------------------------------------------

fn holder_kind(kind: &str) -> Result<&'static str, ApiError> {
    match kind {
        "USER" => Ok("USER"),
        "ORGANIZATION" => Ok("ORGANIZATION"),
        _ => Err(invalid()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignBody {
    pub expected_version: Option<i64>,
    pub holder_kind: String,
    pub holder_id: Uuid,
    pub plan_code: String,
    pub price_book_id: Uuid,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub billing_account_ref: Option<String>,
}

async fn holder_resource<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    holder: Uuid,
) -> Result<Value, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT jsonb_build_object('holderKind', holder_kind, 'holderId', holder_ref, 'planCode', plan_code, \
                    'priceBookId', price_book_id, 'updatedAt', updated_at, 'version', version) AS v \
             FROM devplat_plan_holders WHERE holder_kind = $1 AND holder_ref = $2",
            vec![kind.into(), holder.into()],
        ))
        .await?
        .map(|r| r.try_get::<Value>("", "v"))
        .transpose()?
        .unwrap_or_else(|| {
            json!({ "holderKind": kind, "holderId": holder, "planCode": "FREE", "version": 0 })
        }))
}

/// `pricing.plan.assign` (R3): the CF Meet user plan of a user or business organization,
/// at the holder's version (0: never assigned, on Free). The plan must exist in the
/// CONSUMER lineage's effective version (FREE always does). History is append-only.
pub async fn assign(
    db: &DatabaseConnection,
    request: &RequestContext,
    body: AssignBody,
    cmd: &BridgeCommand,
) -> Result<Value, ApiError> {
    const COMMAND: &str = "pricing.plan.assign";
    let kind = holder_kind(&body.holder_kind)?;
    let code = body.plan_code.clone();
    let reason_code = reason(body.reason_code.clone(), "HQ_ASSIGNED")?;
    if !rates::plan_code(&code)
        || body.billing_account_ref.as_deref().is_some_and(|r| {
            r.is_empty()
                || r.len() > 128
                || !r
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:/-".contains(&c))
        })
    {
        return Err(invalid());
    }
    let b = book(db, body.price_book_id, false).await?;
    if b.product_line != rates::CONSUMER {
        return Err(invalid());
    }
    let existing: Option<Uuid> = db
        .query_one(stmt(
            "SELECT id FROM devplat_plan_holders WHERE holder_kind = $1 AND holder_ref = $2",
            vec![kind.into(), body.holder_id.into()],
        ))
        .await?
        .map(|r| r.try_get("", "id"))
        .transpose()?;
    let subject = existing.unwrap_or(cmd.key);
    if let Some(v) = replayed(
        bc::prior(db, cmd, COMMAND, Subject::PlanHolder, subject).await?,
        holder_resource(db, kind, body.holder_id).await?,
    ) {
        return Ok(v);
    }
    let now = Utc::now();
    if code != "FREE" {
        let v = effective_version(db, b.id, now)
            .await?
            .ok_or(ApiError(ErrorCode::InvalidState))?;
        if rates::plan(&version(db, v).await?.rates, &code).is_none() {
            return Err(ApiError(ErrorCode::InvalidState));
        }
    }
    let txn = db.begin().await?;
    let row = lock_natural(
        &txn,
        cmd,
        "SELECT id, version FROM devplat_plan_holders WHERE holder_kind = $1 AND holder_ref = $2 FOR UPDATE",
        vec![kind.into(), body.holder_id.into()],
    )
    .await?;
    let id = match row {
        Some(id) => {
            txn.execute(stmt(
                "UPDATE devplat_plan_holders SET plan_code = $2, price_book_id = $3, \
                     billing_account_ref = COALESCE($4, billing_account_ref), updated_at = $5, \
                     version = version + 1 WHERE id = $1",
                vec![
                    id.into(),
                    code.clone().into(),
                    b.id.into(),
                    body.billing_account_ref.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            id
        }
        None => {
            let n = txn
                .execute(stmt(
                    "INSERT INTO devplat_plan_holders (id, holder_kind, holder_ref, plan_code, price_book_id, \
                       billing_account_ref, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
                    vec![
                        cmd.key.into(),
                        kind.into(),
                        body.holder_id.into(),
                        code.clone().into(),
                        b.id.into(),
                        body.billing_account_ref.clone().into(),
                        now.into(),
                    ],
                ))
                .await?
                .rows_affected();
            if n != 1 {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            cmd.key
        }
    };
    txn.execute(stmt(
        "INSERT INTO devplat_plan_assignments_consumer (id, holder_id, holder_kind, plan_code, price_book_id, \
           reason_code, assigned_by, effective_from) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            Uuid::new_v4().into(),
            id.into(),
            kind.into(),
            code.clone().into(),
            b.id.into(),
            reason_code.into(),
            cmd.operator.clone().into(),
            now.into(),
        ],
    ))
    .await?;
    finish(
        &txn,
        request,
        cmd,
        COMMAND,
        Subject::PlanHolder,
        id,
        &code,
        "PLAN_HOLDER",
    )
    .await?;
    let resource = holder_resource(&txn, kind, body.holder_id).await?;
    txn.commit().await?;
    let n = resource["version"].as_i64();
    Ok(bc::answer(&code, n, resource))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OverrideBody {
    pub expected_version: Option<i64>,
    pub holder_kind: String,
    pub holder_id: Uuid,
    /// The overriding limits; `null` revokes the override.
    pub limits: Option<Value>,
    pub reason_code: String,
}

async fn override_resource<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    holder: Uuid,
) -> Result<Value, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT jsonb_build_object('overrideId', id, 'holderKind', holder_kind, 'holderId', holder_ref, \
                    'state', state, 'limits', limits, 'reasonCode', reason_code, 'updatedAt', updated_at, \
                    'version', version) AS v \
             FROM devplat_plan_overrides WHERE holder_kind = $1 AND holder_ref = $2",
            vec![kind.into(), holder.into()],
        ))
        .await?
        .map(|r| r.try_get::<Value>("", "v"))
        .transpose()?
        .unwrap_or_else(|| json!({ "holderKind": kind, "holderId": holder, "state": "NONE", "version": 0 })))
}

/// `pricing.override.change` (R4): sets (or, with `limits: null`, revokes) a holder's
/// limit overrides, at the override's version (0: none yet).
pub async fn change_override(
    db: &DatabaseConnection,
    request: &RequestContext,
    body: OverrideBody,
    cmd: &BridgeCommand,
) -> Result<Value, ApiError> {
    const COMMAND: &str = "pricing.override.change";
    let kind = holder_kind(&body.holder_kind)?;
    if !valid_reason(&body.reason_code) {
        return Err(invalid());
    }
    if let Some(l) = &body.limits {
        rates::validate_limits(l)?;
    }
    let existing: Option<Uuid> = db
        .query_one(stmt(
            "SELECT id FROM devplat_plan_overrides WHERE holder_kind = $1 AND holder_ref = $2",
            vec![kind.into(), body.holder_id.into()],
        ))
        .await?
        .map(|r| r.try_get("", "id"))
        .transpose()?;
    let subject = existing.unwrap_or(cmd.key);
    if let Some(v) = replayed(
        bc::prior(db, cmd, COMMAND, Subject::PlanOverride, subject).await?,
        override_resource(db, kind, body.holder_id).await?,
    ) {
        return Ok(v);
    }
    let now = Utc::now();
    let state = if body.limits.is_some() {
        "ACTIVE"
    } else {
        "REVOKED"
    };
    let txn = db.begin().await?;
    let row = lock_natural(
        &txn,
        cmd,
        "SELECT id, version FROM devplat_plan_overrides WHERE holder_kind = $1 AND holder_ref = $2 FOR UPDATE",
        vec![kind.into(), body.holder_id.into()],
    )
    .await?;
    let id = match row {
        Some(id) => {
            txn.execute(stmt(
                "UPDATE devplat_plan_overrides SET state = $2, limits = COALESCE($3, limits), reason_code = $4, \
                     changed_by = $5, updated_at = $6, version = version + 1 WHERE id = $1",
                vec![
                    id.into(),
                    state.into(),
                    body.limits.clone().into(),
                    body.reason_code.clone().into(),
                    cmd.operator.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            id
        }
        // Nothing to revoke.
        None if body.limits.is_none() => return Err(ApiError(ErrorCode::InvalidState)),
        None => {
            let n = txn
                .execute(stmt(
                    "INSERT INTO devplat_plan_overrides (id, holder_kind, holder_ref, state, limits, reason_code, \
                       changed_by, created_at, updated_at) VALUES ($1, $2, $3, 'ACTIVE', $4, $5, $6, $7, $7) \
                     ON CONFLICT DO NOTHING",
                    vec![
                        cmd.key.into(),
                        kind.into(),
                        body.holder_id.into(),
                        body.limits.clone().into(),
                        body.reason_code.clone().into(),
                        cmd.operator.clone().into(),
                        now.into(),
                    ],
                ))
                .await?
                .rows_affected();
            if n != 1 {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            cmd.key
        }
    };
    finish(
        &txn,
        request,
        cmd,
        COMMAND,
        Subject::PlanOverride,
        id,
        state,
        "PLAN_OVERRIDE",
    )
    .await?;
    let resource = override_resource(&txn, kind, body.holder_id).await?;
    txn.commit().await?;
    let n = resource["version"].as_i64();
    Ok(bc::answer(state, n, resource))
}

// ---------------------------------------------------------------------------------------
// Plan enforcement support: a holder's effective limits (signed internal read)
// ---------------------------------------------------------------------------------------

/// How long a caller may cache an effective-limits answer (the answer says so).
pub const LIMITS_CACHE_SECONDS: u64 = 60;

/// ADR 0068 A6 (plan enforcement where the usage happens): the effective CF Meet plan
/// limits of a user or business organization — its plan (FREE when never assigned) priced
/// by the effective version of its CONSUMER lineage (the holder's, else the GLOBAL one),
/// with an ACTIVE override applied key by key. A limit the plan does not set is `null`
/// (the caller keeps today's behaviour for it). Ids, codes and limits only.
pub async fn effective_limits<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    holder: Uuid,
    now: DateTime<Utc>,
) -> Result<Value, ApiError> {
    let kind = holder_kind(kind)?;
    let assigned = db
        .query_one(stmt(
            "SELECT plan_code, price_book_id, version FROM devplat_plan_holders              WHERE holder_kind = $1 AND holder_ref = $2",
            vec![kind.into(), holder.into()],
        ))
        .await?;
    let (plan_code, book, holder_version) = match &assigned {
        Some(r) => (
            r.try_get::<String>("", "plan_code")?,
            Some(r.try_get::<Uuid>("", "price_book_id")?),
            r.try_get::<i64>("", "version")?,
        ),
        None => ("FREE".to_string(), None, 0),
    };
    let book = match book {
        Some(b) => Some(b),
        None => db
            .query_one(stmt(
                "SELECT id FROM devplat_price_books WHERE product_line = 'CONSUMER' AND market_code = 'GLOBAL'                  ORDER BY currency LIMIT 1",
                vec![],
            ))
            .await?
            .map(|r| r.try_get::<Uuid>("", "id"))
            .transpose()?,
    };
    let effective = match book {
        Some(b) => effective_version(db, b, now).await?,
        None => None,
    };
    let plan_limits = match effective {
        Some(v) => rates::plan(&version(db, v).await?.rates, &plan_code)
            .map(|p| p["limits"].clone())
            .unwrap_or(Value::Null),
        None => Value::Null,
    };
    let over = db
        .query_one(stmt(
            "SELECT limits, version FROM devplat_plan_overrides              WHERE holder_kind = $1 AND holder_ref = $2 AND state = 'ACTIVE'",
            vec![kind.into(), holder.into()],
        ))
        .await?;
    let (override_limits, override_version) = match &over {
        Some(r) => (
            r.try_get::<Option<Value>>("", "limits")?,
            Some(r.try_get::<i64>("", "version")?),
        ),
        None => (None, None),
    };
    let mut limits = Map::new();
    for (key, _) in rates::PLAN_LIMITS {
        let from_override = override_limits.as_ref().and_then(|o| o.get(*key)).cloned();
        let v =
            from_override.unwrap_or_else(|| plan_limits.get(*key).cloned().unwrap_or(Value::Null));
        limits.insert((*key).to_string(), v);
    }
    Ok(json!({
        "holderKind": kind,
        "holderId": holder,
        "planCode": plan_code,
        "assigned": assigned.is_some(),
        "holderVersion": holder_version,
        "priceBookId": book,
        "versionId": effective,
        "overrideApplied": override_limits.is_some(),
        "overrideVersion": override_version,
        "limits": limits,
        "evaluatedAt": now,
        "cacheSeconds": LIMITS_CACHE_SECONDS,
    }))
}

// ---------------------------------------------------------------------------------------
// Charge simulation (a read)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulationBody {
    pub price_book_id: Uuid,
    /// A specific version (any state); omitted: the lineage's effective version.
    #[serde(default)]
    pub version_id: Option<Uuid>,
    /// PARTNER_PLATFORM: `{usageType: quantity}`.
    #[serde(default)]
    pub usage: Option<Map<String, Value>>,
    /// CONSUMER: the plan and the billing cycle (`MONTHLY` | `YEARLY`).
    #[serde(default)]
    pub plan_code: Option<String>,
    #[serde(default)]
    pub cycle: Option<String>,
}

/// `pricing.charges.simulate`: what a usage vector (partner) or a plan (CF Meet user)
/// would cost under a version. No side effect, nothing is charged.
pub async fn simulate(db: &DatabaseConnection, body: SimulationBody) -> Result<Value, ApiError> {
    let b = book(db, body.price_book_id, false).await?;
    let currency: String = db
        .query_one(stmt(
            "SELECT currency FROM devplat_price_books WHERE id = $1",
            vec![b.id.into()],
        ))
        .await?
        .ok_or_else(not_found)?
        .try_get("", "currency")?;
    let vid = match body.version_id {
        Some(v) => v,
        None => effective_version(db, b.id, Utc::now())
            .await?
            .ok_or(ApiError(ErrorCode::InvalidState))?,
    };
    let v = version(db, vid).await?;
    if v.book_id != b.id {
        return Err(not_found());
    }
    let mut out = match b.product_line.as_str() {
        rates::PARTNER_PLATFORM => {
            if body.plan_code.is_some() || body.cycle.is_some() {
                return Err(invalid());
            }
            rates::simulate_partner(&v.rates, &body.usage.ok_or_else(invalid)?)?
        }
        _ => {
            if body.usage.is_some() {
                return Err(invalid());
            }
            let code = body.plan_code.ok_or_else(invalid)?;
            let field = match body.cycle.as_deref().unwrap_or("MONTHLY") {
                "MONTHLY" => "monthlyMinor",
                "YEARLY" => "yearlyMinor",
                _ => return Err(invalid()),
            };
            let amount = match rates::plan(&v.rates, &code) {
                Some(p) => p[field].as_u64().unwrap_or(0),
                None if code == "FREE" => 0,
                None => return Err(not_found()),
            };
            json!({ "planCode": code, "cycle": body.cycle.unwrap_or_else(|| "MONTHLY".into()),
                    "totalMinor": amount })
        }
    };
    out["priceBookId"] = json!(b.id);
    out["versionId"] = json!(v.id);
    out["versionState"] = json!(v.state);
    out["currency"] = json!(currency);
    out["estimate"] = json!(true);
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------

/// `pricing.plans.list`: the plans of every CONSUMER lineage's effective version (or of
/// one lineage), FREE included even when no version is effective yet.
pub async fn plans(db: &DatabaseConnection, book_id: Option<Uuid>) -> Result<Value, ApiError> {
    let books = db
        .query_all(stmt(
            "SELECT id, currency, market_code FROM devplat_price_books \
             WHERE product_line = 'CONSUMER' AND ($1::uuid IS NULL OR id = $1) ORDER BY market_code, currency",
            vec![book_id.into()],
        ))
        .await?;
    if book_id.is_some() && books.is_empty() {
        return Err(not_found());
    }
    let mut items = Vec::new();
    let now = Utc::now();
    for r in &books {
        let id: Uuid = r.try_get("", "id")?;
        let base = json!({ "priceBookId": id, "currency": r.try_get::<String>("", "currency")?,
                           "marketCode": r.try_get::<String>("", "market_code")? });
        let effective = effective_version(db, id, now).await?;
        let content = match effective {
            Some(v) => version(db, v).await?.rates,
            None => json!({ "plans": [] }),
        };
        let mut list: Vec<Value> = content["plans"].as_array().cloned().unwrap_or_default();
        if !list.iter().any(|p| p["code"] == "FREE") {
            list.insert(
                0,
                json!({ "code": "FREE", "label": "Free", "monthlyMinor": 0, "yearlyMinor": 0, "limits": null }),
            );
        }
        for p in list {
            let mut item = base.clone();
            item["versionId"] = json!(effective);
            item["planCode"] = p["code"].clone();
            item["label"] = p["label"].clone();
            item["monthlyMinor"] = p["monthlyMinor"].clone();
            item["yearlyMinor"] = p["yearlyMinor"].clone();
            item["limits"] = p["limits"].clone();
            items.push(item);
        }
    }
    Ok(json!({ "items": items, "nextCursor": null }))
}

/// The keyset statements of the pricing lists (rows carry `k_at`, `k_id`, `v`).
pub const BOOKS_SQL: &str = "SELECT b.created_at AS k_at, b.id AS k_id, jsonb_build_object( \
    'priceBookId', b.id, 'productLine', b.product_line, 'currency', b.currency, 'marketCode', b.market_code, \
    'effectiveVersionId', (SELECT a.version_id FROM devplat_price_activations a WHERE a.price_book_id = b.id \
        AND a.effective_from <= now() ORDER BY a.effective_from DESC, a.seq DESC LIMIT 1), \
    'frozen', EXISTS (SELECT 1 FROM devplat_market_freezes f WHERE f.market_code = b.market_code AND f.state = 'FROZEN'), \
    'updatedAt', b.updated_at, 'version', b.version) AS v \
    FROM devplat_price_books b WHERE ($1::text IS NULL OR b.product_line = $1) \
      AND ($2::timestamptz IS NULL OR (b.created_at, b.id) < ($2, $3::uuid)) \
    ORDER BY b.created_at DESC, b.id DESC LIMIT $4";

pub fn versions_sql() -> String {
    format!(
        "SELECT v.created_at AS k_at, v.id AS k_id, {VERSION_SUMMARY} AS v \
         FROM devplat_price_versions v WHERE v.price_book_id = $1 \
           AND ($2::timestamptz IS NULL OR (v.created_at, v.id) < ($2, $3::uuid)) \
         ORDER BY v.created_at DESC, v.id DESC LIMIT $4"
    )
}

pub const ACTIVATIONS_SQL: &str = "SELECT a.recorded_at AS k_at, a.id AS k_id, jsonb_build_object( \
    'activationId', a.id, 'priceBookId', a.price_book_id, 'versionId', a.version_id, 'kind', a.kind, \
    'effectiveFrom', a.effective_from, 'operatorRef', a.operator_ref, 'approverRef', a.approver_ref, \
    'recordedAt', a.recorded_at) AS v \
    FROM devplat_price_activations a WHERE a.price_book_id = $1 \
      AND ($2::timestamptz IS NULL OR (a.recorded_at, a.id) < ($2, $3::uuid)) \
    ORDER BY a.recorded_at DESC, a.id DESC LIMIT $4";

pub const FREEZES_SQL: &str = "SELECT f.updated_at AS k_at, f.id AS k_id, jsonb_build_object( \
    'marketCode', f.market_code, 'state', f.state, 'reasonCode', f.reason_code, 'changedBy', f.changed_by, \
    'frozenAt', f.frozen_at, 'updatedAt', f.updated_at, 'version', f.version) AS v \
    FROM devplat_market_freezes f WHERE ($1::text IS NULL OR f.state = $1) \
      AND ($2::timestamptz IS NULL OR (f.updated_at, f.id) < ($2, $3::uuid)) \
    ORDER BY f.updated_at DESC, f.id DESC LIMIT $4";

pub const ASSIGNMENTS_SQL: &str = "SELECT a.effective_from AS k_at, a.id AS k_id, jsonb_build_object( \
    'assignmentId', a.id, 'holderKind', a.holder_kind, 'holderId', h.holder_ref, 'planCode', a.plan_code, \
    'priceBookId', a.price_book_id, 'reasonCode', a.reason_code, 'assignedBy', a.assigned_by, \
    'effectiveFrom', a.effective_from, 'current', (h.plan_code = a.plan_code AND a.seq = \
        (SELECT max(x.seq) FROM devplat_plan_assignments_consumer x WHERE x.holder_id = a.holder_id))) AS v \
    FROM devplat_plan_assignments_consumer a JOIN devplat_plan_holders h ON h.id = a.holder_id \
    WHERE ($1::uuid IS NULL OR h.holder_ref = $1) AND ($5::text IS NULL OR a.holder_kind = $5) \
      AND ($6::text IS NULL OR a.plan_code = $6) \
      AND ($2::timestamptz IS NULL OR (a.effective_from, a.id) < ($2, $3::uuid)) \
    ORDER BY a.effective_from DESC, a.id DESC LIMIT $4";

pub const OVERRIDES_SQL: &str = "SELECT o.updated_at AS k_at, o.id AS k_id, jsonb_build_object( \
    'overrideId', o.id, 'holderKind', o.holder_kind, 'holderId', o.holder_ref, 'state', o.state, \
    'limits', o.limits, 'reasonCode', o.reason_code, 'changedBy', o.changed_by, 'updatedAt', o.updated_at, \
    'version', o.version) AS v \
    FROM devplat_plan_overrides o WHERE ($1::uuid IS NULL OR o.holder_ref = $1) \
      AND ($5::text IS NULL OR o.state = $5) \
      AND ($2::timestamptz IS NULL OR (o.updated_at, o.id) < ($2, $3::uuid)) \
    ORDER BY o.updated_at DESC, o.id DESC LIMIT $4";

/// The book must exist (404 otherwise).
pub async fn existing_book<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<(), ApiError> {
    book(db, id, false).await.map(|_| ())
}

// ---------------------------------------------------------------------------------------
// Finance reports (§6.12): one currency per figure, minor units, never "revenue"
// ---------------------------------------------------------------------------------------

/// `finance.usage.report`: metered usage per partner workspace and usage type over
/// `[from, end)` UTC days, with the workspace's current commercial plan code.
pub async fn usage_report(
    db: &DatabaseConnection,
    from: chrono::NaiveDate,
    end: chrono::NaiveDate,
    offset: i64,
    limit: usize,
) -> Result<(Vec<Value>, bool), ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT jsonb_build_object('organizationId', r.tenant_id, 'planCode', \
                    (SELECT c.code FROM devplat_plan_assignments pa JOIN devplat_commercial_plans c ON c.id = pa.plan_id \
                      WHERE pa.tenant_id = r.tenant_id AND pa.application_id IS NULL AND pa.superseded_by IS NULL LIMIT 1), \
                    'usageType', r.usage_type, 'unit', MIN(r.unit), 'quantity', SUM(r.quantity)::bigint, \
                    'factCount', SUM(r.fact_count)::bigint) AS v \
             FROM devplat_usage_rollups_daily r WHERE r.usage_day >= $1 AND r.usage_day < $2 \
             GROUP BY r.tenant_id, r.usage_type ORDER BY r.tenant_id, r.usage_type OFFSET $3 LIMIT $4",
            vec![from.into(), end.into(), offset.into(), (limit as i64 + 1).into()],
        ))
        .await?;
    page_values(rows, limit)
}

fn page_values(rows: Vec<QueryResult>, limit: usize) -> Result<(Vec<Value>, bool), ApiError> {
    let more = rows.len() > limit;
    let items = rows
        .iter()
        .take(limit)
        .map(|r| r.try_get::<Value>("", "v").map_err(ApiError::from))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((items, more))
}

/// `finance.estimated-charges.report`: per partner workspace, what its metered usage over
/// the window would cost under a PARTNER_PLATFORM lineage's effective version (the window
/// counts as one month for minimum charges). An estimate, never charged.
pub async fn estimated_charges(
    db: &DatabaseConnection,
    book_id: Uuid,
    from: chrono::NaiveDate,
    end: chrono::NaiveDate,
    offset: i64,
    limit: usize,
) -> Result<(Value, Vec<Value>, bool), ApiError> {
    let b = book(db, book_id, false).await?;
    if b.product_line != rates::PARTNER_PLATFORM {
        return Err(invalid());
    }
    let vid = effective_version(db, book_id, Utc::now()).await?;
    let content = match vid {
        Some(v) => version(db, v).await?.rates,
        None => rates::empty(rates::PARTNER_PLATFORM),
    };
    let tenants = db
        .query_all(stmt(
            "SELECT DISTINCT tenant_id FROM devplat_usage_rollups_daily WHERE usage_day >= $1 AND usage_day < $2 \
             ORDER BY tenant_id OFFSET $3 LIMIT $4",
            vec![from.into(), end.into(), offset.into(), (limit as i64 + 1).into()],
        ))
        .await?;
    let more = tenants.len() > limit;
    let mut items = Vec::new();
    for t in tenants.iter().take(limit) {
        let tenant: Uuid = t.try_get("", "tenant_id")?;
        let usage_rows = db
            .query_all(stmt(
                "SELECT usage_type, SUM(quantity)::bigint AS q FROM devplat_usage_rollups_daily \
                 WHERE tenant_id = $1 AND usage_day >= $2 AND usage_day < $3 GROUP BY usage_type",
                vec![tenant.into(), from.into(), end.into()],
            ))
            .await?;
        let mut usage = Map::new();
        for rate_type in rates::USAGE_TYPES {
            let Some(ledger) = rates::ledger_type(rate_type) else {
                continue;
            };
            for r in &usage_rows {
                if r.try_get::<String>("", "usage_type")? == ledger {
                    let q: i64 = r.try_get("", "q")?;
                    usage.insert((*rate_type).to_string(), json!(q.max(0)));
                }
            }
        }
        let mut sim = rates::simulate_partner(&content, &usage)?;
        sim["organizationId"] = json!(tenant);
        items.push(sim);
    }
    let currency: String = db
        .query_one(stmt(
            "SELECT currency FROM devplat_price_books WHERE id = $1",
            vec![book_id.into()],
        ))
        .await?
        .ok_or_else(not_found)?
        .try_get("", "currency")?;
    Ok((
        json!({ "priceBookId": book_id, "versionId": vid, "currency": currency, "estimate": true }),
        items,
        more,
    ))
}

/// `finance.plan-spread.report`: holders per CF Meet user plan as of `at` (users and
/// organizations never assigned are on Free and are not counted: the Developer Platform
/// does not know them), and the assignments made in `[since, at)`.
pub async fn plan_spread(
    db: &DatabaseConnection,
    since: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) -> Result<Vec<Value>, ApiError> {
    let rows = db
        .query_all(stmt(
            "WITH latest AS ( \
               SELECT DISTINCT ON (a.holder_id) a.holder_id, a.holder_kind, a.plan_code \
                 FROM devplat_plan_assignments_consumer a WHERE a.effective_from <= $1 \
                ORDER BY a.holder_id, a.seq DESC) \
             SELECT jsonb_build_object('planCode', l.plan_code, 'holderKind', l.holder_kind, \
                    'holders', count(*), 'assignedInWindow', \
                    (SELECT count(*) FROM devplat_plan_assignments_consumer x WHERE x.plan_code = l.plan_code \
                       AND x.holder_kind = l.holder_kind AND x.effective_from < $1 \
                       AND ($2::timestamptz IS NULL OR x.effective_from >= $2))) AS v \
               FROM latest l GROUP BY l.plan_code, l.holder_kind ORDER BY l.plan_code, l.holder_kind",
            vec![at.into(), since.into()],
        ))
        .await?;
    rows.iter()
        .map(|r| r.try_get::<Value>("", "v").map_err(ApiError::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_codes_are_bounded() {
        assert!(market_code("GLOBAL") && market_code("NG") && market_code("EU_WEST"));
        assert!(!market_code("ng") && !market_code("G") && !market_code("A-B"));
        assert!(reason(None, "HQ_ASSIGNED").is_ok());
        assert!(reason(Some("bad reason".into()), "X").is_err());
    }

    #[test]
    fn every_pricing_command_is_two_person() {
        for c in [
            VersionCommand::Submit,
            VersionCommand::Approve,
            VersionCommand::Activate,
        ] {
            assert!(c.two_person());
            assert!(c.name().starts_with("pricing.version."));
        }
    }
}

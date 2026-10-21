//! Server-side derivatives of MANAGED attachments (§80): a THUMBNAIL of every READY
//! MANAGED PNG / JPEG image, and an INSPECTION (measured container, duration and
//! dimensions, [`super::media_inspect`]) of every READY MANAGED image, voice note and
//! video of an inspectable type.
//!
//! * Only [`super::processing::eligible`] attachments (MANAGED and READY — scanned where
//!   policy requires it) are processed; the database refuses a derivative of anything
//!   else (migration `m20261118_000016`). PRIVATE_E2EE attachments are ciphertext and are
//!   never read for processing: their thumbnails are made, encrypted and referenced by
//!   the sending client ([`super::processing::E2eeDerivative`]).
//! * Untrusted input is decoded defensively: the stored object is re-checked against its
//!   verified SHA-256, dimensions are read from the header and refused above
//!   [`MAX_PIXELS`] before any pixel is decoded, the decoder runs with explicit
//!   allocation / dimension limits on a blocking thread under a timeout, and only PNG /
//!   JPEG decoders are compiled in.
//! * The thumbnail is a re-encoded PNG of at most [`THUMBNAIL_MAX_EDGE`] pixels per edge:
//!   no metadata (EXIF location, comments) of the original survives.
//! * A derivative lives and dies with its attachment: deleting the attachment queues the
//!   derivative object for deletion too (legal holds apply to both).
//! * Storage (ADR 0054): a rendered thumbnail's bytes are reserved against the environment
//!   budget, under the usage row lock, *before* its object is written; the database trigger
//!   turns the reservation into committed bytes when the thumbnail becomes READY. A
//!   thumbnail that does not fit is `QUOTA_DEFERRED` (no object written, the original is
//!   untouched) and is retried later or when the budget changes. Every attempt writes its
//!   own object key, so a crashed or superseded attempt's object is queued for deletion
//!   with its reservation as the charge and never overwrites or deletes a live thumbnail.

use std::{io::Cursor, time::Duration};

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{processing, AttachmentRow, ATTACHMENT_COLUMNS};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    events::bounded_error,
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const THUMBNAIL_MAX_EDGE: u32 = 320;
/// Largest source object read for processing.
pub const MAX_SOURCE_BYTES: usize = 25 * 1024 * 1024;
/// Largest source image (pixels) decoded.
pub const MAX_PIXELS: u64 = 40_000_000;
const MAX_EDGE: u32 = 16_384;
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
const DECODE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: i32 = 3;
const PROCESSABLE: &[&str] = &["image/png", "image/jpeg"];

/// A rendered thumbnail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
}

fn limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(MAX_EDGE);
    l.max_image_height = Some(MAX_EDGE);
    l.max_alloc = Some(MAX_ALLOC);
    l
}

/// Renders a thumbnail of untrusted image bytes (a stable error code on refusal).
pub fn render_thumbnail(
    bytes: &[u8],
    media_type: &str,
) -> std::result::Result<Thumbnail, &'static str> {
    let format = match media_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return Err("UNSUPPORTED_FORMAT"),
    };
    let reader = |b: &[u8]| {
        let mut r = image::ImageReader::with_format(Cursor::new(b.to_vec()), format);
        r.limits(limits());
        r
    };
    let (w, h) = reader(bytes).into_dimensions().map_err(|_| "UNDECODABLE")?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("TOO_MANY_PIXELS");
    }
    let img = reader(bytes).decode().map_err(|e| match e {
        image::ImageError::Limits(_) => "DECODE_LIMIT",
        _ => "UNDECODABLE",
    })?;
    let thumb = img.thumbnail(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE);
    let mut png = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|_| "ENCODE_FAILED")?;
    Ok(Thumbnail {
        width: thumb.width(),
        height: thumb.height(),
        source_width: w,
        source_height: h,
        png,
    })
}

/// The object key of one render attempt (unique per lease): a retry never overwrites an
/// object another attempt may still reference, and a stale attempt's object can be
/// deleted without touching the READY thumbnail.
pub fn attempt_key(environment_id: Uuid, attachment_id: Uuid, attempt: Uuid) -> String {
    format!(
        "drv/{environment_id}/thumbnail/{}/{}.png",
        attachment_id.simple(),
        attempt.simple()
    )
}

/// Prefix of every derivative object (orphan reconciliation lists it).
pub const OBJECT_PREFIX: &str = "drv/";
/// A thumbnail deferred for storage budget is retried after this long (or as soon as the
/// environment's attachment policy changes).
pub const QUOTA_RETRY: chrono::Duration = chrono::Duration::minutes(15);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DerivativeReport {
    pub enqueued: u64,
    pub ready: usize,
    pub failed: usize,
    pub retried: usize,
    /// Thumbnails not written because the environment's storage budget had no room.
    pub deferred: usize,
}

#[derive(FromQueryResult, Clone)]
struct Due {
    id: Uuid,
    attachment_id: Uuid,
    environment_id: Uuid,
    kind: String,
    attempts: i32,
}

/// One processing pass: enqueue READY MANAGED images without a thumbnail, then render
/// due ones under a lease (safe on several instances).
pub async fn process_due(state: &AppState, batch: i64) -> Result<DerivativeReport> {
    let mut report = DerivativeReport::default();
    if !state.attachments.media_processing || !state.storage.configured() {
        return Ok(report);
    }
    let now = state.now();
    report.enqueued = db::exec(
        &state.db,
        "INSERT INTO messaging_attachment_derivatives (id, attachment_id, environment_id, security_profile, kind, status, \
           next_attempt_at, created_at) \
         SELECT gen_random_uuid(), a.id, a.environment_id, 'MANAGED', 'THUMBNAIL', 'PENDING', $1, $1 \
           FROM messaging_attachments a \
          WHERE a.state = 'READY' AND a.security_profile = 'MANAGED' AND a.kind = 'IMAGE' AND a.media_type = ANY($2) \
            AND NOT EXISTS (SELECT 1 FROM messaging_attachment_derivatives d WHERE d.attachment_id = a.id AND d.kind = 'THUMBNAIL') \
          LIMIT $3 ON CONFLICT DO NOTHING",
        vec![
            now.into(),
            PROCESSABLE.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
            batch.into(),
        ],
    )
    .await?;
    report.enqueued += db::exec(
        &state.db,
        "INSERT INTO messaging_attachment_derivatives (id, attachment_id, environment_id, security_profile, kind, status, \
           next_attempt_at, created_at) \
         SELECT gen_random_uuid(), a.id, a.environment_id, 'MANAGED', 'INSPECTION', 'PENDING', $1, $1 \
           FROM messaging_attachments a \
          WHERE a.state = 'READY' AND a.security_profile = 'MANAGED' AND a.kind IN ('IMAGE', 'VOICE_NOTE', 'VIDEO') \
            AND a.media_type = ANY($2) \
            AND NOT EXISTS (SELECT 1 FROM messaging_attachment_derivatives d WHERE d.attachment_id = a.id AND d.kind = 'INSPECTION') \
          LIMIT $3 ON CONFLICT DO NOTHING",
        vec![
            now.into(),
            INSPECTABLE.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
            batch.into(),
        ],
    )
    .await?;
    let token = Uuid::new_v4();
    let due: Vec<Due> = db::all(
        &state.db,
        "WITH due AS (SELECT id FROM messaging_attachment_derivatives WHERE status IN ('PENDING', 'QUOTA_DEFERRED') \
             AND next_attempt_at <= $1 AND (lease_expires_at IS NULL OR lease_expires_at <= $1) \
             ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
         UPDATE messaging_attachment_derivatives d SET status = 'PENDING', lease_token = $3, lease_expires_at = $4 \
           FROM due WHERE d.id = due.id \
         RETURNING d.id, d.attachment_id, d.environment_id, d.kind, d.attempts",
        vec![
            now.into(),
            batch.into(),
            token.into(),
            (now + chrono::Duration::seconds(120)).into(),
        ],
    )
    .await?;
    for d in due {
        if d.kind == "INSPECTION" {
            match inspect_one(state, d.attachment_id).await {
                Ok(metadata) => {
                    let updated = db::exec(
                        &state.db,
                        "UPDATE messaging_attachment_derivatives SET status = 'READY', metadata = $3, error_code = NULL, \
                           attempts = attempts + 1, lease_token = NULL, lease_expires_at = NULL, completed_at = $4 \
                         WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                        vec![d.id.into(), token.into(), metadata.into(), state.now().into()],
                    )
                    .await?;
                    if updated == 1 {
                        report.ready += 1;
                        crate::metrics::inc(
                            "messaging_attachment_derivatives_total",
                            &[("outcome", "ready")],
                        );
                    }
                }
                Err(RenderError::Transient(code)) => {
                    retry_or_fail(state, &d, token, code, code, &mut report).await?;
                }
                Err(RenderError::Final(code)) => {
                    fail(state, &d, token, code).await?;
                    report.failed += 1;
                }
            }
            continue;
        }
        // An environment already at its budget gets no render at all (saves the decode).
        if budget_exhausted(state, d.environment_id).await? {
            if defer(state, &d, token).await? {
                report.deferred += 1;
            }
            continue;
        }
        match render_one(state, d.attachment_id).await {
            Ok(t) => {
                let key = attempt_key(d.environment_id, d.attachment_id, token);
                let digest = hex::encode(Sha256::digest(&t.png));
                let size = t.png.len() as i64;
                match reserve(state, &d, token, size, &key).await? {
                    Reservation::Granted => {}
                    Reservation::Deferred => {
                        report.deferred += 1;
                        continue;
                    }
                    Reservation::Lost => continue,
                }
                if let Err(e) = state.storage.put(&key, t.png, "image/png").await {
                    // A failed PUT has an unknown outcome: the reservation stays with this
                    // attempt; the retry or the final failure moves it to the deletion queue.
                    retry_or_fail(
                        state,
                        &d,
                        token,
                        "STORAGE_UNAVAILABLE",
                        &e.to_string(),
                        &mut report,
                    )
                    .await?;
                    continue;
                }
                // The trigger turns this attempt's reservation into committed bytes.
                let updated = db::exec(
                    &state.db,
                    "UPDATE messaging_attachment_derivatives SET status = 'READY', object_key = pending_object_key, \
                       pending_object_key = NULL, reserved_bytes = 0, media_type = 'image/png', \
                       width = $4, height = $5, source_width = $6, source_height = $7, bytes = $8, sha256 = $9, error_code = NULL, \
                       attempts = attempts + 1, lease_token = NULL, lease_expires_at = NULL, completed_at = $10 \
                     WHERE id = $1 AND lease_token = $2 AND status = 'PENDING' AND pending_object_key = $3",
                    vec![
                        d.id.into(),
                        token.into(),
                        key.clone().into(),
                        (t.width as i32).into(),
                        (t.height as i32).into(),
                        (t.source_width as i32).into(),
                        (t.source_height as i32).into(),
                        size.into(),
                        digest.into(),
                        state.now().into(),
                    ],
                )
                .await?;
                if updated == 0 {
                    // The attachment was deleted, or another worker took the lease over,
                    // while this attempt rendered. Its object is referenced by nothing:
                    // whoever ended the attempt moved its reservation to the deletion queue;
                    // queue the object again in case that deletion already ran.
                    super::lifecycle::enqueue_deletion(
                        &state.db,
                        &key,
                        d.environment_id,
                        Some(d.attachment_id),
                        "DERIVATIVE_SUPERSEDED",
                        state.now(),
                    )
                    .await?;
                    continue;
                }
                report.ready += 1;
                crate::metrics::inc(
                    "messaging_attachment_derivatives_total",
                    &[("outcome", "ready")],
                );
            }
            Err(RenderError::Transient(code)) => {
                retry_or_fail(state, &d, token, code, code, &mut report).await?;
            }
            Err(RenderError::Final(code)) => {
                fail(state, &d, token, code).await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// The environment's storage budget: the application policy's, bounded by the
/// deployment's `MESSAGING_STORAGE_BUDGET_BYTES` (the rule of [`super::quota::reserve`]).
async fn budget(state: &AppState, environment_id: Uuid) -> Result<Option<i64>> {
    let policy = crate::conversations::policy::current(&state.db, environment_id).await?;
    Ok(super::quota::effective_budget(
        &policy.attachments,
        state.attachments.storage_budget_bytes,
    ))
}

/// Advisory pre-check (no lock): the environment has no room left at all.
async fn budget_exhausted(state: &AppState, environment_id: Uuid) -> Result<bool> {
    let Some(b) = budget(state, environment_id).await? else {
        return Ok(false);
    };
    let used = db::scalar_i64(
        &state.db,
        "SELECT COALESCE((SELECT committed_bytes + reserved_bytes FROM messaging_storage_usage WHERE environment_id = $1), 0)",
        vec![environment_id.into()],
    )
    .await?;
    Ok(used >= b)
}

enum Reservation {
    Granted,
    Deferred,
    /// The attachment was deleted or another worker owns the derivative now.
    Lost,
}

#[derive(FromQueryResult)]
struct Attempt {
    reserved_bytes: i64,
    pending_object_key: Option<String>,
}

/// Locks this worker's derivative row (only while it still holds the lease).
async fn locked_attempt<C: sea_orm::ConnectionTrait>(
    txn: &C,
    d: &Due,
    token: Uuid,
) -> Result<Option<Attempt>> {
    Ok(db::one(
        txn,
        "SELECT reserved_bytes, pending_object_key FROM messaging_attachment_derivatives \
          WHERE id = $1 AND lease_token = $2 AND status = 'PENDING' FOR UPDATE",
        vec![d.id.into(), token.into()],
    )
    .await?)
}

/// A previous attempt's object (a crash after its upload, or a lost lease) may exist: it
/// is queued for deletion carrying its reservation as the charge, so it stays accounted
/// until it is really gone. The caller clears the row's reservation in the same transaction.
async fn retire_attempt<C: sea_orm::ConnectionTrait>(
    txn: &C,
    d: &Due,
    attempt: &Attempt,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    if let Some(key) = &attempt.pending_object_key {
        super::lifecycle::enqueue_charged_deletion(
            txn,
            key,
            d.environment_id,
            Some(d.attachment_id),
            reason,
            attempt.reserved_bytes,
            now,
        )
        .await?;
    }
    Ok(())
}

/// Reserves `size` bytes for this attempt under the environment's usage row lock (the
/// same lock upload intents take, so concurrent workers and uploads cannot overshoot).
async fn reserve(
    state: &AppState,
    d: &Due,
    token: Uuid,
    size: i64,
    key: &str,
) -> Result<Reservation> {
    let budget = budget(state, d.environment_id).await?;
    let st = state.clone();
    let d = d.clone();
    let key = key.to_string();
    state
        .db
        .transaction::<_, Reservation, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                #[derive(FromQueryResult)]
                struct Usage {
                    committed_bytes: i64,
                    reserved_bytes: i64,
                }
                // Lock order: the derivative row, then the usage row (the order of the
                // accounting triggers and of attachment deletion: no deadlock).
                let Some(attempt) = locked_attempt(txn, &d, token).await? else {
                    return Ok(Reservation::Lost);
                };
                let usage: Usage = db::one(
                    txn,
                    "SELECT committed_bytes, reserved_bytes FROM messaging_storage_usage WHERE environment_id = $1 FOR UPDATE",
                    vec![d.environment_id.into()],
                )
                .await?
                .ok_or(Error::code(ErrorCode::InternalError))?;
                // A previous attempt's reservation is still part of `reserved_bytes`: its
                // object may exist, so it is not subtracted (it moves to the deletion queue).
                let fits = budget
                    .is_none_or(|b| usage.committed_bytes + usage.reserved_bytes + size <= b);
                retire_attempt(txn, &d, &attempt, "DERIVATIVE_RETRIED", now).await?;
                if !fits {
                    mark_deferred(txn, &d, token, now).await?;
                    return Ok(Reservation::Deferred);
                }
                db::exec(
                    txn,
                    "UPDATE messaging_attachment_derivatives SET reserved_bytes = $3, pending_object_key = $4 \
                     WHERE id = $1 AND lease_token = $2",
                    vec![d.id.into(), token.into(), size.into(), key.into()],
                )
                .await?;
                Ok(Reservation::Granted)
            })
        })
        .await
        .map_err(Error::from)
}

async fn mark_deferred<C: sea_orm::ConnectionTrait>(
    txn: &C,
    d: &Due,
    token: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        txn,
        "UPDATE messaging_attachment_derivatives SET status = 'QUOTA_DEFERRED', error_code = 'STORAGE_BUDGET', \
           reserved_bytes = 0, pending_object_key = NULL, next_attempt_at = $3, lease_token = NULL, lease_expires_at = NULL \
         WHERE id = $1 AND lease_token = $2",
        vec![d.id.into(), token.into(), (now + QUOTA_RETRY).into()],
    )
    .await?;
    crate::metrics::inc(
        "messaging_attachment_derivatives_total",
        &[("outcome", "quota_deferred")],
    );
    Ok(())
}

/// Defers this derivative for storage budget (retiring any previous attempt's object).
async fn defer(state: &AppState, d: &Due, token: Uuid) -> Result<bool> {
    let st = state.clone();
    let d = d.clone();
    state
        .db
        .transaction::<_, bool, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let Some(attempt) = locked_attempt(txn, &d, token).await? else {
                    return Ok(false);
                };
                retire_attempt(txn, &d, &attempt, "DERIVATIVE_DEFERRED", now).await?;
                mark_deferred(txn, &d, token, now).await?;
                Ok(true)
            })
        })
        .await
        .map_err(Error::from)
}

/// Makes the environment's QUOTA_DEFERRED thumbnails due now (its budget changed).
pub async fn requeue_quota_deferred<C: sea_orm::ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<u64> {
    Ok(db::exec(
        db,
        "UPDATE messaging_attachment_derivatives SET next_attempt_at = $2 \
         WHERE environment_id = $1 AND status = 'QUOTA_DEFERRED' AND next_attempt_at > $2",
        vec![environment_id.into(), now.into()],
    )
    .await?)
}

/// Re-renders a READY thumbnail (an operator action, e.g. after a rendering change). The
/// current object stays charged in the deletion queue until it is removed; the new render
/// is reserved and charged on its own, so usage never drops below what is stored.
pub async fn regenerate_thumbnail<C: sea_orm::ConnectionTrait>(
    db: &C,
    attachment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    #[derive(FromQueryResult)]
    struct Old {
        environment_id: Uuid,
        object_key: String,
        bytes: i64,
    }
    let old: Option<Old> = db::one(
        db,
        "WITH old AS (SELECT id, environment_id, object_key, bytes FROM messaging_attachment_derivatives \
                       WHERE attachment_id = $1 AND kind = 'THUMBNAIL' AND status = 'READY' FOR UPDATE) \
         UPDATE messaging_attachment_derivatives d SET status = 'PENDING', object_key = NULL, media_type = NULL, width = NULL, \
           height = NULL, source_width = NULL, source_height = NULL, bytes = NULL, sha256 = NULL, error_code = NULL, attempts = 0, \
           next_attempt_at = $2, completed_at = NULL \
           FROM old WHERE d.id = old.id RETURNING old.environment_id, old.object_key, old.bytes",
        vec![attachment_id.into(), now.into()],
    )
    .await?;
    let Some(old) = old else { return Ok(false) };
    super::lifecycle::enqueue_charged_deletion(
        db,
        &old.object_key,
        old.environment_id,
        Some(attachment_id),
        "DERIVATIVE_REPLACED",
        old.bytes,
        now,
    )
    .await?;
    Ok(true)
}

enum RenderError {
    Transient(&'static str),
    Final(&'static str),
}

async fn render_one(
    state: &AppState,
    attachment_id: Uuid,
) -> std::result::Result<Thumbnail, RenderError> {
    let row: Option<AttachmentRow> = db::one(
        &state.db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE id = $1"),
        vec![attachment_id.into()],
    )
    .await
    .map_err(|_| RenderError::Transient("DATABASE_UNAVAILABLE"))?;
    let row = row.ok_or(RenderError::Final("ATTACHMENT_GONE"))?;
    // The only gate: MANAGED and READY (never PRIVATE_E2EE ciphertext).
    let req = processing::eligible(&row, processing::Derivative::Thumbnail)
        .map_err(|_| RenderError::Final("NOT_ELIGIBLE"))?;
    let bytes = match state.storage.get(&req.object_key, MAX_SOURCE_BYTES).await {
        Ok(Some(b)) => b,
        Ok(None) => return Err(RenderError::Final("OBJECT_MISSING")),
        Err(cf_object_storage::StorageError::Integrity) => {
            return Err(RenderError::Final("SOURCE_TOO_LARGE"))
        }
        Err(_) => return Err(RenderError::Transient("STORAGE_UNAVAILABLE")),
    };
    // The stored bytes must be exactly what was verified at finalization.
    if hex::encode(Sha256::digest(&bytes)) != row.declared_sha256.to_ascii_lowercase() {
        return Err(RenderError::Final("INTEGRITY_MISMATCH"));
    }
    let media_type = req.media_type.clone();
    let job = tokio::task::spawn_blocking(move || render_thumbnail(&bytes, &media_type));
    match tokio::time::timeout(DECODE_TIMEOUT, job).await {
        Ok(Ok(Ok(t))) => Ok(t),
        Ok(Ok(Err(code))) => Err(RenderError::Final(code)),
        Ok(Err(_)) => Err(RenderError::Final("DECODER_PANICKED")),
        Err(_) => Err(RenderError::Final("DECODE_TIMEOUT")),
    }
}

/// Media types inspected server-side (MANAGED only).
pub const INSPECTABLE: &[&str] = &[
    "image/png",
    "image/jpeg",
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "audio/mp4",
    "audio/webm",
    "audio/ogg",
    "audio/wav",
    "audio/mpeg",
    "audio/aac",
    "audio/amr",
];
/// Largest object read for inspection (larger: `SOURCE_TOO_LARGE`, never a guess).
pub const MAX_INSPECT_BYTES: usize = 64 * 1024 * 1024;

/// Reads a READY MANAGED attachment and measures it ([`super::media_inspect`]). The
/// result compares the measured duration with the client's declaration.
async fn inspect_one(
    state: &AppState,
    attachment_id: Uuid,
) -> std::result::Result<Value, RenderError> {
    let row: Option<AttachmentRow> = db::one(
        &state.db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE id = $1"),
        vec![attachment_id.into()],
    )
    .await
    .map_err(|_| RenderError::Transient("DATABASE_UNAVAILABLE"))?;
    let row = row.ok_or(RenderError::Final("ATTACHMENT_GONE"))?;
    let req = processing::eligible(&row, processing::Derivative::Inspection)
        .map_err(|_| RenderError::Final("NOT_ELIGIBLE"))?;
    let bytes = match state.storage.get(&req.object_key, MAX_INSPECT_BYTES).await {
        Ok(Some(b)) => b,
        Ok(None) => return Err(RenderError::Final("OBJECT_MISSING")),
        Err(cf_object_storage::StorageError::Integrity) => {
            return Err(RenderError::Final("SOURCE_TOO_LARGE"))
        }
        Err(_) => return Err(RenderError::Transient("STORAGE_UNAVAILABLE")),
    };
    if hex::encode(Sha256::digest(&bytes)) != row.declared_sha256.to_ascii_lowercase() {
        return Err(RenderError::Final("INTEGRITY_MISMATCH"));
    }
    let media_type = req.media_type.clone();
    let job =
        tokio::task::spawn_blocking(move || super::media_inspect::inspect(&bytes, &media_type));
    let facts = match tokio::time::timeout(DECODE_TIMEOUT, job).await {
        Ok(Ok(Ok(f))) => f,
        Ok(Ok(Err(code))) => return Err(RenderError::Final(code)),
        Ok(Err(_)) => return Err(RenderError::Final("DECODER_PANICKED")),
        Err(_) => return Err(RenderError::Final("DECODE_TIMEOUT")),
    };
    let declared = row
        .media_metadata
        .as_ref()
        .and_then(|m| m.get("durationMs"))
        .and_then(Value::as_i64);
    let matches = match (declared, facts.duration_ms) {
        (Some(d), Some(m)) => Some(super::media_inspect::duration_matches(d, m)),
        _ => None,
    };
    if matches == Some(false) {
        crate::metrics::inc("messaging_attachment_duration_mismatch_total", &[]);
    }
    Ok(json!({
        "container": facts.container,
        "durationMs": facts.duration_ms,
        "width": facts.width,
        "height": facts.height,
        "hasVideo": facts.has_video,
        "hasAudio": facts.has_audio,
        "declaredDurationMs": declared,
        "durationMatchesDeclared": matches,
    }))
}

async fn fail(state: &AppState, d: &Due, token: Uuid, code: &str) -> Result<()> {
    let st = state.clone();
    let d = d.clone();
    let code = code.to_string();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let Some(attempt) = locked_attempt(txn, &d, token).await? else {
                    return Ok(());
                };
                // A failed attempt may have left an object: it is deleted, still charged.
                retire_attempt(txn, &d, &attempt, "DERIVATIVE_FAILED", now).await?;
                db::exec(
                    txn,
                    "UPDATE messaging_attachment_derivatives SET status = 'FAILED', error_code = $3, attempts = attempts + 1, \
                       reserved_bytes = 0, pending_object_key = NULL, lease_token = NULL, lease_expires_at = NULL, completed_at = $4 \
                     WHERE id = $1 AND lease_token = $2",
                    vec![d.id.into(), token.into(), code.into(), now.into()],
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    crate::metrics::inc(
        "messaging_attachment_derivatives_total",
        &[("outcome", "failed")],
    );
    Ok(())
}

async fn retry_or_fail(
    state: &AppState,
    d: &Due,
    token: Uuid,
    code: &'static str,
    detail: &str,
    report: &mut DerivativeReport,
) -> Result<()> {
    if d.attempts + 1 >= MAX_ATTEMPTS {
        report.failed += 1;
        return fail(state, d, token, code).await;
    }
    tracing::warn!(error = %bounded_error(detail), "derivative rendering will be retried");
    db::exec(
        &state.db,
        "UPDATE messaging_attachment_derivatives SET attempts = attempts + 1, next_attempt_at = $3, error_code = $4, \
           lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND lease_token = $2",
        vec![
            d.id.into(),
            token.into(),
            (state.now() + chrono::Duration::seconds(30 * i64::from(d.attempts + 1))).into(),
            code.into(),
        ],
    )
    .await?;
    report.retried += 1;
    Ok(())
}

#[derive(FromQueryResult)]
struct DerivativeRow {
    kind: String,
    status: String,
    object_key: Option<String>,
    media_type: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    source_width: Option<i32>,
    source_height: Option<i32>,
    bytes: Option<i64>,
    sha256: Option<String>,
    error_code: Option<String>,
    metadata: Option<Value>,
    completed_at: Option<DateTime<Utc>>,
}

async fn rows(state: &AppState, attachment_id: Uuid) -> Result<Vec<DerivativeRow>> {
    Ok(db::all(
        &state.db,
        "SELECT kind, status, object_key, media_type, width, height, source_width, source_height, bytes, sha256, error_code, metadata, completed_at \
           FROM messaging_attachment_derivatives WHERE attachment_id = $1 AND status <> 'DELETED' ORDER BY kind",
        vec![attachment_id.into()],
    )
    .await?)
}

/// The server-made derivatives of one attachment (empty for PRIVATE_E2EE).
pub async fn views(state: &AppState, attachment_id: Uuid) -> Result<Value> {
    Ok(Value::Array(
        rows(state, attachment_id)
            .await?
            .iter()
            .map(|d| {
                json!({
                    "kind": d.kind,
                    "status": d.status,
                    "mediaType": d.media_type,
                    "width": d.width,
                    "height": d.height,
                    "sourceWidth": d.source_width,
                    "sourceHeight": d.source_height,
                    "size": d.bytes,
                    "sha256": d.sha256,
                    "errorCode": d.error_code,
                    "metadata": d.metadata,
                    "completedAt": d.completed_at.map(rfc3339),
                })
            })
            .collect(),
    ))
}

/// `POST /v1/conversations/{id}/attachments/{attachmentId}/thumbnail-grants`: the same
/// authorization as a download grant of the original.
pub async fn thumbnail_grant(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<Value> {
    super::require_storage(state)?;
    let row = super::authorize_read(state, ctx, acting, conversation_id, attachment_id).await?;
    let d = rows(state, row.id)
        .await?
        .into_iter()
        .find(|d| d.kind == "THUMBNAIL" && d.status == "READY")
        .ok_or(Error::reason(
            ErrorCode::AttachmentNotReady,
            "THUMBNAIL_NOT_READY",
        ))?;
    let key = d.object_key.ok_or(Error::code(ErrorCode::InternalError))?;
    let now = state.now();
    let signed = state
        .storage
        .signed_read_as(
            &key,
            state.attachments.grant_ttl,
            now,
            &cf_object_storage::ResponseOverrides {
                content_type: Some("image/png".into()),
                content_disposition: Some("inline; filename=\"thumbnail.png\"".into()),
            },
        )
        .map_err(super::storage_error)?;
    Ok(json!({
        "attachmentId": row.id,
        "kind": "THUMBNAIL",
        "method": "GET",
        "url": signed.url,
        "expiresAt": rfc3339(signed.expires_at),
        "mediaType": "image/png",
        "size": d.bytes,
        "sha256": d.sha256,
        "width": d.width,
        "height": d.height,
        "residualAccessSeconds": state.attachments.grant_ttl.as_secs(),
    }))
}

/// Queues the deletion of an attachment's derivative objects (inside the attachment's
/// deletion transaction); the deletion worker honours legal holds on the attachment. The
/// derivative's bytes (a READY thumbnail, or an attempt's reservation) move with each
/// object into the deletion queue and stay charged until the object is really removed.
pub async fn delete_for_attachment<C: sea_orm::ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    if row.security_profile != "MANAGED" {
        return Ok(());
    }
    #[derive(FromQueryResult)]
    struct Old {
        previous: String,
        kind: String,
        object_key: Option<String>,
        bytes: Option<i64>,
        pending_object_key: Option<String>,
        reserved_bytes: i64,
    }
    let ended: Vec<Old> = db::all(
        db,
        "WITH old AS (SELECT id, status, object_key, bytes, pending_object_key, reserved_bytes FROM messaging_attachment_derivatives \
                       WHERE attachment_id = $1 AND status <> 'DELETED' FOR UPDATE) \
         UPDATE messaging_attachment_derivatives d SET status = 'DELETED', object_key = NULL, metadata = NULL, \
           pending_object_key = NULL, reserved_bytes = 0, lease_token = NULL, lease_expires_at = NULL, completed_at = $2 \
           FROM old WHERE d.id = old.id \
         RETURNING old.status AS previous, d.kind, old.object_key, old.bytes, old.pending_object_key, old.reserved_bytes",
        vec![row.id.into(), now.into()],
    )
    .await?;
    for o in ended.iter().filter(|o| o.kind == "THUMBNAIL") {
        if let (true, Some(key)) = (o.previous == "READY", &o.object_key) {
            super::lifecycle::enqueue_charged_deletion(
                db,
                key,
                row.environment_id,
                Some(row.id),
                reason,
                o.bytes.unwrap_or(0),
                now,
            )
            .await?;
        }
        // An in-flight or crashed attempt (an in-flight render also notices the deletion
        // and queues its own output again: `process_due`).
        if let Some(key) = &o.pending_object_key {
            super::lifecycle::enqueue_charged_deletion(
                db,
                key,
                row.environment_id,
                Some(row.id),
                reason,
                o.reserved_bytes,
                now,
            )
            .await?;
        }
    }
    Ok(())
}

/// Every object key a live derivative references (orphan reconciliation).
pub async fn referenced_keys<C: sea_orm::ConnectionTrait>(
    db: &C,
    keys: &[String],
) -> Result<Vec<String>> {
    #[derive(FromQueryResult)]
    struct K {
        k: String,
    }
    let rows: Vec<K> = db::all(
        db,
        "SELECT object_key AS k FROM messaging_attachment_derivatives WHERE object_key = ANY($1) \
         UNION SELECT pending_object_key AS k FROM messaging_attachment_derivatives WHERE pending_object_key = ANY($1) \
         UNION SELECT object_key AS k FROM messaging_object_deletions WHERE object_key = ANY($1) AND status IN ('PENDING', 'HELD', 'DEAD')",
        vec![keys.to_vec().into()],
    )
    .await?;
    Ok(rows.into_iter().map(|r| r.k).collect())
}

//! User reports (§85).

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry},
    conversations::{
        access,
        policy::{self, EffectivePolicy},
        service::{request_context, rfc3339, ConversationRow},
        ContextClass,
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    messages::{managed, MessageRow, MESSAGE_COLUMNS},
    state::AppState,
};

pub const CATEGORIES: &[&str] = &[
    "SPAM",
    "HARASSMENT",
    "HATE",
    "VIOLENCE",
    "SEXUAL_CONTENT",
    "CHILD_SAFETY",
    "SELF_HARM",
    "ILLEGAL_CONTENT",
    "IMPERSONATION",
    "OTHER",
];
pub const MAX_MESSAGES: usize = 20;
pub const MAX_NOTE_CHARS: usize = 2000;
pub const MAX_DISCLOSED_CHARS: usize = 4000;
const EVIDENCE_LABEL: &str = "cfmeet-messaging-report-evidence:v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Disclosure {
    pub message_id: Uuid,
    /// What the reporter's own device decrypted (voluntarily disclosed).
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportRequest {
    pub category: String,
    #[serde(default)]
    pub message_ids: Vec<Uuid>,
    #[serde(default)]
    pub reported_principal_id: Option<Uuid>,
    #[serde(default)]
    pub note: Option<String>,
    /// PRIVATE_E2EE only, optional: the reporter's voluntary disclosure.
    #[serde(default)]
    pub disclosure: Vec<Disclosure>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Evidence {
    note: Option<String>,
    disclosure: Vec<Disclosure>,
}

fn evidence_aad(ctx_tenant: Uuid, app: Uuid, env: Uuid, report: Uuid) -> Vec<u8> {
    format!("{EVIDENCE_LABEL}:{ctx_tenant}:{app}:{env}:{report}").into_bytes()
}

fn reporting_enabled(policy: &EffectivePolicy, conversation: &ConversationRow) -> bool {
    let context = ContextClass::parse(&conversation.context_class)
        .and_then(|c| policy.contexts.get(&c))
        .map(|c| c.reporting_enabled)
        .unwrap_or(true);
    policy.moderation.enabled && policy.moderation.reports_enabled && context
}

/// `POST /v1/conversations/{id}/reports` (acting member).
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: ReportRequest,
) -> Result<Value> {
    let now = state.now();
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    if !reporting_enabled(&effective, &conversation) {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "REPORTING_DISABLED",
        ));
    }
    if !CATEGORIES.contains(&request.category.as_str()) {
        return Err(Error::validation(format!(
            "category must be one of {}",
            CATEGORIES.join(", ")
        )));
    }
    let mut ids = request.message_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() != request.message_ids.len() || ids.len() > MAX_MESSAGES {
        return Err(Error::validation(format!(
            "messageIds: at most {MAX_MESSAGES} distinct ids"
        )));
    }
    if request
        .note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(Error::validation(format!(
            "note: at most {MAX_NOTE_CHARS} characters"
        )));
    }
    if request.reported_principal_id == Some(acting.principal_id) {
        return Err(Error::validation("a principal cannot report themselves"));
    }
    if let Some(p) = request.reported_principal_id {
        // Any member, current or former, of THIS conversation; nothing else (no oracle).
        if access::membership(&state.db, conversation.id, p, false)
            .await?
            .is_none()
        {
            return Err(Error::code(ErrorCode::NotFound));
        }
    }
    if !ids.is_empty() {
        let visible = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_messages WHERE conversation_id = $1 AND id = ANY($2) AND sequence >= $3 \
               AND message_kind <> 'MLS_COMMIT'",
            vec![conversation.id.into(), ids.clone().into(), member.visible_from_sequence.into()],
        )
        .await?;
        if visible as usize != ids.len() {
            return Err(Error::code(ErrorCode::NotFound));
        }
    }
    let profile = conversation.profile();
    let evidence_kind = match profile {
        SecurityProfile::Managed => {
            if !request.disclosure.is_empty() {
                return Err(Error::validation(
                    "disclosure applies to PRIVATE_E2EE conversations; MANAGED reports reference messageIds",
                ));
            }
            if ids.is_empty() {
                "NONE"
            } else {
                "MANAGED_REFERENCE"
            }
        }
        SecurityProfile::PrivateE2ee => {
            if request.disclosure.len() > MAX_MESSAGES
                || request.disclosure.iter().any(|d| {
                    !ids.contains(&d.message_id) || d.text.chars().count() > MAX_DISCLOSED_CHARS
                })
            {
                return Err(Error::validation(format!(
                    "disclosure: one entry per reported message (in messageIds), at most {MAX_DISCLOSED_CHARS} characters each"
                )));
            }
            if request.disclosure.is_empty() {
                "NONE"
            } else {
                "REPORTER_DISCLOSURE"
            }
        }
    };
    let key = format!(
        "reports:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(
        &state.db,
        &key,
        Window::Hour,
        effective.abuse.reports_per_hour,
        now,
    )
    .await?
    {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let id = Uuid::new_v4();
    let sealed = if request.note.is_some() || !request.disclosure.is_empty() {
        let ring = state.managed_keys.as_ref().ok_or(Error::reason(
            ErrorCode::FeatureDisabled,
            "EVIDENCE_SEALING_NOT_CONFIGURED",
        ))?;
        let evidence = Evidence {
            note: request.note.clone(),
            disclosure: request.disclosure.clone(),
        };
        let bytes =
            serde_json::to_vec(&evidence).map_err(|_| Error::code(ErrorCode::InternalError))?;
        Some(
            ring.seal_record(
                &state.db,
                &crate::messages::keys::KeyScope {
                    tenant_id: ctx.scope.tenant_id,
                    application_id: ctx.scope.application_id,
                    environment_id: ctx.scope.environment_id,
                },
                &evidence_aad(
                    ctx.scope.tenant_id,
                    ctx.scope.application_id,
                    ctx.scope.environment_id,
                    id,
                ),
                &bytes,
            )
            .await?,
        )
    } else {
        None
    };
    let (sealed_bytes, nonce, key_id) = match sealed {
        Some((s, n, k)) => (Some(s), Some(n), Some(k)),
        None => (None, None, None),
    };
    let txn = sea_orm::TransactionTrait::begin(&state.db).await?;
    db::exec(
        &txn,
        "INSERT INTO messaging_reports (id, tenant_id, application_id, environment_id, conversation_id, security_profile, \
           reporter_principal_id, reported_principal_id, message_ids, category, evidence_kind, sealed_evidence, \
           evidence_nonce, evidence_key_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            conversation.id.into(),
            profile.as_str().into(),
            acting.principal_id.into(),
            request.reported_principal_id.into(),
            ids.clone().into(),
            request.category.clone().into(),
            evidence_kind.into(),
            sealed_bytes.into(),
            nonce.into(),
            key_id.into(),
            now.into(),
        ],
    )
    .await?;
    // §118: message.reported (ids and codes only; the reporter is not disclosed).
    crate::events::outbox::record(
        &txn,
        "messaging.report.created",
        "REPORT",
        id,
        crate::conversations::service::scope_of(ctx),
        json!({ "report_id": id, "conversation_id": conversation.id, "security_profile": profile.as_str(),
                "category": request.category, "evidence_kind": evidence_kind, "message_count": ids.len() }),
        now,
    )
    .await?;
    txn.commit().await?;
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: Some(acting.principal_id),
            action: "REPORT_CREATED",
            target_kind: "REPORT",
            target_id: id,
            detail: json!({ "conversation_id": conversation.id, "category": request.category, "evidence_kind": evidence_kind,
                            "message_count": ids.len() }),
        },
        &request_context(),
        now,
    )
    .await?;
    crate::metrics::inc("messaging_reports_total", &[("evidence", evidence_kind)]);
    // The reporter learns only that the report was received.
    Ok(
        json!({ "reportId": id, "status": "OPEN", "category": request.category, "evidenceKind": evidence_kind, "createdAt": rfc3339(now) }),
    )
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ReportRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub security_profile: String,
    pub reporter_principal_id: Uuid,
    pub reported_principal_id: Option<Uuid>,
    pub message_ids: Vec<Uuid>,
    pub category: String,
    pub evidence_kind: String,
    pub sealed_evidence: Option<Vec<u8>>,
    pub evidence_nonce: Option<Vec<u8>>,
    pub evidence_key_id: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, tenant_id, application_id, environment_id, conversation_id, security_profile, reporter_principal_id, \
     reported_principal_id, message_ids, category, evidence_kind, sealed_evidence, evidence_nonce, evidence_key_id, status, \
     resolution, resolved_at, created_at";

fn summary(r: &ReportRow) -> Value {
    json!({
        "reportId": r.id,
        "conversationId": r.conversation_id,
        "securityProfile": r.security_profile,
        "reporterPrincipalId": r.reporter_principal_id,
        "reportedPrincipalId": r.reported_principal_id,
        "messageIds": r.message_ids,
        "category": r.category,
        "evidenceKind": r.evidence_kind,
        "status": r.status,
        "resolution": r.resolution,
        "resolvedAt": r.resolved_at.map(rfc3339),
        "createdAt": rfc3339(r.created_at),
    })
}

pub async fn load<C: sea_orm::ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    id: Uuid,
    lock: bool,
) -> Result<Option<ReportRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_reports WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into(), ctx.scope.tenant_id.into(), ctx.scope.application_id.into(), ctx.scope.environment_id.into()],
    )
    .await?)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /v1/messaging/reports` — the moderation queue (no evidence in the listing).
pub async fn list(state: &AppState, ctx: &TrustedContext, q: ListQuery) -> Result<Value> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 200"));
    }
    if q.status
        .as_deref()
        .is_some_and(|s| !matches!(s, "OPEN" | "ACTIONED" | "DISMISSED"))
    {
        return Err(Error::validation(
            "status must be OPEN, ACTIONED or DISMISSED",
        ));
    }
    let rows: Vec<ReportRow> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_reports WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND ($4::varchar IS NULL OR status = $4) ORDER BY created_at, id LIMIT $5"
        ),
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            q.status.into(),
            limit.into(),
        ],
    )
    .await?;
    Ok(json!({ "reports": rows.iter().map(summary).collect::<Vec<_>>() }))
}

/// `GET /v1/messaging/reports/{id}` — one report with its evidence (audited).
pub async fn get(state: &AppState, ctx: &TrustedContext, id: Uuid) -> Result<Value> {
    let r = load(&state.db, ctx, id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    let rows: Vec<MessageRow> = db::all(
        &state.db,
        &format!("SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 AND m.id = ANY($2) ORDER BY m.sequence"),
        vec![r.conversation_id.into(), r.message_ids.clone().into()],
    )
    .await?;
    let evidence: Option<Evidence> = match (
        &r.sealed_evidence,
        &r.evidence_nonce,
        &r.evidence_key_id,
        state.managed_keys.as_ref(),
    ) {
        (Some(s), Some(n), Some(k), Some(ring)) => ring
            .open_record(
                &state.db,
                &evidence_aad(r.tenant_id, r.application_id, r.environment_id, r.id),
                s,
                n,
                k,
                crate::messages::keys::DecryptPurpose::ReportEvidence,
            )
            .await
            .and_then(|b| serde_json::from_slice(&b).ok()),
        _ => None,
    };
    let show_managed = effective.moderation.managed_evidence_allowed;
    let mut managed_texts = Vec::with_capacity(rows.len());
    for m in &rows {
        managed_texts.push(if show_managed {
            managed::stored_text_of(state, m).await
        } else {
            None
        });
    }
    let messages: Vec<Value> = rows
        .iter()
        .zip(managed_texts)
        .map(|(m, managed_text)| {
            let disclosed = evidence
                .as_ref()
                .and_then(|e| e.disclosure.iter().find(|d| d.message_id == m.id))
                .map(|d| d.text.clone());
            let mut v = json!({
                "messageId": m.id,
                "sequence": m.sequence,
                "senderPrincipalId": m.sender_principal_id,
                "senderDeviceId": m.sender_device_id,
                "lifecycle": m.lifecycle,
                "createdAt": rfc3339(m.created_at),
            });
            match m.profile() {
                SecurityProfile::PrivateE2ee => {
                    v["e2ee"] = json!({
                        "ciphertextSha256": m.ciphertext_sha256,
                        "acceptanceDigest": m.acceptance_digest,
                        "reporterDisclosure": disclosed,
                        // The server cannot check a disclosure against MLS ciphertext.
                        "verification": if disclosed.is_some() { "REPORTER_ATTESTED_UNVERIFIABLE" } else { "NO_PLAINTEXT_AVAILABLE" },
                    });
                }
                SecurityProfile::Managed => {
                    v["managed"] = json!({
                        "text": managed_text,
                        "visibility": if show_managed { "POLICY_ALLOWS_MODERATOR_ACCESS" } else { "POLICY_WITHHOLDS_CONTENT" },
                    });
                }
            }
            v
        })
        .collect();
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: None,
            action: "REPORT_VIEWED",
            target_kind: "REPORT",
            target_id: r.id,
            detail: json!({ "evidence_kind": r.evidence_kind, "managed_shown": show_managed && r.security_profile == "MANAGED" }),
        },
        &request_context(),
        state.now(),
    )
    .await?;
    let mut v = summary(&r);
    v["note"] = json!(evidence.as_ref().and_then(|e| e.note.clone()));
    v["messages"] = json!(messages);
    v["decryptionCapability"] = json!("NONE");
    Ok(v)
}

/// Marks an OPEN report resolved inside the caller's transaction.
pub async fn resolve<C: sea_orm::ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    report: Uuid,
    status: &str,
    resolution: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let n = db::exec(
        db,
        "UPDATE messaging_reports SET status = $2, resolution = $3, resolved_at = $4, resolved_by_credential_id = $5 \
         WHERE id = $1 AND status = 'OPEN' AND environment_id = $6",
        vec![
            report.into(),
            status.into(),
            resolution.into(),
            now.into(),
            ctx.credential_id.into(),
            ctx.scope.environment_id.into(),
        ],
    )
    .await?;
    if n != 1 {
        return Err(Error::reason(ErrorCode::Conflict, "REPORT_NOT_OPEN"));
    }
    Ok(())
}

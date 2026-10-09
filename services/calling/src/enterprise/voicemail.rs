//! Voicemail (Part F §60, §100–§113): mailboxes and messages as administrators and
//! mailbox owners see them.
//!
//! * A mailbox belongs to a principal, an extension, a queue or a ring group of the scope.
//! * A message belongs to ONE originating canonical Call. Its audio is a protected
//!   artifact: the API returns integrity metadata (content type, size, duration, SHA-256)
//!   but never an object key or a stored URL. Media is reached only through
//!   [`EnterpriseService::voicemail_media_access`]: the requester is authorized FIRST
//!   (mailbox owner or voicemail administrator, in scope — another scope's message is
//!   404), then a short-lived signed read is minted and the access is audited. Knowing a
//!   message id or an object key is never enough.
//! * Deletion is asynchronous and idempotent: DELETING (with audit) → the worker removes
//!   the object → DELETED. The originating Call's history is never touched.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    destinations::{self, Destination, DestinationType},
    exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

/// Signed media reads live this long (bounded further by the store).
pub const MEDIA_ACCESS_TTL: Duration = Duration::from_secs(300);

text_enum!(
    /// What a mailbox belongs to.
    MailboxOwnerType {
        Principal = "PRINCIPAL",
        Extension = "EXTENSION",
        Queue = "QUEUE",
        RingGroup = "RING_GROUP",
    }
);

impl MailboxOwnerType {
    pub fn destination_type(self) -> DestinationType {
        match self {
            Self::Principal => DestinationType::Principal,
            Self::Extension => DestinationType::Extension,
            Self::Queue => DestinationType::Queue,
            Self::RingGroup => DestinationType::RingGroup,
        }
    }
}

text_enum!(
    /// Processing state of a message.
    MessageState {
        Recording = "RECORDING",
        Processing = "PROCESSING",
        Ready = "READY",
        Failed = "FAILED",
        Deleting = "DELETING",
        Deleted = "DELETED",
    }
);

text_enum!(
    /// Read state of a message.
    ReadState {
        Unread = "UNREAD",
        Read = "READ",
    }
);

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct MailboxRow {
    pub id: Uuid,
    pub owner_type: String,
    pub owner_id: Uuid,
    pub display_label: String,
    pub max_message_seconds: i32,
    pub retention_days: i32,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

const MAILBOX_COLUMNS: &str = "id, owner_type, owner_id, display_label, max_message_seconds, \
    retention_days, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxOwner {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxView {
    pub mailbox_id: Uuid,
    pub owner: MailboxOwner,
    pub display_label: String,
    pub max_message_seconds: i32,
    pub retention_days: i32,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxPage {
    pub mailboxes: Vec<MailboxView>,
    pub next_cursor: Option<String>,
}

fn mailbox_view(row: MailboxRow) -> MailboxView {
    MailboxView {
        mailbox_id: row.id,
        owner: MailboxOwner {
            kind: row.owner_type,
            id: row.owner_id,
        },
        display_label: row.display_label,
        max_message_seconds: row.max_message_seconds,
        retention_days: row.retention_days,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct MessageRow {
    pub id: Uuid,
    pub mailbox_id: Uuid,
    pub call_id: Uuid,
    pub caller_principal_id: Uuid,
    pub capture_source: String,
    pub state: String,
    pub read_state: String,
    pub object_key: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_ms: Option<i32>,
    pub sha256: Option<String>,
    pub received_at: DateTime<Utc>,
    pub recording_deadline: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub retention_until: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub delete_reason: Option<String>,
    pub delete_attempts: i32,
    pub version: i64,
}

pub(crate) const MESSAGE_COLUMNS: &str = "id, mailbox_id, call_id, caller_principal_id, \
    capture_source, state, read_state, object_key, content_type, size_bytes, duration_ms, sha256, \
    received_at, recording_deadline, ready_at, read_at, failure_code, retention_until, deleted_at, \
    delete_reason, delete_attempts, version";

/// A message as its owner / an administrator sees it: integrity metadata, never the
/// object key or a URL.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicemailMessageView {
    pub message_id: Uuid,
    pub mailbox_id: Uuid,
    pub call_id: Uuid,
    pub caller_principal_id: Uuid,
    pub capture_source: String,
    pub state: String,
    pub read_state: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_ms: Option<i32>,
    pub sha256: Option<String>,
    pub received_at: String,
    pub ready_at: Option<String>,
    pub read_at: Option<String>,
    pub failure_code: Option<String>,
    pub retention_until: Option<String>,
    pub deleted_at: Option<String>,
    pub version: i64,
}

pub(crate) fn message_view(row: MessageRow) -> VoicemailMessageView {
    VoicemailMessageView {
        message_id: row.id,
        mailbox_id: row.mailbox_id,
        call_id: row.call_id,
        caller_principal_id: row.caller_principal_id,
        capture_source: row.capture_source,
        state: row.state,
        read_state: row.read_state,
        content_type: row.content_type,
        size_bytes: row.size_bytes,
        duration_ms: row.duration_ms,
        sha256: row.sha256,
        received_at: time(row.received_at),
        ready_at: row.ready_at.map(time),
        read_at: row.read_at.map(time),
        failure_code: row.failure_code,
        retention_until: row.retention_until.map(time),
        deleted_at: row.deleted_at.map(time),
        version: row.version,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicemailMessagePage {
    pub messages: Vec<VoicemailMessageView>,
    pub next_cursor: Option<String>,
}

/// A short-lived, single-object, read-only media URL (never stored).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicemailMediaAccess {
    pub message_id: Uuid,
    pub url: String,
    pub expires_at: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub sha256: String,
}

/// Who asks for messages: a voicemail administrator, or the application acting for one
/// principal (who must own the mailbox).
#[derive(Debug, Clone, Copy)]
pub enum VoicemailRequester {
    Administrator,
    Principal(Uuid),
}

/// Mailbox settings.
#[derive(Debug, Clone)]
pub struct MailboxSettings {
    pub owner_type: MailboxOwnerType,
    pub owner_id: Uuid,
    pub display_label: String,
    pub max_message_seconds: i32,
    pub retention_days: i32,
}

#[derive(Debug, Clone, Default)]
pub struct MailboxChange {
    pub display_label: Option<String>,
    pub max_message_seconds: Option<i32>,
    pub retention_days: Option<i32>,
}

/// Message filters.
#[derive(Debug, Clone, Copy, Default)]
pub struct MessageFilter {
    pub state: Option<MessageState>,
    pub read_state: Option<ReadState>,
}

fn validate_limits(max: i32, retention: i32) -> EnterpriseResult<()> {
    if !(5..=600).contains(&max) {
        return Err(EnterpriseError::Validation(
            "maxMessageSeconds must be between 5 and 600.",
        ));
    }
    if !(1..=3650).contains(&retention) {
        return Err(EnterpriseError::Validation(
            "retentionDays must be between 1 and 3650.",
        ));
    }
    Ok(())
}

pub(crate) async fn mailbox_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<MailboxRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    MailboxRow::find_by_statement(stmt(
        &format!(
            "SELECT {MAILBOX_COLUMNS} FROM voicemail_mailboxes \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

pub(crate) async fn message_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<MessageRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    MessageRow::find_by_statement(stmt(
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM voicemail_messages \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The principals who own `mailbox`: its principal; an extension's member; a queue's
/// agents; a ring group's principal members.
pub(crate) async fn mailbox_owners<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    mailbox: &MailboxRow,
) -> EnterpriseResult<Vec<Uuid>> {
    let mut values = scope_values(scope);
    values.push(mailbox.owner_id.into());
    let sql = match MailboxOwnerType::parse(&mailbox.owner_type) {
        Some(MailboxOwnerType::Principal) => return Ok(vec![mailbox.owner_id]),
        Some(MailboxOwnerType::Extension) => {
            "SELECT destination_id FROM enterprise_extensions WHERE tenant_id = $1 \
             AND application_id = $2 AND environment_id = $3 AND id = $4 \
             AND destination_type IN ('PRINCIPAL', 'ENTERPRISE_MEMBER')"
        }
        Some(MailboxOwnerType::Queue) => {
            "SELECT principal_id FROM enterprise_queue_members WHERE tenant_id = $1 \
             AND application_id = $2 AND environment_id = $3 AND queue_id = $4"
        }
        Some(MailboxOwnerType::RingGroup) => {
            "SELECT member_id FROM enterprise_ring_group_members WHERE tenant_id = $1 \
             AND application_id = $2 AND environment_id = $3 AND ring_group_id = $4 \
             AND member_type IN ('PRINCIPAL', 'ENTERPRISE_MEMBER')"
        }
        None => return Ok(Vec::new()),
    };
    let rows = db.query_all(stmt(sql, values)).await?;
    rows.into_iter()
        .map(|r| r.try_get_by_index::<Uuid>(0).map_err(EnterpriseError::from))
        .collect()
}

/// The object key of a message's audio (opaque, internal, never returned).
pub(crate) fn object_key(scope: CallScope, message_id: Uuid, content_type: &str) -> String {
    // The extension follows the verified format (SIP edge: WAV; RTC egress: Ogg/Opus).
    let extension = if content_type == "audio/ogg" {
        "ogg"
    } else {
        "wav"
    };
    format!(
        "voicemail/{}/{}/{}/{message_id}.{extension}",
        scope.tenant_id().as_uuid().simple(),
        scope.application_id().as_uuid().simple(),
        scope.environment_id().as_uuid().simple()
    )
}

impl EnterpriseService {
    pub async fn create_mailbox(
        &self,
        actor: &AdminActor,
        settings: MailboxSettings,
    ) -> EnterpriseResult<MailboxView> {
        let label = clean_name(
            &settings.display_label,
            100,
            "displayLabel must be 1-100 characters.",
        )?;
        validate_limits(settings.max_message_seconds, settings.retention_days)?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageVoicemail).await?;
        if !policy::effective_policy(&txn, actor.scope, None)
            .await?
            .rules
            .voicemail_allowed
        {
            return Err(EnterpriseError::PolicyDenied(
                "The enterprise policy does not permit voicemail.",
            ));
        }
        let owner_type = settings.owner_type.destination_type();
        destinations::validate(
            &txn,
            actor.scope,
            Destination::new(owner_type, settings.owner_id),
            &[owner_type],
        )
        .await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            settings.owner_type.as_str().into(),
            settings.owner_id.into(),
            label.into(),
            settings.max_message_seconds.into(),
            settings.retention_days.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO voicemail_mailboxes (id, tenant_id, application_id, environment_id, \
             owner_type, owner_id, display_label, max_message_seconds, retention_days, status, \
             version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ACTIVE', 1, $10, $10)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::VoicemailMailbox,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("ownerType", settings.owner_type.as_str())
                    .with("retentionDays", settings.retention_days as i64),
            },
            now,
        )
        .await?;
        let out = mailbox_view(mailbox_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "VOICEMAIL_MAILBOX"), ("operation", "CREATE")],
        );
        Ok(out)
    }

    pub async fn get_mailbox(&self, scope: CallScope, id: Uuid) -> EnterpriseResult<MailboxView> {
        Ok(mailbox_view(
            mailbox_row(self.db(), scope, id, false).await?,
        ))
    }

    pub async fn list_mailboxes(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<MailboxPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {MAILBOX_COLUMNS} FROM voicemail_mailboxes \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = MailboxRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        Ok(MailboxPage {
            mailboxes: rows.into_iter().map(mailbox_view).collect(),
            next_cursor,
        })
    }

    pub async fn update_mailbox(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: MailboxChange,
    ) -> EnterpriseResult<MailboxView> {
        let label = change
            .display_label
            .as_deref()
            .map(|l| clean_name(l, 100, "displayLabel must be 1-100 characters."))
            .transpose()?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageVoicemail).await?;
        let row = mailbox_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled mailbox cannot be changed",
            ));
        }
        let max = change
            .max_message_seconds
            .unwrap_or(row.max_message_seconds);
        let retention = change.retention_days.unwrap_or(row.retention_days);
        validate_limits(max, retention)?;
        let now = self.now();
        exec(
            &txn,
            "UPDATE voicemail_mailboxes SET display_label = $2, max_message_seconds = $3, \
             retention_days = $4, version = version + 1, updated_at = GREATEST(updated_at, $5) \
             WHERE id = $1",
            vec![
                id.into(),
                label.unwrap_or(row.display_label.clone()).into(),
                max.into(),
                retention.into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::VoicemailMailbox,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new().with("retentionDays", retention as i64),
            },
            now,
        )
        .await?;
        let out = mailbox_view(mailbox_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// Never blocked: routing to a disabled mailbox ends in NO_ROUTE; its messages stay.
    pub async fn disable_mailbox(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<MailboxView> {
        let txn = self.begin().await?;
        let row = mailbox_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict("the mailbox is already disabled"));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE voicemail_mailboxes SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::VoicemailMailbox,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = mailbox_view(mailbox_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// Authorizes `requester` on `mailbox` (an owner, or an administrator). A principal
    /// who does not own it gets 404 — like a message of another scope.
    async fn authorize_mailbox<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        mailbox_id: Uuid,
        requester: VoicemailRequester,
    ) -> EnterpriseResult<MailboxRow> {
        let mailbox = mailbox_row(db, scope, mailbox_id, false).await?;
        if let VoicemailRequester::Principal(p) = requester {
            if !mailbox_owners(db, scope, &mailbox).await?.contains(&p) {
                return Err(EnterpriseError::NotFound);
            }
        }
        Ok(mailbox)
    }

    /// A mailbox's messages, newest first (keyset).
    pub async fn list_voicemail_messages(
        &self,
        scope: CallScope,
        mailbox_id: Uuid,
        requester: VoicemailRequester,
        filter: MessageFilter,
        page: &PageRequest,
    ) -> EnterpriseResult<VoicemailMessagePage> {
        self.authorize_mailbox(self.db(), scope, mailbox_id, requester)
            .await?;
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        values.push(mailbox_id.into());
        let mut sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM voicemail_messages \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND mailbox_id = $4"
        );
        match filter.state {
            Some(state) => {
                values.push(state.as_str().into());
                sql.push_str(&format!(" AND state = ${}", values.len()));
            }
            // Deleted messages are history, not mailbox content.
            None => sql.push_str(" AND state <> 'DELETED'"),
        }
        if let Some(read) = filter.read_state {
            values.push(read.as_str().into());
            sql.push_str(&format!(" AND read_state = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (received_at, id) < (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(
            " ORDER BY received_at DESC, id DESC LIMIT {}",
            limit + 1
        ));
        let mut rows = MessageRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.received_at, r.id))
        } else {
            None
        };
        Ok(VoicemailMessagePage {
            messages: rows.into_iter().map(message_view).collect(),
            next_cursor,
        })
    }

    async fn authorized_message<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        id: Uuid,
        requester: VoicemailRequester,
        lock: bool,
    ) -> EnterpriseResult<MessageRow> {
        let message = message_row(db, scope, id, lock).await?;
        self.authorize_mailbox(db, scope, message.mailbox_id, requester)
            .await?;
        Ok(message)
    }

    pub async fn get_voicemail_message(
        &self,
        scope: CallScope,
        id: Uuid,
        requester: VoicemailRequester,
    ) -> EnterpriseResult<VoicemailMessageView> {
        Ok(message_view(
            self.authorized_message(self.db(), scope, id, requester, false)
                .await?,
        ))
    }

    /// Marks a message READ / UNREAD (versioned, audited).
    pub async fn set_voicemail_read(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        read: bool,
        requester: VoicemailRequester,
    ) -> EnterpriseResult<VoicemailMessageView> {
        let txn = self.begin().await?;
        let row = self
            .authorized_message(&txn, actor.scope, id, requester, true)
            .await?;
        check_version(expected_version, row.version)?;
        if matches!(row.state.as_str(), "DELETING" | "DELETED") {
            return Err(EnterpriseError::Conflict("the message is deleted"));
        }
        let target = if read { "READ" } else { "UNREAD" };
        if row.read_state != target {
            let now = self.now();
            exec(
                &txn,
                "UPDATE voicemail_messages SET read_state = $2, \
                 read_at = CASE WHEN $2 = 'READ' THEN GREATEST(received_at, $3) END, \
                 version = version + 1, updated_at = GREATEST(updated_at, $3) WHERE id = $1",
                vec![id.into(), target.into(), now.into()],
            )
            .await?;
            audit::record(
                &txn,
                actor,
                AuditFact {
                    resource: AuditResource::VoicemailMessage,
                    resource_id: id,
                    operation: if read {
                        AuditOperation::MarkRead
                    } else {
                        AuditOperation::MarkUnread
                    },
                    old_version: Some(row.version),
                    new_version: Some(row.version + 1),
                    details: AuditDetails::new().with(
                        "onBehalfOfOwner",
                        matches!(requester, VoicemailRequester::Principal(_)),
                    ),
                },
                now,
            )
            .await?;
        }
        let out = message_view(message_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// Requests deletion (asynchronous: DELETING now, the object removed by the worker).
    pub async fn delete_voicemail_message(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        requester: VoicemailRequester,
    ) -> EnterpriseResult<VoicemailMessageView> {
        let txn = self.begin().await?;
        let row = self
            .authorized_message(&txn, actor.scope, id, requester, true)
            .await?;
        check_version(expected_version, row.version)?;
        match row.state.as_str() {
            "DELETING" | "DELETED" => {
                return Err(EnterpriseError::Conflict("the message is already deleted"))
            }
            "RECORDING" | "PROCESSING" => {
                return Err(EnterpriseError::Conflict(
                    "the message is still being captured",
                ))
            }
            _ => {}
        }
        let now = self.now();
        let reason = match requester {
            VoicemailRequester::Principal(_) => "USER",
            VoicemailRequester::Administrator => "ADMIN",
        };
        exec(
            &txn,
            "UPDATE voicemail_messages SET state = 'DELETING', deleting_at = $2, delete_reason = $3, \
             next_attempt_at = $2, version = version + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1",
            vec![id.into(), now.into(), reason.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::VoicemailMessage,
                resource_id: id,
                operation: AuditOperation::Delete,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new().with("reason", reason),
            },
            now,
        )
        .await?;
        let out = message_view(message_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// Authorized media access: owner / administrator, READY only, a short-lived signed
    /// read of ONE object, audited. Nothing is stored.
    pub async fn voicemail_media_access(
        &self,
        actor: &AdminActor,
        id: Uuid,
        requester: VoicemailRequester,
    ) -> EnterpriseResult<VoicemailMediaAccess> {
        let txn = self.begin().await?;
        let row = self
            .authorized_message(&txn, actor.scope, id, requester, false)
            .await?;
        if row.state != "READY" {
            return Err(EnterpriseError::Conflict(
                "the message media is not available",
            ));
        }
        let (Some(key), Some(content_type), Some(size), Some(sha256)) = (
            row.object_key.as_deref(),
            row.content_type.clone(),
            row.size_bytes,
            row.sha256.clone(),
        ) else {
            return Err(EnterpriseError::Conflict(
                "the message media is not available",
            ));
        };
        let now = self.now();
        let read = self
            .artifacts
            .signed_read(key, MEDIA_ACCESS_TTL, now)
            .map_err(|e| match e {
                cf_object_storage::StorageError::NotConfigured => {
                    EnterpriseError::Unavailable("artifact storage is not configured")
                }
                _ => EnterpriseError::StorageUnavailable,
            })?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::VoicemailMessage,
                resource_id: id,
                operation: AuditOperation::MediaAccess,
                old_version: Some(row.version),
                new_version: Some(row.version),
                details: AuditDetails::new()
                    .with("ttlSeconds", MEDIA_ACCESS_TTL.as_secs() as i64)
                    .with(
                        "onBehalfOfOwner",
                        matches!(requester, VoicemailRequester::Principal(_)),
                    ),
            },
            now,
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_artifact_access_total",
            &[("artifact", "VOICEMAIL")],
        );
        Ok(VoicemailMediaAccess {
            message_id: id,
            url: read.url,
            expires_at: time(read.expires_at),
            content_type,
            size_bytes: size,
            sha256,
        })
    }
}

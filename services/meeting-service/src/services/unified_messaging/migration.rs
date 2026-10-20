//! Part V H §100 — the chat migration state machine.
//!
//! ```text
//! LEGACY_PRIMARY ──▶ SHADOW_VALIDATION ──▶ MESSAGING_PRIMARY ──▶ LEGACY_RETIRED (final)
//!        ▲                 │    ▲                  │
//!        └──── abort ──────┘    └──── rollback ────┘
//! ```
//!
//! | state | main-room chat writes / reads | Messaging |
//! |---|---|---|
//! | LEGACY_PRIMARY | legacy store | not used (identity bridge only) |
//! | SHADOW_VALIDATION | legacy store (authoritative) | conversations + members synced, every message mirrored as LEGACY_MANAGED, parity measured; a Messaging failure never affects the meeting |
//! | MESSAGING_PRIMARY | Messaging (authoritative); the database refuses new legacy main-room rows | — |
//! | LEGACY_RETIRED | Messaging only, no rollback | — |
//!
//! Gates (all enforced here, transitions are audited and version-checked):
//! * SHADOW_VALIDATION → MESSAGING_PRIMARY needs a PASSED parity run that finished in
//!   the current shadow period, nothing left unmirrored, and a named approver who is not
//!   the requesting operator.
//! * MESSAGING_PRIMARY → LEGACY_RETIRED needs a named approver; it is irreversible (also
//!   enforced by a database trigger).
//! * MESSAGING_PRIMARY → SHADOW_VALIDATION is the rollback: the legacy store becomes
//!   authoritative again immediately and every Messaging-native message is copied back
//!   (labelled `MESSAGING_ROLLBACK`) before an abort to LEGACY_PRIMARY is allowed.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{all, exec, one, rfc3339, UnifiedMessaging};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::ops_metrics::{inc, OPS_METRICS as OM},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChatMigrationState {
    LegacyPrimary,
    ShadowValidation,
    MessagingPrimary,
    LegacyRetired,
}

impl ChatMigrationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LegacyPrimary => "LEGACY_PRIMARY",
            Self::ShadowValidation => "SHADOW_VALIDATION",
            Self::MessagingPrimary => "MESSAGING_PRIMARY",
            Self::LegacyRetired => "LEGACY_RETIRED",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "LEGACY_PRIMARY" => Self::LegacyPrimary,
            "SHADOW_VALIDATION" => Self::ShadowValidation,
            "MESSAGING_PRIMARY" => Self::MessagingPrimary,
            "LEGACY_RETIRED" => Self::LegacyRetired,
            _ => return None,
        })
    }

    /// Messaging is the authority for main-room chat.
    pub fn messaging_authoritative(&self) -> bool {
        matches!(self, Self::MessagingPrimary | Self::LegacyRetired)
    }

    /// Allowed edges (gates are checked separately).
    pub fn can_become(&self, next: Self) -> bool {
        use ChatMigrationState::*;
        matches!(
            (self, next),
            (LegacyPrimary, ShadowValidation)
                | (ShadowValidation, LegacyPrimary)
                | (ShadowValidation, MessagingPrimary)
                | (MessagingPrimary, ShadowValidation)
                | (MessagingPrimary, LegacyRetired)
        )
    }
}

/// The state and its version (no lock).
pub async fn current<C: ConnectionTrait>(db: &C) -> ApiResult<(ChatMigrationState, i64)> {
    let r = one(
        db,
        "SELECT state, version FROM conference_chat_migration WHERE id = 1",
        vec![],
    )
    .await?
    .ok_or_else(|| ApiError::InternalError("chat migration state missing".into()))?;
    let s: String = r.try_get_by_index(0)?;
    Ok((
        ChatMigrationState::parse(&s)
            .ok_or_else(|| ApiError::InternalError("unknown chat migration state".into()))?,
        r.try_get_by_index(1)?,
    ))
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransitionRequest {
    pub target_state: ChatMigrationState,
    /// Optimistic concurrency: the version the operator looked at.
    pub expected_version: i64,
    /// Why (audited), 3–500 characters.
    pub reason: String,
    /// Second operator approving a cutover or retirement (never the requester).
    #[serde(default)]
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParityRunView {
    pub id: Uuid,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub conferences_checked: i32,
    pub messages_checked: i32,
    pub mismatches: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransitionView {
    pub from_state: String,
    pub to_state: String,
    pub version: i64,
    pub actor: String,
    pub approved_by: Option<String>,
    pub reason: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationStatusView {
    pub state: ChatMigrationState,
    pub version: i64,
    pub changed_at: String,
    pub changed_by: String,
    /// Legacy messages (every scope) of linked conferences not yet mirrored.
    pub pending_mirror: i64,
    /// Messaging-native messages not yet copied back after a rollback.
    pub pending_rollback_copies: i64,
    pub pending_session_revocations: i64,
    pub linked_conferences: i64,
    pub latest_parity_run: Option<ParityRunView>,
    pub transitions: Vec<TransitionView>,
    /// What is migrated: every meeting chat scope.
    pub scope: Value,
}

fn refused(message: impl Into<String>) -> ApiError {
    ApiError::domain(ErrorCode::ChatMigrationRefused, message)
}

async fn scalar<C: ConnectionTrait>(db: &C, sql: &str) -> ApiResult<i64> {
    Ok(one(db, sql, vec![])
        .await?
        .map(|r| r.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0))
}

/// Legacy messages of linked conferences not yet mirrored: main room, breakout rooms
/// and private chats alike (all three must be in Messaging before a cutover).
pub(crate) const PENDING_MIRROR_SQL: &str = "SELECT count(*) FROM conference_messages m \
     JOIN conference_chat_links l ON l.conference_id = m.conference_id \
     LEFT JOIN conference_chat_message_links k ON k.legacy_message_id = m.id \
     WHERE m.origin = 'MEETING' AND k.messaging_message_id IS NULL";

pub(crate) const PENDING_ROLLBACK_SQL: &str = "SELECT count(*) FROM conference_chat_message_links \
     WHERE origin = 'MESSAGING_NATIVE' AND legacy_message_id IS NULL";

fn row_time(r: &sea_orm::QueryResult, i: usize) -> ApiResult<DateTime<Utc>> {
    Ok(r.try_get_by_index(i)?)
}

impl UnifiedMessaging {
    pub async fn latest_parity(&self) -> ApiResult<Option<ParityRunView>> {
        one(
            &self.db,
            "SELECT id, status, started_at, finished_at, conferences_checked, messages_checked, mismatches \
             FROM conference_chat_parity_runs ORDER BY started_at DESC LIMIT 1",
            vec![],
        )
        .await?
        .map(|r| -> ApiResult<ParityRunView> {
            Ok(ParityRunView {
                id: r.try_get_by_index(0)?,
                status: r.try_get_by_index(1)?,
                started_at: rfc3339(row_time(&r, 2)?),
                finished_at: r
                    .try_get_by_index::<Option<DateTime<Utc>>>(3)?
                    .map(rfc3339),
                conferences_checked: r.try_get_by_index(4)?,
                messages_checked: r.try_get_by_index(5)?,
                mismatches: r.try_get_by_index(6)?,
            })
        })
        .transpose()
    }

    pub async fn migration_status(&self) -> ApiResult<MigrationStatusView> {
        let r = one(
            &self.db,
            "SELECT state, version, changed_at, changed_by FROM conference_chat_migration WHERE id = 1",
            vec![],
        )
        .await?
        .ok_or_else(|| ApiError::InternalError("chat migration state missing".into()))?;
        let state: String = r.try_get_by_index(0)?;
        let transitions = all(
            &self.db,
            "SELECT from_state, to_state, version, actor, approved_by, reason, created_at \
             FROM conference_chat_migration_transitions ORDER BY version DESC LIMIT 20",
            vec![],
        )
        .await?
        .iter()
        .map(|t| -> ApiResult<TransitionView> {
            Ok(TransitionView {
                from_state: t.try_get_by_index(0)?,
                to_state: t.try_get_by_index(1)?,
                version: t.try_get_by_index(2)?,
                actor: t.try_get_by_index(3)?,
                approved_by: t.try_get_by_index(4)?,
                reason: t.try_get_by_index(5)?,
                created_at: rfc3339(row_time(t, 6)?),
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
        Ok(MigrationStatusView {
            state: ChatMigrationState::parse(&state)
                .ok_or_else(|| ApiError::InternalError("unknown chat migration state".into()))?,
            version: r.try_get_by_index(1)?,
            changed_at: rfc3339(row_time(&r, 2)?),
            changed_by: r.try_get_by_index(3)?,
            pending_mirror: scalar(&self.db, PENDING_MIRROR_SQL).await?,
            pending_rollback_copies: scalar(&self.db, PENDING_ROLLBACK_SQL).await?,
            pending_session_revocations: self.pending_revocations().await?,
            linked_conferences: scalar(&self.db, "SELECT count(*) FROM conference_chat_links")
                .await?,
            latest_parity_run: self.latest_parity().await?,
            transitions,
            scope: json!({
                "migrated": ["MAIN_ROOM_CHAT", "ANNOUNCEMENTS", "BREAKOUT_CHAT", "PRIVATE_CHAT"],
                "legacy": [],
                "securityProfile": "MANAGED",
                "historicalProvenance": "LEGACY_MANAGED",
            }),
        })
    }

    /// One audited, version-checked transition (operator console / platform admin).
    pub async fn transition(
        &self,
        actor: &str,
        request: TransitionRequest,
    ) -> ApiResult<MigrationStatusView> {
        let reason = request.reason.trim().to_string();
        if !(3..=500).contains(&reason.chars().count()) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "reason must be 3-500 characters",
            ));
        }
        let approver = request
            .approved_by
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string);
        if approver.as_deref().is_some_and(|a| a.chars().count() > 160) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "approvedBy must be at most 160 characters",
            ));
        }
        let target = request.target_state;
        let txn = self.db.begin().await?;
        let r = one(
            &txn,
            "SELECT state, version, changed_at FROM conference_chat_migration WHERE id = 1 FOR UPDATE",
            vec![],
        )
        .await?
        .ok_or_else(|| ApiError::InternalError("chat migration state missing".into()))?;
        let from = ChatMigrationState::parse(&r.try_get_by_index::<String>(0)?)
            .ok_or_else(|| ApiError::InternalError("unknown chat migration state".into()))?;
        let version: i64 = r.try_get_by_index(1)?;
        let changed_at = row_time(&r, 2)?;
        if version != request.expected_version {
            return Err(ApiError::domain(
                ErrorCode::MeetingVersionConflict,
                format!("The migration state changed (version {version}); review and retry"),
            ));
        }
        if !from.can_become(target) {
            return Err(refused(format!(
                "{} cannot become {}",
                from.as_str(),
                target.as_str()
            )));
        }
        let mut parity_run: Option<Uuid> = None;
        use ChatMigrationState::*;
        match (from, target) {
            (ShadowValidation, MessagingPrimary) => {
                let a = approver.as_deref().ok_or_else(|| {
                    refused("Cutover needs a second operator's approval (approvedBy)")
                })?;
                if a.eq_ignore_ascii_case(actor) {
                    return Err(refused("The approver must not be the requesting operator"));
                }
                let pending = scalar(&txn, PENDING_MIRROR_SQL).await?;
                if pending > 0 {
                    return Err(refused(format!(
                        "{pending} meeting chat messages are not mirrored yet"
                    )));
                }
                let run = one(
                    &txn,
                    "SELECT id, status, finished_at FROM conference_chat_parity_runs \
                     WHERE finished_at IS NOT NULL ORDER BY finished_at DESC LIMIT 1",
                    vec![],
                )
                .await?
                .ok_or_else(|| refused("No parity run has completed"))?;
                let status: String = run.try_get_by_index(1)?;
                let finished: DateTime<Utc> = run.try_get_by_index(2)?;
                if status != "PASSED" {
                    return Err(refused(format!("The latest parity run is {status}")));
                }
                if finished < changed_at {
                    return Err(refused(
                        "The latest parity run predates this shadow period; run parity again",
                    ));
                }
                parity_run = Some(run.try_get_by_index(0)?);
            }
            (MessagingPrimary, LegacyRetired) => {
                let a = approver.as_deref().ok_or_else(|| {
                    refused("Retirement needs a second operator's approval (approvedBy)")
                })?;
                if a.eq_ignore_ascii_case(actor) {
                    return Err(refused("The approver must not be the requesting operator"));
                }
            }
            (ShadowValidation, LegacyPrimary) => {
                // Abort shadow mode only once every rolled-back message is home again.
                let owed = scalar(&txn, PENDING_ROLLBACK_SQL).await?;
                if owed > 0 {
                    return Err(refused(format!(
                        "{owed} Messaging messages are not copied back yet"
                    )));
                }
            }
            _ => {}
        }
        let next = version + 1;
        exec(
            &txn,
            "UPDATE conference_chat_migration SET state = $1, version = $2, changed_at = now(), changed_by = $3, reason = $4 WHERE id = 1",
            vec![target.as_str().into(), next.into(), actor.into(), reason.clone().into()],
        )
        .await?;
        exec(
            &txn,
            "INSERT INTO conference_chat_migration_transitions (from_state, to_state, version, actor, approved_by, reason, parity_run_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                from.as_str().into(),
                target.as_str().into(),
                next.into(),
                actor.into(),
                approver.into(),
                reason.into(),
                parity_run.into(),
            ],
        )
        .await?;
        txn.commit().await?;
        inc(&OM.unified_messaging_cutover_transitions);
        tracing::warn!(
            from = from.as_str(),
            to = target.as_str(),
            version = next,
            "chat migration state changed"
        );
        if (from, target) == (MessagingPrimary, ShadowValidation) {
            // Start copying back right away (the worker finishes whatever is left).
            if let Err(e) = self.copy_back_native(500).await {
                tracing::warn!(error = %e, "rollback copy deferred to the worker");
            }
        }
        self.migration_status().await
    }
}

//! ConsentService — independent consent types and policy-driven semantics (Part G
//! §17–§22).
//!
//! RECORDING, TRANSCRIPTION, AI_PROCESSING and DATA_SHARING are independent: none
//! implies another. Each `meeting_consents` row is the participant's *current*
//! decision for one type and policy version (ACCEPTED is stored as `GRANTED`);
//! history lives in the audit log. Only the participant can decide for themselves —
//! requests carry no participant field.
//!
//! - Pre-join: active capture with EXPLICIT_CONSENT_REQUIRED adds that consent to the
//!   join requirements (`AdmissionService`).
//! - Mid-meeting: enabling processing re-evaluates consent; participants still missing
//!   it receive a targeted `consent.required` (individual answers are never broadcast)
//!   and processing does not start until the requirement is satisfied.
//! - Withdrawal follows the profile's `withdrawalMode`; previously retained artefacts
//!   are never deleted retroactively unless policy requires it.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ArtifactCtx;
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    middleware::request_id::current_request_id,
    services::{
        actor::ActorContext,
        collaboration::{effective_policy, policy::ConsentKind},
        meeting_core::{lock_conference, Core},
        meeting_events::{
            action, audit, emit_to, event, Audience, AuditActor, AuditRecord, OutboxRecord,
        },
        ops_metrics::{inc, OPS_METRICS as OM},
        profiles::{ConsentMode, ResolvedProfile, WithdrawalMode},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConsentDecision {
    Accepted,
    Declined,
    Withdrawn,
}

impl ConsentDecision {
    fn stored(&self) -> &'static str {
        match self {
            Self::Accepted => "GRANTED",
            Self::Declined => "DECLINED",
            Self::Withdrawn => "WITHDRAWN",
        }
    }

    fn from_stored(s: &str) -> Option<Self> {
        match s {
            "GRANTED" | "ACCEPTED" => Some(Self::Accepted),
            "DECLINED" => Some(Self::Declined),
            "WITHDRAWN" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// The four processing consents governed by Part G.
pub const PROCESSING_CONSENTS: [ConsentKind; 4] = [
    ConsentKind::Recording,
    ConsentKind::Transcription,
    ConsentKind::AiProcessing,
    ConsentKind::DataSharing,
];

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequirement {
    pub consent_type: ConsentKind,
    pub mode: ConsentMode,
    /// Affirmative consent is required before this processing runs.
    pub required: bool,
    /// The processing is currently active in this conference.
    pub active: bool,
    pub consent_version: String,
    pub decision: Option<ConsentDecision>,
    pub decided_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequirementsResponse {
    pub conference_id: Uuid,
    pub requirements: Vec<ConsentRequirement>,
    /// Recording notice is always shown while capture is active.
    pub recording_notice: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsentDecisionInput {
    pub consent_type: ConsentKind,
    /// ACCEPTED or DECLINED (withdrawal has its own endpoint).
    pub decision: ConsentDecision,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordConsentRequest {
    pub consent_version: String,
    pub decisions: Vec<ConsentDecisionInput>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WithdrawalEffect {
    /// Recorded; nothing else changed.
    Recorded,
    /// The active capture that depended on this consent was stopped.
    ProcessingStopped,
    /// The participant was returned to the waiting room.
    ParticipantExited,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawalResponse {
    pub consent_type: ConsentKind,
    pub effect: WithdrawalEffect,
    pub withdrawal_mode: WithdrawalMode,
    /// Previously retained artefacts are not deleted by a withdrawal.
    pub retroactive_deletion: bool,
}

pub struct ConsentService {
    core: Core,
}

/// Mode of a processing consent under the effective profile.
pub fn mode_for(profile: &ResolvedProfile, kind: ConsentKind) -> ConsentMode {
    let p = &profile.policy;
    let explicit = |b: bool| {
        if b {
            ConsentMode::ExplicitConsentRequired
        } else {
            ConsentMode::NoticeOnly
        }
    };
    match kind {
        ConsentKind::Recording => p.recording.consent_mode,
        ConsentKind::Transcription => p.transcription.consent_mode,
        ConsentKind::AiProcessing => explicit(p.intelligence.consent_required),
        ConsentKind::DataSharing => explicit(p.data_sharing.consent_required),
        ConsentKind::HealthcareTerms | ConsentKind::PrivacyNotice => {
            ConsentMode::ExplicitConsentRequired
        }
    }
}

fn withdrawal_mode_for(profile: &ResolvedProfile, kind: ConsentKind) -> WithdrawalMode {
    match kind {
        ConsentKind::Recording => profile.policy.recording.withdrawal_mode,
        ConsentKind::Transcription => profile.policy.transcription.withdrawal_mode,
        _ => WithdrawalMode::StopFutureProcessing,
    }
}

/// Current decisions of one participant for a version.
pub async fn decisions<C: ConnectionTrait>(
    conn: &C,
    participant_id: Uuid,
    version: &str,
) -> ApiResult<Vec<(ConsentKind, ConsentDecision, chrono::DateTime<Utc>)>> {
    let rows = conn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT consent_type, decision, decided_at FROM meeting_consents
              WHERE participant_id = $1 AND consent_version = $2",
            [participant_id.into(), version.into()],
        ))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let t: String = r.try_get_by_index(0).ok()?;
            let d: String = r.try_get_by_index(1).ok()?;
            let at: chrono::DateTime<Utc> = r.try_get_by_index(2).ok()?;
            let kind: ConsentKind = serde_json::from_value(serde_json::Value::String(t)).ok()?;
            Some((kind, ConsentDecision::from_stored(&d)?, at))
        })
        .collect())
}

/// Participants who have not accepted `kind` (`missing`) and those who explicitly
/// declined / withdrew it (`declined`). `active_only`: admitted participants with an
/// active session (live processing); otherwise everyone who ever had a session
/// (post-meeting processing of their media).
pub async fn missing_consent<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    kind: ConsentKind,
    version: &str,
    active_only: bool,
) -> ApiResult<(Vec<Uuid>, Vec<Uuid>)> {
    let scope = if active_only {
        "p.admission_state = 'ADMITTED' AND EXISTS (SELECT 1 FROM participant_sessions s
                             WHERE s.conference_participant_id = p.id
                               AND s.state IN ('PENDING','CONNECTED'))"
    } else {
        "EXISTS (SELECT 1 FROM participant_sessions s WHERE s.conference_participant_id = p.id)"
    };
    let rows = conn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT p.id, c.decision
                   FROM conference_participants p
                   LEFT JOIN meeting_consents c
                     ON c.participant_id = p.id AND c.consent_type = $2 AND c.consent_version = $3
                  WHERE p.conference_id = $1 AND {scope}"
            ),
            [conference_id.into(), kind.as_str().into(), version.into()],
        ))
        .await?;
    let mut missing = vec![];
    let mut declined = vec![];
    for r in rows {
        let id: Uuid = match r.try_get_by_index(0) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let d: Option<String> = r.try_get_by_index(1).ok().flatten();
        match d.as_deref() {
            Some("GRANTED") => {}
            Some("DECLINED") | Some("WITHDRAWN") => {
                declined.push(id);
                missing.push(id);
            }
            _ => missing.push(id),
        }
    }
    Ok((missing, declined))
}

impl ConsentService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    pub fn version(c: &ArtifactCtx) -> String {
        effective_policy(&c.space, &c.conference).consent_version
    }

    /// Mid-meeting / start-time gate for a processing kind. NOTICE_ONLY never blocks.
    /// Missing participants get a targeted `consent.required`.
    pub async fn require_for_processing(
        &self,
        c: &ArtifactCtx,
        kind: ConsentKind,
    ) -> ApiResult<()> {
        self.require(c, kind, true).await
    }

    /// Post-meeting variant: everyone whose media / words are processed must consent.
    pub async fn require_for_post_meeting(
        &self,
        c: &ArtifactCtx,
        kind: ConsentKind,
    ) -> ApiResult<()> {
        self.require(c, kind, false).await
    }

    async fn require(
        &self,
        c: &ArtifactCtx,
        kind: ConsentKind,
        active_only: bool,
    ) -> ApiResult<()> {
        if mode_for(&c.profile, kind) != ConsentMode::ExplicitConsentRequired {
            return Ok(());
        }
        let version = Self::version(c);
        let (missing, declined) =
            missing_consent(&self.core.db, c.conference.id, kind, &version, active_only).await?;
        if missing.is_empty() {
            return Ok(());
        }
        inc(&OM.consent_required);
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, c.conference.id).await?;
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "consent",
                aggregate_id: c.conference.id,
                event_type: event::CONSENT_REQUIRED,
                meeting_space_id: Some(c.space.id),
                conference_id: Some(c.conference.id),
                payload: json!({
                    "conferenceId": c.conference.id,
                    "consentType": kind,
                    "consentVersion": version,
                    "mode": ConsentMode::ExplicitConsentRequired,
                }),
            },
            Some(Audience::Participants {
                ids: missing.clone(),
                moderator_permission: None,
            }),
        )
        .await?;
        txn.commit().await?;
        if !declined.is_empty() {
            inc(&OM.consent_declined);
            return Err(ApiError::domain(
                ErrorCode::ConsentDeclined,
                format!(
                    "{} participant(s) declined {} consent",
                    declined.len(),
                    kind.as_str()
                ),
            ));
        }
        Err(ApiError::domain(
            ErrorCode::ConsentRequired,
            format!(
                "Waiting for {} participant(s) to grant {} consent",
                missing.len(),
                kind.as_str()
            ),
        ))
    }

    pub async fn requirements(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<ConsentRequirementsResponse> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let version = Self::version(&c);
        let mine = match &c.participant {
            Some(p) => decisions(&self.core.db, p.id, &version).await?,
            None => vec![],
        };
        let recording_active = c.conference.recording_state != "OFF";
        let transcription_active = c.conference.transcription_state != "OFF";
        let requirements = PROCESSING_CONSENTS
            .iter()
            .map(|k| {
                let mode = mode_for(&c.profile, *k);
                let active = match k {
                    ConsentKind::Recording => recording_active,
                    ConsentKind::Transcription => transcription_active,
                    _ => false,
                };
                let d = mine.iter().find(|(kind, _, _)| kind == k);
                ConsentRequirement {
                    consent_type: *k,
                    mode,
                    required: mode == ConsentMode::ExplicitConsentRequired,
                    active,
                    consent_version: version.clone(),
                    decision: d.map(|(_, d, _)| *d),
                    decided_at: d.map(|(_, _, at)| *at),
                }
            })
            .collect();
        Ok(ConsentRequirementsResponse {
            conference_id,
            requirements,
            recording_notice: recording_active,
        })
    }

    /// The participant records their OWN decisions (no participant field exists).
    pub async fn record(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: &RecordConsentRequest,
    ) -> ApiResult<ConsentRequirementsResponse> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        let version = Self::version(&c);
        if req.consent_version != version {
            return Err(ApiError::domain(
                ErrorCode::ConsentPolicyConflict,
                "The consent policy version changed; review the current notice",
            ));
        }
        if req.decisions.is_empty() || req.decisions.len() > PROCESSING_CONSENTS.len() {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "decisions must contain 1-4 entries",
            ));
        }
        for d in &req.decisions {
            if !PROCESSING_CONSENTS.contains(&d.consent_type) {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "unsupported consentType",
                ));
            }
            if d.decision == ConsentDecision::Withdrawn {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "use the withdraw endpoint to withdraw consent",
                ));
            }
        }
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, conference_id).await?;
        for d in &req.decisions {
            upsert(
                &txn,
                conference_id,
                me.id,
                d.consent_type,
                &version,
                d.decision,
                "IN_MEETING",
            )
            .await?;
            let declined = d.decision == ConsentDecision::Declined;
            audit(
                &txn,
                &AuditActor::participant(me.id),
                AuditRecord {
                    meeting_space_id: c.space.id,
                    conference_id: Some(conference_id),
                    action: if declined {
                        action::CONSENT_DECLINED
                    } else {
                        action::CONSENT_RECORDED
                    },
                    target_type: Some("CONSENT"),
                    target_ref: Some(d.consent_type.as_str().to_string()),
                    metadata: json!({ "consentType": d.consent_type, "consentVersion": version, "decision": d.decision }),
                },
            )
            .await?;
            emit_to(
                &txn,
                OutboxRecord {
                    aggregate_type: "consent",
                    aggregate_id: me.id,
                    event_type: event::CONSENT_RECORDED,
                    meeting_space_id: Some(c.space.id),
                    conference_id: Some(conference_id),
                    payload: json!({
                        "conferenceId": conference_id,
                        "participantId": me.id,
                        "consentType": d.consent_type,
                        "decision": d.decision,
                        "consentVersion": version,
                    }),
                },
                // Individual answers go to the participant only.
                Some(Audience::Participants {
                    ids: vec![me.id],
                    moderator_permission: None,
                }),
            )
            .await?;
            inc(if declined {
                &OM.consent_declined
            } else {
                &OM.consent_recorded
            });
        }
        txn.commit().await?;
        self.requirements(ctx, conference_id).await
    }

    pub async fn withdraw(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        kind: ConsentKind,
    ) -> ApiResult<WithdrawalResponse> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        if !PROCESSING_CONSENTS.contains(&kind) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "unsupported consentType",
            ));
        }
        let version = Self::version(&c);
        let mode = withdrawal_mode_for(&c.profile, kind);
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, conference_id).await?;
        upsert(
            &txn,
            conference_id,
            me.id,
            kind,
            &version,
            ConsentDecision::Withdrawn,
            "WITHDRAWAL",
        )
        .await?;
        audit(
            &txn,
            &AuditActor::participant(me.id),
            AuditRecord {
                meeting_space_id: c.space.id,
                conference_id: Some(conference_id),
                action: action::CONSENT_WITHDRAWN,
                target_type: Some("CONSENT"),
                target_ref: Some(kind.as_str().to_string()),
                metadata: json!({ "consentType": kind, "consentVersion": version, "withdrawalMode": mode }),
            },
        )
        .await?;
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "consent",
                aggregate_id: me.id,
                event_type: event::CONSENT_WITHDRAWN,
                meeting_space_id: Some(c.space.id),
                conference_id: Some(conference_id),
                payload: json!({ "conferenceId": conference_id, "participantId": me.id, "consentType": kind }),
            },
            Some(Audience::Participants {
                ids: vec![me.id],
                moderator_permission: None,
            }),
        )
        .await?;
        txn.commit().await?;
        inc(&OM.consent_withdrawn);

        let explicit = mode_for(&c.profile, kind) == ConsentMode::ExplicitConsentRequired;
        let stop = match mode {
            WithdrawalMode::StopFutureProcessing => explicit,
            WithdrawalMode::StopProcessingMode => true,
            WithdrawalMode::RequireParticipantExit | WithdrawalMode::NoRetroactiveDeletion => false,
        };
        let mut effect = WithdrawalEffect::Recorded;
        if stop {
            let stopped = match kind {
                ConsentKind::Recording => {
                    super::recording::RecordingService::new(self.core.clone())
                        .system_stop(conference_id, "CONSENT_WITHDRAWN")
                        .await?
                }
                ConsentKind::Transcription => {
                    super::transcription::TranscriptionService::new(self.core.clone())
                        .system_stop(conference_id, "CONSENT_WITHDRAWN")
                        .await?
                }
                _ => false,
            };
            if stopped {
                effect = WithdrawalEffect::ProcessingStopped;
            }
        }
        if mode == WithdrawalMode::RequireParticipantExit {
            let active = match kind {
                ConsentKind::Recording => c.conference.recording_state != "OFF",
                ConsentKind::Transcription => c.conference.transcription_state != "OFF",
                _ => false,
            };
            if active {
                crate::services::admission::AdmissionService::new(self.core.clone())
                    .system_return_to_waiting(conference_id, me.id, "CONSENT_WITHDRAWN")
                    .await?;
                effect = WithdrawalEffect::ParticipantExited;
            }
        }
        Ok(WithdrawalResponse {
            consent_type: kind,
            effect,
            withdrawal_mode: mode,
            retroactive_deletion: false,
        })
    }
}

/// Current decision upsert (one row per participant / type / version).
pub async fn upsert<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    participant_id: Uuid,
    kind: ConsentKind,
    version: &str,
    decision: ConsentDecision,
    source: &str,
) -> ApiResult<()> {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO meeting_consents
            (id, conference_id, participant_id, consent_type, consent_version, decision,
             decided_at, withdrawn_at, source, request_id, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6, now(), CASE WHEN $6 = 'WITHDRAWN' THEN now() END, $7, $8, now())
         ON CONFLICT (participant_id, consent_type, consent_version) DO UPDATE
            SET decision = EXCLUDED.decision, decided_at = now(),
                withdrawn_at = EXCLUDED.withdrawn_at, source = EXCLUDED.source,
                request_id = EXCLUDED.request_id, updated_at = now()",
        [
            Uuid::new_v4().into(),
            conference_id.into(),
            participant_id.into(),
            kind.as_str().into(),
            version.into(),
            decision.stored().into(),
            source.into(),
            current_request_id().into(),
        ],
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{MeetingType, SecurityProfile},
        services::profiles::MeetingProfileResolver,
    };
    use serde_json::json;

    fn profile(t: MeetingType, s: SecurityProfile, space: serde_json::Value) -> ResolvedProfile {
        MeetingProfileResolver::resolve(t, s, None, &space, None)
    }

    #[test]
    fn consent_types_are_independent_and_follow_the_profile() {
        let std = profile(MeetingType::Standard, SecurityProfile::Standard, json!({}));
        let hc = profile(
            MeetingType::Healthcare,
            SecurityProfile::Healthcare,
            json!({}),
        );
        // Healthcare: every processing consent is explicit.
        for kind in [
            ConsentKind::Recording,
            ConsentKind::Transcription,
            ConsentKind::AiProcessing,
            ConsentKind::DataSharing,
        ] {
            assert_eq!(
                mode_for(&hc, kind),
                ConsentMode::ExplicitConsentRequired,
                "{kind:?} under HEALTHCARE"
            );
        }
        // Terms / privacy notices are always explicit.
        for kind in [ConsentKind::HealthcareTerms, ConsentKind::PrivacyNotice] {
            assert_eq!(mode_for(&std, kind), ConsentMode::ExplicitConsentRequired);
        }
        // Recording mode comes from the recording profile, not from AI / sharing flags.
        assert_eq!(
            mode_for(&std, ConsentKind::Recording),
            std.policy.recording.consent_mode
        );
        assert_eq!(
            mode_for(&std, ConsentKind::Transcription),
            std.policy.transcription.consent_mode
        );
    }

    #[test]
    fn space_override_switches_recording_to_explicit_consent() {
        let p = profile(
            MeetingType::Standard,
            SecurityProfile::Standard,
            json!({ "recording": { "consentMode": "EXPLICIT_CONSENT_REQUIRED" } }),
        );
        assert_eq!(
            mode_for(&p, ConsentKind::Recording),
            ConsentMode::ExplicitConsentRequired
        );
        assert_eq!(
            withdrawal_mode_for(&p, ConsentKind::AiProcessing),
            WithdrawalMode::StopFutureProcessing
        );
    }
}

//! QuestionService — Q&A, separate from chat (Part F §49–§52).
//!
//! States: `OPEN → ANSWERED`, `OPEN|ANSWERED → DISMISSED`, any non-archived →
//! `ARCHIVED` (HIDDEN is a legacy alias treated like DISMISSED). Pinning is a flag on
//! OPEN/ANSWERED questions. Upvotes are one row per (question, participant) and the
//! count is recomputed from the vote rows under the question row lock, so it can never
//! be forged or double counted.
//!
//! Anonymous questions keep the submitter id server-internally (abuse handling, "my
//! question") but no projection, event or error ever returns it to hosts or other
//! participants.

use chrono::Utc;
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    QueryFilter, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, meeting_questions},
    services::{
        actor::ActorContext,
        meeting_core::{lock_conference, Core},
        meeting_events::{action, audit, emit, event, AuditRecord, OutboxRecord},
        meeting_permissions::Permission,
    },
};

use super::{
    metrics::{inc, COLLAB_METRICS as CM},
    Collab, RateAction,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QuestionStatus {
    Open,
    Answered,
    Dismissed,
    Archived,
}

impl QuestionStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "ANSWERED" => QuestionStatus::Answered,
            "DISMISSED" | "HIDDEN" => QuestionStatus::Dismissed,
            "ARCHIVED" => QuestionStatus::Archived,
            _ => QuestionStatus::Open,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            QuestionStatus::Open => "OPEN",
            QuestionStatus::Answered => "ANSWERED",
            QuestionStatus::Dismissed => "DISMISSED",
            QuestionStatus::Archived => "ARCHIVED",
        }
    }

    /// Visible to attendees and accepting votes / pins.
    pub fn is_live(&self) -> bool {
        matches!(self, QuestionStatus::Open | QuestionStatus::Answered)
    }
}

pub fn question_transition_allowed(from: QuestionStatus, to: QuestionStatus) -> bool {
    use QuestionStatus::*;
    matches!(
        (from, to),
        (Open, Answered)
            | (Open, Dismissed)
            | (Answered, Dismissed)
            | (Open, Archived)
            | (Answered, Archived)
            | (Dismissed, Archived)
    )
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitQuestionRequest {
    pub body: String,
    #[serde(default)]
    pub anonymous: bool,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnswerQuestionRequest {
    /// Optional written answer (omit when answered live).
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionView {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub body: String,
    pub anonymous: bool,
    /// Absent for anonymous questions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub status: QuestionStatus,
    pub answer: Option<String>,
    pub answered_at: Option<chrono::DateTime<Utc>>,
    pub pinned: bool,
    pub upvote_count: i32,
    pub voted_by_me: bool,
    pub is_mine: bool,
    pub created_at: chrono::DateTime<Utc>,
    pub version: i32,
}

/// Privacy-safe projection (§50, §66).
pub fn project(
    q: &meeting_questions::Model,
    author_name: Option<&str>,
    viewer: Option<Uuid>,
    voted: bool,
) -> QuestionView {
    let anonymous = q.is_anonymous;
    if anonymous {
        inc(&CM.anonymous_projection_guards);
    }
    QuestionView {
        id: q.id,
        conference_id: q.conference_id,
        body: q.body.clone(),
        anonymous,
        participant_id: (!anonymous).then_some(q.participant_id),
        display_name: if anonymous {
            None
        } else {
            author_name.map(String::from)
        },
        status: QuestionStatus::parse(&q.status),
        answer: q.answer_text.clone(),
        answered_at: q.answered_at,
        pinned: q.pinned_at.is_some(),
        upvote_count: q.upvote_count,
        voted_by_me: voted,
        is_mine: viewer == Some(q.participant_id),
        created_at: q.created_at,
        version: q.version,
    }
}

fn not_found() -> ApiError {
    ApiError::domain(ErrorCode::QuestionNotFound, "Question not found")
}

/// Payload for realtime events: identical projection rules, no viewer state.
fn event_payload(q: &meeting_questions::Model, author: Option<&str>) -> serde_json::Value {
    let v = project(q, author, None, false);
    json!({
        "questionId": v.id,
        "conferenceId": v.conference_id,
        "body": v.body,
        "anonymous": v.anonymous,
        "participantId": v.participant_id,
        "displayName": v.display_name,
        "status": v.status,
        "answer": v.answer,
        "pinned": v.pinned,
        "upvoteCount": v.upvote_count,
        "version": v.version,
    })
}

pub struct QuestionService {
    core: Core,
}

impl QuestionService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    async fn find<C: ConnectionTrait>(
        &self,
        conn: &C,
        conference_id: Uuid,
        id: Uuid,
        lock: bool,
    ) -> ApiResult<meeting_questions::Model> {
        let mut q = meeting_questions::Entity::find_by_id(id)
            .filter(meeting_questions::Column::ConferenceId.eq(conference_id));
        if lock {
            q = q.lock(LockType::Update);
        }
        q.one(conn).await?.ok_or_else(not_found)
    }

    async fn author_name<C: ConnectionTrait>(
        &self,
        conn: &C,
        q: &meeting_questions::Model,
    ) -> ApiResult<Option<String>> {
        if q.is_anonymous {
            return Ok(None);
        }
        Ok(
            conference_participants::Entity::find_by_id(q.participant_id)
                .one(conn)
                .await?
                .map(|p| p.display_name),
        )
    }

    fn record<'a>(
        q: &meeting_questions::Model,
        space_id: Uuid,
        event_type: &'a str,
        payload: serde_json::Value,
    ) -> OutboxRecord<'a> {
        OutboxRecord {
            aggregate_type: "meeting_question",
            aggregate_id: q.id,
            event_type,
            meeting_space_id: Some(space_id),
            conference_id: Some(q.conference_id),
            payload,
        }
    }

    fn require_enabled(c: &Collab) -> ApiResult<()> {
        if c.policy.qna_enabled {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::QnaDisabled,
                "Q&A is disabled in this meeting",
            ))
        }
    }

    pub async fn submit(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: SubmitQuestionRequest,
    ) -> ApiResult<QuestionView> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        c.require(
            Permission::QnaSubmit,
            "Your role does not allow asking questions",
        )?;
        c.require_live()?;
        Self::require_enabled(&c)?;
        if req.anonymous && !c.policy.qna_anonymous_allowed {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Anonymous questions are not allowed in this meeting",
            ));
        }
        let body = req.body.trim().to_string();
        if body.is_empty() || body.chars().count() > 1000 {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Questions must be 1–1000 characters",
            ));
        }
        self.core
            .collab_limiter
            .check(RateAction::QnaSubmit, &me.id.to_string())
            .await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let now = Utc::now();
        let q = meeting_questions::ActiveModel {
            id: Set(Uuid::new_v4()),
            conference_id: Set(conference_id),
            participant_id: Set(me.id),
            body: Set(body),
            is_anonymous: Set(req.anonymous),
            status: Set("OPEN".into()),
            answer_text: Set(None),
            answered_by: Set(None),
            upvote_count: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            pinned_at: Set(None),
            pinned_by: Set(None),
            answered_at: Set(None),
            dismissed_at: Set(None),
            archived_at: Set(None),
            version: Set(1),
        }
        .insert(&txn)
        .await?;
        let author = (!q.is_anonymous).then(|| me.display_name.clone());
        emit(
            &txn,
            Self::record(
                &q,
                conference.meeting_space_id,
                event::QNA_QUESTION_CREATED,
                event_payload(&q, author.as_deref()),
            ),
        )
        .await?;
        txn.commit().await?;
        inc(&CM.qna_submissions);
        Ok(project(&q, author.as_deref(), Some(me.id), false))
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<Vec<QuestionView>> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let moderator = c.allows(Permission::QnaDismiss);
        let viewer = c.participant.as_ref().map(|p| p.id);
        let mut q = meeting_questions::Entity::find()
            .filter(meeting_questions::Column::ConferenceId.eq(conference_id));
        if !moderator {
            q = q.filter(meeting_questions::Column::Status.is_in(["OPEN", "ANSWERED"]));
        }
        let mut rows = q.limit(1000).all(&self.core.db).await?;
        // Pinned first, then most upvoted, then oldest.
        rows.sort_by(|a, b| {
            b.pinned_at
                .is_some()
                .cmp(&a.pinned_at.is_some())
                .then(b.upvote_count.cmp(&a.upvote_count))
                .then(a.created_at.cmp(&b.created_at))
        });
        let names: std::collections::HashMap<Uuid, String> =
            conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                .all(&self.core.db)
                .await?
                .into_iter()
                .map(|p| (p.id, p.display_name))
                .collect();
        let voted: std::collections::HashSet<Uuid> = match viewer {
            Some(v) => self
                .core
                .db
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT question_id FROM meeting_question_votes WHERE conference_id = $1 AND participant_id = $2",
                    [conference_id.into(), v.into()],
                ))
                .await?
                .iter()
                .filter_map(|r| r.try_get_by_index::<Uuid>(0).ok())
                .collect(),
            None => Default::default(),
        };
        Ok(rows
            .iter()
            .map(|q| {
                project(
                    q,
                    names.get(&q.participant_id).map(String::as_str),
                    viewer,
                    voted.contains(&q.id),
                )
            })
            .collect())
    }

    /// Add (true) / remove (false) the caller's upvote. Idempotent.
    pub async fn vote(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        question_id: Uuid,
        up: bool,
    ) -> ApiResult<(QuestionView, bool)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.id;
        let _ = self
            .find(&self.core.db, conference_id, question_id, false)
            .await?;
        c.require(Permission::QnaVote, "Your role does not allow voting")?;
        c.require_live()?;
        Self::require_enabled(&c)?;
        self.core
            .collab_limiter
            .check(RateAction::QnaVote, &me.to_string())
            .await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let q = self.find(&txn, conference_id, question_id, true).await?;
        if up && !QuestionStatus::parse(&q.status).is_live() {
            return Err(ApiError::domain(
                ErrorCode::ConferenceStateConflict,
                "This question no longer accepts votes",
            ));
        }
        if up && q.participant_id == me {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "You cannot upvote your own question",
            ));
        }
        let changed = if up {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO meeting_question_votes (id, question_id, conference_id, participant_id) VALUES ($1,$2,$3,$4) \
                 ON CONFLICT (question_id, participant_id) DO NOTHING",
                [Uuid::new_v4().into(), q.id.into(), conference_id.into(), me.into()],
            ))
            .await?
            .rows_affected()
        } else {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM meeting_question_votes WHERE question_id = $1 AND conference_id = $2 AND participant_id = $3",
                [q.id.into(), conference_id.into(), me.into()],
            ))
            .await?
            .rows_affected()
        } > 0;
        let q = if changed {
            let count = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT count(*)::int FROM meeting_question_votes WHERE question_id = $1",
                    [q.id.into()],
                ))
                .await?
                .and_then(|r| r.try_get_by_index::<i32>(0).ok())
                .unwrap_or(0);
            let mut am: meeting_questions::ActiveModel = q.clone().into();
            am.upvote_count = Set(count);
            am.version = Set(q.version + 1);
            am.updated_at = Set(Utc::now());
            let updated = am.update(&txn).await?;
            emit(
                &txn,
                Self::record(&updated, conference.meeting_space_id, event::QNA_QUESTION_UPVOTED,
                    json!({ "questionId": updated.id, "upvoteCount": updated.upvote_count, "version": updated.version })),
            )
            .await?;
            updated
        } else {
            q
        };
        let author = self.author_name(&txn, &q).await?;
        txn.commit().await?;
        Ok((project(&q, author.as_deref(), Some(me), up), changed))
    }

    async fn moderate(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        question_id: Uuid,
        op: Moderation,
    ) -> ApiResult<(QuestionView, bool)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let _ = self
            .find(&self.core.db, conference_id, question_id, false)
            .await?;
        let permission = match op {
            Moderation::Answer(_) => Permission::QnaAnswer,
            Moderation::Dismiss | Moderation::Archive => Permission::QnaDismiss,
            Moderation::Pin(_) => Permission::QnaPin,
        };
        c.require(permission, "Your role does not allow moderating Q&A")?;
        c.require_configurable()?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let q = self.find(&txn, conference_id, question_id, true).await?;
        let status = QuestionStatus::parse(&q.status);
        let now = Utc::now();
        let mut am: meeting_questions::ActiveModel = q.clone().into();
        let (changed, act, ev) = match &op {
            Moderation::Answer(text) => {
                if status == QuestionStatus::Answered {
                    // Two hosts answering: the first committed answer wins (idempotent).
                    (false, action::QNA_ANSWER, event::QNA_QUESTION_ANSWERED)
                } else if !question_transition_allowed(status, QuestionStatus::Answered) {
                    return Err(ApiError::domain(
                        ErrorCode::ConferenceStateConflict,
                        "Only open questions can be answered",
                    ));
                } else {
                    am.status = Set("ANSWERED".into());
                    am.answer_text = Set(text.clone());
                    am.answered_by = Set(Some(c.audit.reference()));
                    am.answered_at = Set(Some(now));
                    (true, action::QNA_ANSWER, event::QNA_QUESTION_ANSWERED)
                }
            }
            Moderation::Dismiss => {
                if status == QuestionStatus::Dismissed {
                    (false, action::QNA_DISMISS, event::QNA_QUESTION_DISMISSED)
                } else if !question_transition_allowed(status, QuestionStatus::Dismissed) {
                    return Err(ApiError::domain(
                        ErrorCode::ConferenceStateConflict,
                        "This question cannot be dismissed",
                    ));
                } else {
                    am.status = Set("DISMISSED".into());
                    am.dismissed_at = Set(Some(now));
                    am.pinned_at = Set(None);
                    am.pinned_by = Set(None);
                    (true, action::QNA_DISMISS, event::QNA_QUESTION_DISMISSED)
                }
            }
            Moderation::Archive => {
                if status == QuestionStatus::Archived {
                    (false, action::QNA_ARCHIVE, event::QNA_QUESTION_ARCHIVED)
                } else {
                    am.status = Set("ARCHIVED".into());
                    am.archived_at = Set(Some(now));
                    am.pinned_at = Set(None);
                    am.pinned_by = Set(None);
                    (true, action::QNA_ARCHIVE, event::QNA_QUESTION_ARCHIVED)
                }
            }
            Moderation::Pin(on) => {
                let (a, e) = if *on {
                    (action::QNA_PIN, event::QNA_QUESTION_PINNED)
                } else {
                    (action::QNA_UNPIN, event::QNA_QUESTION_UNPINNED)
                };
                if q.pinned_at.is_some() == *on {
                    (false, a, e)
                } else if *on && !status.is_live() {
                    // Pin racing dismiss: a dismissed question can never be pinned.
                    return Err(ApiError::domain(
                        ErrorCode::ConferenceStateConflict,
                        "Only open or answered questions can be pinned",
                    ));
                } else {
                    am.pinned_at = Set(on.then_some(now));
                    am.pinned_by = Set(on.then(|| c.audit.reference()));
                    (true, a, e)
                }
            }
        };
        let q = if changed {
            am.version = Set(q.version + 1);
            am.updated_at = Set(now);
            let updated = am.update(&txn).await?;
            audit(
                &txn,
                &c.audit,
                AuditRecord {
                    meeting_space_id: conference.meeting_space_id,
                    conference_id: Some(conference_id),
                    action: act,
                    target_type: Some("QUESTION"),
                    target_ref: Some(updated.id.to_string()),
                    metadata: json!({}),
                },
            )
            .await?;
            let author = self.author_name(&txn, &updated).await?;
            emit(
                &txn,
                Self::record(
                    &updated,
                    conference.meeting_space_id,
                    ev,
                    event_payload(&updated, author.as_deref()),
                ),
            )
            .await?;
            inc(&CM.qna_moderations);
            updated
        } else {
            q
        };
        let author = self.author_name(&txn, &q).await?;
        txn.commit().await?;
        Ok((
            project(
                &q,
                author.as_deref(),
                c.participant.as_ref().map(|p| p.id),
                false,
            ),
            changed,
        ))
    }

    pub async fn answer(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        question_id: Uuid,
        req: AnswerQuestionRequest,
    ) -> ApiResult<(QuestionView, bool)> {
        let text = req
            .answer
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty());
        if text
            .as_ref()
            .map(|t| t.chars().count() > 2000)
            .unwrap_or(false)
        {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Answers must be at most 2000 characters",
            ));
        }
        self.moderate(ctx, conference_id, question_id, Moderation::Answer(text))
            .await
    }

    pub async fn dismiss(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        question_id: Uuid,
    ) -> ApiResult<(QuestionView, bool)> {
        self.moderate(ctx, conference_id, question_id, Moderation::Dismiss)
            .await
    }

    pub async fn archive(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        question_id: Uuid,
    ) -> ApiResult<(QuestionView, bool)> {
        self.moderate(ctx, conference_id, question_id, Moderation::Archive)
            .await
    }

    pub async fn pin(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        question_id: Uuid,
        on: bool,
    ) -> ApiResult<(QuestionView, bool)> {
        self.moderate(ctx, conference_id, question_id, Moderation::Pin(on))
            .await
    }
}

enum Moderation {
    Answer(Option<String>),
    Dismiss,
    Archive,
    Pin(bool),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(anonymous: bool) -> meeting_questions::Model {
        let now = Utc::now();
        meeting_questions::Model {
            id: Uuid::new_v4(),
            conference_id: Uuid::new_v4(),
            participant_id: Uuid::new_v4(),
            body: "why?".into(),
            is_anonymous: anonymous,
            status: "OPEN".into(),
            answer_text: None,
            answered_by: None,
            upvote_count: 2,
            created_at: now,
            updated_at: now,
            pinned_at: None,
            pinned_by: None,
            answered_at: None,
            dismissed_at: None,
            archived_at: None,
            version: 1,
        }
    }

    #[test]
    fn transitions() {
        use QuestionStatus::*;
        assert!(question_transition_allowed(Open, Answered));
        assert!(question_transition_allowed(Answered, Dismissed));
        assert!(question_transition_allowed(Dismissed, Archived));
        assert!(!question_transition_allowed(Dismissed, Open));
        assert!(!question_transition_allowed(Archived, Answered));
        assert!(!question_transition_allowed(Dismissed, Answered));
        assert_eq!(QuestionStatus::parse("HIDDEN"), Dismissed);
    }

    #[test]
    fn anonymous_projection_never_exposes_submitter() {
        let q = model(true);
        let v = project(&q, Some("Alice"), None, false);
        assert!(v.participant_id.is_none() && v.display_name.is_none());
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            !json.contains(&q.participant_id.to_string()) && !json.contains("Alice"),
            "{json}"
        );
        let p = event_payload(&q, Some("Alice"));
        assert!(p["participantId"].is_null() && p["displayName"].is_null());
        // The submitter still recognises their own question.
        assert!(project(&q, None, Some(q.participant_id), false).is_mine);
        let named = project(&model(false), Some("Bob"), None, false);
        assert_eq!(named.display_name.as_deref(), Some("Bob"));
    }
}

//! PollService — polls and quizzes (Part F §42–§48).
//!
//! Lifecycle: `DRAFT → OPEN → CLOSED → ARCHIVED` (and `DRAFT → ARCHIVED` as cancel).
//! A CLOSED poll is never reopened.
//!
//! **Anonymous privacy model.** Duplicate prevention and answers live in different
//! tables:
//!
//! - `meeting_poll_ballots (poll_id, participant_id)` — *who voted*, used only to block
//!   a second ballot (and to recognise an idempotent retry through a digest of the
//!   client's idempotency key — never of the answers);
//! - `meeting_poll_responses` — *what was answered*. For anonymous polls
//!   `participant_id` is NULL and `created_at` is the poll's open time, so neither an
//!   identity nor a timestamp links an answer to a ballot.
//!
//! No API, event, error or projection ever returns a voter identity for an anonymous
//! poll. Live aggregates of anonymous polls are withheld until the poll is CLOSED
//! (for hosts too), so results cannot be diffed vote-by-vote against who just voted;
//! only the ballot count is live. No compliance identity is retained for anonymous
//! answers.
//!
//! **Quiz.** Correct answers and points are stored in the definition but stripped
//! from every projection until the results state allows them (managers always; others
//! once CLOSED and visibility ≠ HOST_ONLY). Scores are computed server-side only.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rand::seq::SliceRandom;
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::{meeting_poll_responses, meeting_polls},
    services::{
        actor::ActorContext,
        meeting_core::{lock_conference, Core},
        meeting_events::{action, audit, emit_to, event, Audience, AuditRecord, OutboxRecord},
        meeting_permissions::Permission,
    },
};

use super::{
    metrics::{inc, COLLAB_METRICS as CM},
    Collab, RateAction,
};

pub const MAX_QUESTIONS: usize = 20;
pub const MAX_OPTIONS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PollStatus {
    Draft,
    Open,
    Closed,
    Archived,
}

impl PollStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "OPEN" => PollStatus::Open,
            "CLOSED" => PollStatus::Closed,
            "ARCHIVED" => PollStatus::Archived,
            _ => PollStatus::Draft,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PollStatus::Draft => "DRAFT",
            PollStatus::Open => "OPEN",
            PollStatus::Closed => "CLOSED",
            PollStatus::Archived => "ARCHIVED",
        }
    }

    pub fn finished(&self) -> bool {
        matches!(self, PollStatus::Closed | PollStatus::Archived)
    }
}

/// Strict poll transitions.
pub fn poll_transition_allowed(from: PollStatus, to: PollStatus) -> bool {
    use PollStatus::*;
    matches!(
        (from, to),
        (Draft, Open) | (Open, Closed) | (Closed, Archived) | (Draft, Archived)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PollQuestionType {
    SingleChoice,
    MultipleChoice,
    ShortText,
    LongText,
    Rating,
    Ranking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResultsVisibility {
    HostOnly,
    AfterVote,
    AfterClose,
}

impl ResultsVisibility {
    pub fn parse(s: &str) -> Self {
        match s {
            "HOST_ONLY" => ResultsVisibility::HostOnly,
            "AFTER_VOTE" => ResultsVisibility::AfterVote,
            _ => ResultsVisibility::AfterClose,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ResultsVisibility::HostOnly => "HOST_ONLY",
            ResultsVisibility::AfterVote => "AFTER_VOTE",
            ResultsVisibility::AfterClose => "AFTER_CLOSE",
        }
    }
}

// ----------------------------- definition ---------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollQuestionInput {
    #[serde(rename = "type")]
    pub question_type: PollQuestionType,
    pub prompt: String,
    /// Option labels (choice / ranking). Ids `o1…oN` are assigned by the server.
    pub options: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub required: bool,
    pub min_selections: Option<usize>,
    pub max_selections: Option<usize>,
    pub max_length: Option<usize>,
    pub rating_min: Option<i64>,
    pub rating_max: Option<i64>,
    /// Quiz: correct option indexes (choice), the exact order (ranking) or `[value]`
    /// (rating). Never returned before the results state allows it.
    pub correct: Option<Vec<i64>>,
    pub points: Option<f64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollOption {
    pub id: String,
    pub label: String,
}

/// Stored, validated question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollQuestion {
    pub id: String,
    #[serde(rename = "type")]
    pub question_type: PollQuestionType,
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<PollOption>,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_selections: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_selections: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_max: Option<i64>,
    /// Option ids (choice/ranking) or rating value as string. Stripped from public views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correct: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PollDefinition {
    pub questions: Vec<PollQuestion>,
}

impl PollDefinition {
    pub fn from_json(v: &Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    /// Projection without quiz metadata.
    pub fn public(&self) -> Self {
        Self {
            questions: self
                .questions
                .iter()
                .map(|q| PollQuestion {
                    correct: None,
                    points: None,
                    ..q.clone()
                })
                .collect(),
        }
    }
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::domain(ErrorCode::ValidationError, msg)
}

/// Validate creator input into a stored definition (no executable metadata accepted).
pub fn build_definition(input: &[PollQuestionInput], quiz: bool) -> ApiResult<PollDefinition> {
    if input.is_empty() || input.len() > MAX_QUESTIONS {
        return Err(invalid(format!("A poll needs 1–{MAX_QUESTIONS} questions")));
    }
    let mut questions = vec![];
    let mut any_correct = false;
    for (qi, q) in input.iter().enumerate() {
        let prompt = q.prompt.trim().to_string();
        if prompt.is_empty() || prompt.chars().count() > 500 {
            return Err(invalid("Question prompts must be 1–500 characters"));
        }
        let choice = matches!(
            q.question_type,
            PollQuestionType::SingleChoice
                | PollQuestionType::MultipleChoice
                | PollQuestionType::Ranking
        );
        let options: Vec<PollOption> = if choice {
            let labels = q.options.clone().unwrap_or_default();
            if labels.len() < 2 || labels.len() > MAX_OPTIONS {
                return Err(invalid(format!(
                    "Choice questions need 2–{MAX_OPTIONS} options"
                )));
            }
            let mut seen = HashSet::new();
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let l = l.trim().to_string();
                    if l.is_empty() || l.chars().count() > 200 || !seen.insert(l.to_lowercase()) {
                        return Err(invalid("Options must be unique and 1–200 characters"));
                    }
                    Ok(PollOption {
                        id: format!("o{}", i + 1),
                        label: l,
                    })
                })
                .collect::<ApiResult<_>>()?
        } else {
            if q.options.is_some() {
                return Err(invalid("Only choice and ranking questions take options"));
            }
            vec![]
        };
        let n = options.len();
        let (mut min_sel, mut max_sel, mut max_len, mut rmin, mut rmax) =
            (None, None, None, None, None);
        match q.question_type {
            PollQuestionType::MultipleChoice => {
                let lo = q.min_selections.unwrap_or(1);
                let hi = q.max_selections.unwrap_or(n);
                if lo < 1 || hi > n || lo > hi {
                    return Err(invalid("Invalid selection bounds"));
                }
                min_sel = Some(lo);
                max_sel = Some(hi);
            }
            PollQuestionType::ShortText | PollQuestionType::LongText => {
                let cap = if q.question_type == PollQuestionType::ShortText {
                    280
                } else {
                    4000
                };
                let v = q.max_length.unwrap_or(if cap == 280 { 280 } else { 2000 });
                if v < 1 || v > cap {
                    return Err(invalid(format!("maxLength must be 1–{cap}")));
                }
                max_len = Some(v);
            }
            PollQuestionType::Rating => {
                let lo = q.rating_min.unwrap_or(1);
                let hi = q.rating_max.unwrap_or(5);
                if !(0..=10).contains(&lo) || !(0..=10).contains(&hi) || lo >= hi {
                    return Err(invalid("Rating range must be within 0–10 with min < max"));
                }
                rmin = Some(lo);
                rmax = Some(hi);
            }
            _ => {}
        }
        let correct = match &q.correct {
            None => None,
            Some(_) if !quiz => {
                return Err(invalid("Correct answers are only allowed in quiz mode"))
            }
            Some(c) => {
                any_correct = true;
                Some(match q.question_type {
                    PollQuestionType::SingleChoice | PollQuestionType::MultipleChoice => {
                        let set: HashSet<i64> = c.iter().copied().collect();
                        if set.len() != c.len() || c.iter().any(|i| *i < 0 || *i as usize >= n) {
                            return Err(invalid("Correct answers must be distinct option indexes"));
                        }
                        if q.question_type == PollQuestionType::SingleChoice && c.len() != 1 {
                            return Err(invalid(
                                "A single-choice question has exactly one correct option",
                            ));
                        }
                        let mut ids: Vec<String> =
                            c.iter().map(|i| format!("o{}", i + 1)).collect();
                        ids.sort();
                        ids
                    }
                    PollQuestionType::Ranking => {
                        let set: HashSet<i64> = c.iter().copied().collect();
                        if c.len() != n
                            || set.len() != n
                            || c.iter().any(|i| *i < 0 || *i as usize >= n)
                        {
                            return Err(invalid(
                                "A ranking answer orders every option exactly once",
                            ));
                        }
                        c.iter().map(|i| format!("o{}", i + 1)).collect()
                    }
                    PollQuestionType::Rating => {
                        if c.len() != 1 || c[0] < rmin.unwrap_or(0) || c[0] > rmax.unwrap_or(10) {
                            return Err(invalid("The correct rating must be one value in range"));
                        }
                        vec![c[0].to_string()]
                    }
                    _ => return Err(invalid("Text questions cannot be auto-scored")),
                })
            }
        };
        let points = match q.points {
            None => correct.as_ref().map(|_| 1.0),
            Some(_) if correct.is_none() => return Err(invalid("Points require a correct answer")),
            Some(p) if !(0.0..=100.0).contains(&p) || !p.is_finite() => {
                return Err(invalid("points must be within 0–100"))
            }
            Some(p) => Some(p),
        };
        questions.push(PollQuestion {
            id: format!("q{}", qi + 1),
            question_type: q.question_type,
            prompt,
            options,
            required: q.required,
            min_selections: min_sel,
            max_selections: max_sel,
            max_length: max_len,
            rating_min: rmin,
            rating_max: rmax,
            correct,
            points,
        });
    }
    if quiz && !any_correct {
        return Err(invalid(
            "A quiz needs at least one question with a correct answer",
        ));
    }
    Ok(PollDefinition { questions })
}

// ----------------------------- responses ----------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollAnswerInput {
    pub question_id: String,
    pub option_ids: Option<Vec<String>>,
    pub text: Option<String>,
    pub rating: Option<i64>,
    pub ranking: Option<Vec<String>>,
}

/// Validated answer, as stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<Vec<String>>,
}

/// Validate a response against the definition (§44). Returns answers by question id.
pub fn validate_response(
    def: &PollDefinition,
    answers: &[PollAnswerInput],
) -> ApiResult<HashMap<String, PollAnswer>> {
    let bad = |m: &str| {
        inc(&CM.poll_validation_failures);
        ApiError::domain(ErrorCode::PollResponseInvalid, m.to_string())
    };
    let by_id: HashMap<&str, &PollQuestion> =
        def.questions.iter().map(|q| (q.id.as_str(), q)).collect();
    let mut out = HashMap::new();
    for a in answers {
        let q = by_id
            .get(a.question_id.as_str())
            .ok_or_else(|| bad("Unknown question"))?;
        if out.contains_key(&a.question_id) {
            return Err(bad("Each question can be answered once"));
        }
        let option_ids: HashSet<&str> = q.options.iter().map(|o| o.id.as_str()).collect();
        let answer = match q.question_type {
            PollQuestionType::SingleChoice | PollQuestionType::MultipleChoice => {
                if a.text.is_some() || a.rating.is_some() || a.ranking.is_some() {
                    return Err(bad("Choice questions take optionIds only"));
                }
                let ids = a
                    .option_ids
                    .clone()
                    .ok_or_else(|| bad("optionIds required"))?;
                let set: HashSet<&str> = ids.iter().map(String::as_str).collect();
                if set.len() != ids.len() || !set.iter().all(|i| option_ids.contains(i)) {
                    return Err(bad(
                        "Selected options must be distinct options of the question",
                    ));
                }
                if q.question_type == PollQuestionType::SingleChoice && ids.len() != 1 {
                    return Err(bad("Select exactly one option"));
                }
                if q.question_type == PollQuestionType::MultipleChoice {
                    let (lo, hi) = (
                        q.min_selections.unwrap_or(1),
                        q.max_selections.unwrap_or(q.options.len()),
                    );
                    if ids.len() < lo || ids.len() > hi {
                        return Err(bad("Number of selected options is out of bounds"));
                    }
                }
                let mut ids = ids;
                ids.sort();
                PollAnswer {
                    option_ids: Some(ids),
                    text: None,
                    rating: None,
                    ranking: None,
                }
            }
            PollQuestionType::ShortText | PollQuestionType::LongText => {
                if a.option_ids.is_some() || a.rating.is_some() || a.ranking.is_some() {
                    return Err(bad("Text questions take text only"));
                }
                let t = a.text.clone().unwrap_or_default().trim().to_string();
                let max = q.max_length.unwrap_or(280);
                if t.is_empty() || t.chars().count() > max {
                    return Err(bad("Answer text is empty or too long"));
                }
                PollAnswer {
                    option_ids: None,
                    text: Some(t),
                    rating: None,
                    ranking: None,
                }
            }
            PollQuestionType::Rating => {
                if a.option_ids.is_some() || a.text.is_some() || a.ranking.is_some() {
                    return Err(bad("Rating questions take rating only"));
                }
                let r = a.rating.ok_or_else(|| bad("rating required"))?;
                if r < q.rating_min.unwrap_or(1) || r > q.rating_max.unwrap_or(5) {
                    return Err(bad("Rating is out of range"));
                }
                PollAnswer {
                    option_ids: None,
                    text: None,
                    rating: Some(r),
                    ranking: None,
                }
            }
            PollQuestionType::Ranking => {
                if a.option_ids.is_some() || a.text.is_some() || a.rating.is_some() {
                    return Err(bad("Ranking questions take ranking only"));
                }
                let r = a.ranking.clone().ok_or_else(|| bad("ranking required"))?;
                let set: HashSet<&str> = r.iter().map(String::as_str).collect();
                if r.len() != q.options.len()
                    || set.len() != r.len()
                    || !set.iter().all(|i| option_ids.contains(i))
                {
                    return Err(bad("A ranking orders every option exactly once"));
                }
                PollAnswer {
                    option_ids: None,
                    text: None,
                    rating: None,
                    ranking: Some(r),
                }
            }
        };
        out.insert(a.question_id.clone(), answer);
    }
    if let Some(missing) = def
        .questions
        .iter()
        .find(|q| q.required && !out.contains_key(&q.id))
    {
        return Err(bad(&format!("Question {} is required", missing.id)));
    }
    Ok(out)
}

/// Server-side quiz score: (score, max). Text questions never score.
pub fn score(def: &PollDefinition, answers: &HashMap<String, PollAnswer>) -> (f64, f64) {
    let mut total = 0.0;
    let mut max = 0.0;
    for q in &def.questions {
        let (Some(correct), Some(points)) = (&q.correct, q.points) else {
            continue;
        };
        max += points;
        let Some(a) = answers.get(&q.id) else {
            continue;
        };
        let right = match q.question_type {
            PollQuestionType::SingleChoice | PollQuestionType::MultipleChoice => {
                a.option_ids.as_ref() == Some(correct)
            }
            PollQuestionType::Ranking => a.ranking.as_ref() == Some(correct),
            PollQuestionType::Rating => a.rating.map(|r| r.to_string()) == correct.first().cloned(),
            _ => false,
        };
        if right {
            total += points;
        }
    }
    (total, max)
}

// ----------------------------- results ------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionResult {
    pub question_id: String,
    #[serde(rename = "type")]
    pub question_type: PollQuestionType,
    pub answered: usize,
    /// Choice: option id → votes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option_counts: Option<HashMap<String, usize>>,
    /// Rating: value → count, and the average.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_distribution: Option<HashMap<String, usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_average: Option<f64>,
    /// Ranking: option id → average position (1 = first).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_positions: Option<HashMap<String, f64>>,
    /// Text answers (managers only; never attributed for anonymous polls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texts: Option<Vec<TextAnswer>>,
    /// Quiz: correct answer, once the results state allows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correct: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextAnswer {
    pub text: String,
    /// Only for non-anonymous polls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollResults {
    pub poll_id: Uuid,
    pub status: PollStatus,
    pub anonymous: bool,
    pub response_count: usize,
    pub questions: Vec<QuestionResult>,
    /// Quiz: average score and maximum (aggregate only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_score: Option<f64>,
    /// The caller's own score (non-anonymous quiz, results visible).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_score: Option<f64>,
}

/// One answer row as aggregated (identity only present for non-anonymous polls).
pub struct ResponseRow {
    pub participant_id: Option<Uuid>,
    pub answers: HashMap<String, PollAnswer>,
    pub score: Option<f64>,
}

/// Who is looking at aggregated results.
#[derive(Debug, Clone, Copy)]
pub struct ResultsViewer {
    /// Poll manager: text answers are included.
    pub manager: bool,
    /// Results state reached: quiz correct answers and scores are included.
    pub reveal: bool,
    /// The viewing participant (own score for non-anonymous quizzes).
    pub me: Option<Uuid>,
}

/// Aggregate results for `viewer`.
pub fn aggregate(
    poll_id: Uuid,
    status: PollStatus,
    anonymous: bool,
    def: &PollDefinition,
    rows: &[ResponseRow],
    viewer: ResultsViewer,
) -> PollResults {
    let ResultsViewer {
        manager,
        reveal,
        me,
    } = viewer;
    let mut questions = vec![];
    for q in &def.questions {
        let answers: Vec<(&ResponseRow, &PollAnswer)> = rows
            .iter()
            .filter_map(|r| r.answers.get(&q.id).map(|a| (r, a)))
            .collect();
        let mut res = QuestionResult {
            question_id: q.id.clone(),
            question_type: q.question_type,
            answered: answers.len(),
            option_counts: None,
            rating_distribution: None,
            rating_average: None,
            average_positions: None,
            texts: None,
            correct: if reveal { q.correct.clone() } else { None },
        };
        match q.question_type {
            PollQuestionType::SingleChoice | PollQuestionType::MultipleChoice => {
                let mut counts: HashMap<String, usize> =
                    q.options.iter().map(|o| (o.id.clone(), 0)).collect();
                for (_, a) in &answers {
                    for id in a.option_ids.iter().flatten() {
                        *counts.entry(id.clone()).or_default() += 1;
                    }
                }
                res.option_counts = Some(counts);
            }
            PollQuestionType::Rating => {
                let mut dist = HashMap::new();
                let mut sum = 0i64;
                for (_, a) in &answers {
                    if let Some(r) = a.rating {
                        *dist.entry(r.to_string()).or_insert(0usize) += 1;
                        sum += r;
                    }
                }
                res.rating_average =
                    (!answers.is_empty()).then(|| sum as f64 / answers.len() as f64);
                res.rating_distribution = Some(dist);
            }
            PollQuestionType::Ranking => {
                let mut pos: HashMap<String, (f64, usize)> = HashMap::new();
                for (_, a) in &answers {
                    for (i, id) in a.ranking.iter().flatten().enumerate() {
                        let e = pos.entry(id.clone()).or_insert((0.0, 0));
                        e.0 += (i + 1) as f64;
                        e.1 += 1;
                    }
                }
                res.average_positions = Some(
                    pos.into_iter()
                        .map(|(k, (s, n))| (k, s / n as f64))
                        .collect(),
                );
            }
            PollQuestionType::ShortText | PollQuestionType::LongText => {
                if manager {
                    let mut texts: Vec<TextAnswer> = answers
                        .iter()
                        .filter_map(|(r, a)| {
                            a.text.clone().map(|t| TextAnswer {
                                text: t,
                                participant_id: if anonymous { None } else { r.participant_id },
                            })
                        })
                        .take(500)
                        .collect();
                    if anonymous {
                        inc(&CM.anonymous_projection_guards);
                        texts.shuffle(&mut rand::thread_rng());
                    }
                    res.texts = Some(texts);
                }
            }
        }
        questions.push(res);
    }
    let scored: Vec<f64> = rows.iter().filter_map(|r| r.score).collect();
    let quiz_max = def.questions.iter().filter_map(|q| q.points).sum::<f64>();
    let quiz = def.questions.iter().any(|q| q.correct.is_some());
    PollResults {
        poll_id,
        status,
        anonymous,
        response_count: rows.len(),
        questions,
        average_score: (quiz && reveal && !scored.is_empty())
            .then(|| scored.iter().sum::<f64>() / scored.len() as f64),
        max_score: (quiz && reveal).then_some(quiz_max),
        my_score: if quiz && reveal && !anonymous {
            me.and_then(|m| {
                rows.iter()
                    .find(|r| r.participant_id == Some(m))
                    .and_then(|r| r.score)
            })
        } else {
            None
        },
    }
}

/// Whether a viewer may retrieve aggregate results (§47) — anonymous polls only once
/// finished, for everyone.
pub fn results_visible(
    status: PollStatus,
    visibility: ResultsVisibility,
    anonymous: bool,
    manager: bool,
    has_voted: bool,
) -> bool {
    if status == PollStatus::Draft {
        return manager;
    }
    if anonymous && !status.finished() {
        return false;
    }
    if manager {
        return true;
    }
    match visibility {
        ResultsVisibility::HostOnly => false,
        ResultsVisibility::AfterVote => has_voted || status.finished(),
        ResultsVisibility::AfterClose => status.finished(),
    }
}

/// Whether quiz correct answers may be shown to a viewer.
pub fn reveal_answers(status: PollStatus, visibility: ResultsVisibility, manager: bool) -> bool {
    manager || (status.finished() && visibility != ResultsVisibility::HostOnly)
}

// ----------------------------- service ------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePollRequest {
    pub title: String,
    pub questions: Vec<PollQuestionInput>,
    #[serde(default)]
    pub anonymous: bool,
    #[serde(default)]
    pub quiz_mode: bool,
    pub results_visibility: Option<ResultsVisibility>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitPollResponseRequest {
    pub answers: Vec<PollAnswerInput>,
    /// Client-chosen retry key: a retry with the same key is acknowledged
    /// idempotently instead of failing as a duplicate ballot.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollView {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub title: String,
    pub status: PollStatus,
    pub anonymous: bool,
    pub quiz_mode: bool,
    pub results_visibility: ResultsVisibility,
    pub questions: Vec<PollQuestion>,
    pub response_count: i64,
    pub has_responded: bool,
    pub created_at: chrono::DateTime<Utc>,
    pub opened_at: Option<chrono::DateTime<Utc>>,
    pub closed_at: Option<chrono::DateTime<Utc>>,
    pub archived_at: Option<chrono::DateTime<Utc>>,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollResponseReceipt {
    pub poll_id: Uuid,
    pub recorded: bool,
    /// true when this request was a retry of an already-recorded ballot.
    pub idempotent_replay: bool,
    /// Quiz score, only where the results state already allows it (AFTER_VOTE,
    /// non-anonymous).
    pub score: Option<f64>,
    pub max_score: Option<f64>,
}

fn not_found() -> ApiError {
    ApiError::domain(ErrorCode::PollNotFound, "Poll not found")
}

fn digest(key: &str) -> String {
    Sha256::digest(format!("cfmeet-poll-idem-v1:{key}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub struct PollService {
    core: Core,
}

impl PollService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn is_manager(c: &Collab) -> bool {
        c.allows(Permission::PollViewResults)
    }

    async fn find<C: ConnectionTrait>(
        &self,
        conn: &C,
        conference_id: Uuid,
        poll_id: Uuid,
        lock: bool,
    ) -> ApiResult<meeting_polls::Model> {
        let mut q = meeting_polls::Entity::find_by_id(poll_id)
            .filter(meeting_polls::Column::ConferenceId.eq(conference_id));
        if lock {
            q = q.lock(LockType::Update);
        }
        q.one(conn).await?.ok_or_else(not_found)
    }

    async fn ballot_count<C: ConnectionTrait>(&self, conn: &C, poll_id: Uuid) -> ApiResult<i64> {
        let row = conn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*) FROM meeting_poll_ballots WHERE poll_id = $1",
                [poll_id.into()],
            ))
            .await?;
        Ok(row
            .and_then(|r| r.try_get_by_index::<i64>(0).ok())
            .unwrap_or(0))
    }

    async fn has_voted<C: ConnectionTrait>(
        &self,
        conn: &C,
        poll_id: Uuid,
        participant: Option<Uuid>,
    ) -> ApiResult<bool> {
        let Some(p) = participant else {
            return Ok(false);
        };
        let row = conn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM meeting_poll_ballots WHERE poll_id = $1 AND participant_id = $2",
                [poll_id.into(), p.into()],
            ))
            .await?;
        Ok(row.is_some())
    }

    async fn view_of(&self, c: &Collab, poll: &meeting_polls::Model) -> ApiResult<PollView> {
        let status = PollStatus::parse(&poll.status);
        let visibility = ResultsVisibility::parse(&poll.results_visibility);
        let def = PollDefinition::from_json(&poll.definition);
        let manager = Self::is_manager(c);
        let def = if reveal_answers(status, visibility, manager) {
            def
        } else {
            def.public()
        };
        Ok(PollView {
            id: poll.id,
            conference_id: poll.conference_id,
            title: poll.question.clone(),
            status,
            anonymous: poll.is_anonymous,
            quiz_mode: poll.quiz_mode,
            results_visibility: visibility,
            questions: def.questions,
            response_count: self.ballot_count(&self.core.db, poll.id).await?,
            has_responded: self
                .has_voted(&self.core.db, poll.id, c.participant.as_ref().map(|p| p.id))
                .await?,
            created_at: poll.created_at,
            opened_at: poll.opened_at,
            closed_at: poll.closed_at,
            archived_at: poll.archived_at,
            version: poll.version,
        })
    }

    fn public_payload(poll: &meeting_polls::Model) -> Value {
        json!({
            "pollId": poll.id,
            "conferenceId": poll.conference_id,
            "title": poll.question,
            "status": poll.status,
            "anonymous": poll.is_anonymous,
            "quizMode": poll.quiz_mode,
            "resultsVisibility": poll.results_visibility,
            "questions": PollDefinition::from_json(&poll.definition).public().questions,
            "version": poll.version,
        })
    }

    fn record<'a>(
        poll: &meeting_polls::Model,
        space_id: Uuid,
        event_type: &'a str,
        payload: Value,
    ) -> OutboxRecord<'a> {
        OutboxRecord {
            aggregate_type: "meeting_poll",
            aggregate_id: poll.id,
            event_type,
            meeting_space_id: Some(space_id),
            conference_id: Some(poll.conference_id),
            payload,
        }
    }

    fn managers() -> Audience {
        Audience::Permission {
            permission: Permission::PollViewResults.key().into(),
        }
    }

    pub async fn create(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: CreatePollRequest,
    ) -> ApiResult<PollView> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::PollCreate,
            "Your role does not allow creating polls",
        )?;
        c.require_configurable()?;
        if !c.policy.polls_enabled {
            return Err(ApiError::domain(
                ErrorCode::PollsDisabled,
                "Polls are disabled in this meeting",
            ));
        }
        let title = req.title.trim().to_string();
        if title.is_empty() || title.chars().count() > 300 {
            return Err(invalid("Poll titles must be 1–300 characters"));
        }
        let def = build_definition(&req.questions, req.quiz_mode)?;
        let types: HashSet<PollQuestionType> =
            def.questions.iter().map(|q| q.question_type).collect();
        let poll_type = match (types.len(), types.iter().next()) {
            (1, Some(PollQuestionType::SingleChoice)) => "SINGLE",
            (1, Some(PollQuestionType::MultipleChoice)) => "MULTIPLE",
            _ => "MIXED",
        };
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let now = Utc::now();
        let poll = meeting_polls::ActiveModel {
            id: Set(Uuid::new_v4()),
            conference_id: Set(conference_id),
            created_by_participant_id: Set(c.participant.as_ref().map(|p| p.id)),
            question: Set(title),
            options: Set(json!([])),
            poll_type: Set(poll_type.into()),
            is_anonymous: Set(req.anonymous),
            status: Set("DRAFT".into()),
            created_at: Set(now),
            opened_at: Set(None),
            closed_at: Set(None),
            version: Set(1),
            definition: Set(serde_json::to_value(&def).unwrap_or_else(|_| json!({}))),
            quiz_mode: Set(req.quiz_mode),
            results_visibility: Set(req
                .results_visibility
                .unwrap_or(ResultsVisibility::AfterClose)
                .as_str()
                .into()),
            created_by: Set(Some(c.audit.reference())),
            archived_at: Set(None),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::POLL_CREATE,
                target_type: Some("POLL"),
                target_ref: Some(poll.id.to_string()),
                metadata: json!({ "questions": def.questions.len(), "anonymous": req.anonymous, "quiz": req.quiz_mode }),
            },
        )
        .await?;
        emit_to(
            &txn,
            Self::record(
                &poll,
                conference.meeting_space_id,
                event::POLL_CREATED,
                Self::public_payload(&poll),
            ),
            Some(Audience::Permission {
                permission: Permission::PollCreate.key().into(),
            }),
        )
        .await?;
        txn.commit().await?;
        self.view_of(&c, &poll).await
    }

    pub async fn list(&self, ctx: &ActorContext, conference_id: Uuid) -> ApiResult<Vec<PollView>> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let manager = Self::is_manager(&c) || c.allows(Permission::PollCreate);
        let mut q = meeting_polls::Entity::find()
            .filter(meeting_polls::Column::ConferenceId.eq(conference_id));
        if !manager {
            q = q.filter(meeting_polls::Column::Status.ne("DRAFT"));
        }
        let polls = q
            .order_by_asc(meeting_polls::Column::CreatedAt)
            .limit(200)
            .all(&self.core.db)
            .await?;
        let mut out = vec![];
        for p in &polls {
            out.push(self.view_of(&c, p).await?);
        }
        Ok(out)
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        poll_id: Uuid,
    ) -> ApiResult<PollView> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let poll = self
            .find(&self.core.db, conference_id, poll_id, false)
            .await?;
        if poll.status == "DRAFT" && !(Self::is_manager(&c) || c.allows(Permission::PollCreate)) {
            return Err(not_found());
        }
        self.view_of(&c, &poll).await
    }

    /// Lifecycle transition (open / close / archive). Idempotent on the target state.
    pub async fn transition(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        poll_id: Uuid,
        to: PollStatus,
    ) -> ApiResult<(PollView, bool)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let _ = self
            .find(&self.core.db, conference_id, poll_id, false)
            .await?;
        let permission = match to {
            PollStatus::Open => Permission::PollOpen,
            _ => Permission::PollClose,
        };
        c.require(permission, "Your role does not allow managing polls")?;
        if to == PollStatus::Open {
            c.require_live()?;
            if !c.policy.polls_enabled {
                return Err(ApiError::domain(
                    ErrorCode::PollsDisabled,
                    "Polls are disabled in this meeting",
                ));
            }
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let poll = self.find(&txn, conference_id, poll_id, true).await?;
        let from = PollStatus::parse(&poll.status);
        if from == to {
            txn.commit().await?;
            return Ok((self.view_of(&c, &poll).await?, false));
        }
        if !poll_transition_allowed(from, to) {
            return Err(ApiError::domain(
                if to == PollStatus::Open || from == PollStatus::Draft {
                    ErrorCode::PollNotOpen
                } else {
                    ErrorCode::ConferenceStateConflict
                },
                format!("A {} poll cannot become {}", from.as_str(), to.as_str()),
            ));
        }
        let now = Utc::now();
        let mut am: meeting_polls::ActiveModel = poll.clone().into();
        am.status = Set(to.as_str().into());
        match to {
            PollStatus::Open => am.opened_at = Set(Some(now)),
            PollStatus::Closed => am.closed_at = Set(Some(now)),
            PollStatus::Archived => am.archived_at = Set(Some(now)),
            PollStatus::Draft => {}
        }
        am.version = Set(poll.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        let (act, ev) = match to {
            PollStatus::Open => (action::POLL_OPEN, event::POLL_STARTED),
            PollStatus::Closed => (action::POLL_CLOSE, event::POLL_CLOSED),
            _ => (action::POLL_ARCHIVE, event::POLL_ARCHIVED),
        };
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: act,
                target_type: Some("POLL"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({ "from": from.as_str() }),
            },
        )
        .await?;
        let mut payload = Self::public_payload(&updated);
        let visibility = ResultsVisibility::parse(&updated.results_visibility);
        if to == PollStatus::Closed {
            let results = self
                .results_for(
                    &txn,
                    &updated,
                    false,
                    reveal_answers(to, visibility, false),
                    None,
                )
                .await?;
            if visibility != ResultsVisibility::HostOnly {
                payload["results"] = serde_json::to_value(&results).unwrap_or(Value::Null);
            }
            let manager_results = self.results_for(&txn, &updated, true, true, None).await?;
            emit_to(
                &txn,
                Self::record(
                    &updated,
                    conference.meeting_space_id,
                    event::POLL_RESULTS_UPDATED,
                    json!({ "pollId": updated.id, "results": manager_results }),
                ),
                Some(Self::managers()),
            )
            .await?;
        }
        let audience = if from == PollStatus::Draft && to == PollStatus::Archived {
            Some(Audience::Permission {
                permission: Permission::PollCreate.key().into(),
            })
        } else {
            None
        };
        emit_to(
            &txn,
            Self::record(&updated, conference.meeting_space_id, ev, payload),
            audience,
        )
        .await?;
        txn.commit().await?;
        if to == PollStatus::Open {
            inc(&CM.polls_opened);
        }
        Ok((self.view_of(&c, &updated).await?, true))
    }

    async fn rows<C: ConnectionTrait>(
        &self,
        conn: &C,
        poll: &meeting_polls::Model,
    ) -> ApiResult<Vec<ResponseRow>> {
        Ok(meeting_poll_responses::Entity::find()
            .filter(meeting_poll_responses::Column::PollId.eq(poll.id))
            .filter(meeting_poll_responses::Column::ConferenceId.eq(poll.conference_id))
            .all(conn)
            .await?
            .into_iter()
            .map(|r| ResponseRow {
                // Defense in depth: anonymous polls never project identity.
                participant_id: if poll.is_anonymous {
                    None
                } else {
                    r.participant_id
                },
                answers: serde_json::from_value(r.answers).unwrap_or_default(),
                score: r.score,
            })
            .collect())
    }

    /// Part G POLL_REPORT: aggregate results only. Anonymous polls never carry
    /// identity, and results of polls that are not closed are not exported.
    pub async fn report_results<C: ConnectionTrait>(
        &self,
        conn: &C,
        poll: &meeting_polls::Model,
    ) -> ApiResult<Option<PollResults>> {
        if !matches!(
            PollStatus::parse(&poll.status),
            PollStatus::Closed | PollStatus::Archived
        ) {
            return Ok(None);
        }
        Ok(Some(self.results_for(conn, poll, true, true, None).await?))
    }

    async fn results_for<C: ConnectionTrait>(
        &self,
        conn: &C,
        poll: &meeting_polls::Model,
        manager: bool,
        reveal: bool,
        me: Option<Uuid>,
    ) -> ApiResult<PollResults> {
        let rows = self.rows(conn, poll).await?;
        Ok(aggregate(
            poll.id,
            PollStatus::parse(&poll.status),
            poll.is_anonymous,
            &PollDefinition::from_json(&poll.definition),
            &rows,
            ResultsViewer {
                manager,
                reveal,
                me,
            },
        ))
    }

    pub async fn results(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        poll_id: Uuid,
    ) -> ApiResult<PollResults> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let poll = self
            .find(&self.core.db, conference_id, poll_id, false)
            .await?;
        let manager = Self::is_manager(&c);
        let status = PollStatus::parse(&poll.status);
        if status == PollStatus::Draft && !manager {
            return Err(not_found());
        }
        let visibility = ResultsVisibility::parse(&poll.results_visibility);
        let me = c.participant.as_ref().map(|p| p.id);
        let voted = self.has_voted(&self.core.db, poll.id, me).await?;
        if !results_visible(status, visibility, poll.is_anonymous, manager, voted) {
            return Err(ApiError::domain(
                ErrorCode::PollResultsNotVisible,
                if poll.is_anonymous && !status.finished() {
                    "Results of anonymous polls are available once the poll is closed"
                } else {
                    "Results are not visible yet"
                },
            ));
        }
        self.results_for(
            &self.core.db,
            &poll,
            manager,
            reveal_answers(status, visibility, manager),
            me,
        )
        .await
    }

    pub async fn respond(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        poll_id: Uuid,
        req: SubmitPollResponseRequest,
    ) -> ApiResult<PollResponseReceipt> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.id;
        let _ = self
            .find(&self.core.db, conference_id, poll_id, false)
            .await?;
        c.require(
            Permission::PollRespond,
            "Your role does not allow answering polls",
        )?;
        c.require_live()?;
        if !c.policy.polls_enabled {
            return Err(ApiError::domain(
                ErrorCode::PollsDisabled,
                "Polls are disabled in this meeting",
            ));
        }
        if let Some(k) = &req.idempotency_key {
            if k.is_empty() || k.len() > 128 {
                return Err(invalid("idempotencyKey must be 1–128 characters"));
            }
        }
        self.core
            .collab_limiter
            .check(RateAction::PollResponse, &me.to_string())
            .await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let poll = self.find(&txn, conference_id, poll_id, true).await?;
        if poll.status != "OPEN" {
            return Err(ApiError::domain(
                ErrorCode::PollNotOpen,
                "This poll is not open",
            ));
        }
        let def = PollDefinition::from_json(&poll.definition);
        let answers = validate_response(&def, &req.answers)?;
        let visibility = ResultsVisibility::parse(&poll.results_visibility);
        let request_digest = req.idempotency_key.as_deref().map(digest);
        let inserted = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO meeting_poll_ballots (poll_id, conference_id, participant_id, request_digest) \
                 VALUES ($1,$2,$3,$4) ON CONFLICT (poll_id, participant_id) DO NOTHING",
                [poll.id.into(), conference_id.into(), me.into(), request_digest.clone().into()],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            let existing = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT request_digest FROM meeting_poll_ballots WHERE poll_id = $1 AND participant_id = $2",
                    [poll.id.into(), me.into()],
                ))
                .await?
                .and_then(|r| r.try_get_by_index::<Option<String>>(0).ok())
                .flatten();
            txn.commit().await?;
            if request_digest.is_some() && existing == request_digest {
                return Ok(PollResponseReceipt {
                    poll_id,
                    recorded: true,
                    idempotent_replay: true,
                    score: None,
                    max_score: None,
                });
            }
            return Err(ApiError::domain(
                ErrorCode::PollAlreadyResponded,
                "You have already answered this poll",
            ));
        }
        let (s, max) = if poll.quiz_mode {
            score(&def, &answers)
        } else {
            (0.0, 0.0)
        };
        meeting_poll_responses::ActiveModel {
            id: Set(Uuid::new_v4()),
            poll_id: Set(poll.id),
            conference_id: Set(conference_id),
            participant_id: Set(if poll.is_anonymous { None } else { Some(me) }),
            selected_options: Set(json!([])),
            // Anonymous: no per-answer timestamp that could be matched to a ballot.
            created_at: Set(if poll.is_anonymous {
                poll.opened_at.unwrap_or(poll.created_at)
            } else {
                Utc::now()
            }),
            answers: Set(serde_json::to_value(&answers).unwrap_or_else(|_| json!({}))),
            score: Set(poll.quiz_mode.then_some(s)),
            max_score: Set(poll.quiz_mode.then_some(max)),
        }
        .insert(&txn)
        .await?;
        emit_to(
            &txn,
            Self::record(
                &poll,
                conference.meeting_space_id,
                event::POLL_RESPONSE_RECORDED,
                json!({ "pollId": poll.id, "recorded": true }),
            ),
            Some(Audience::Participants {
                ids: vec![me],
                moderator_permission: None,
            }),
        )
        .await?;
        if !poll.is_anonymous {
            let count = self.ballot_count(&txn, poll.id).await?;
            let manager_results = self.results_for(&txn, &poll, true, true, None).await?;
            emit_to(
                &txn,
                Self::record(&poll, conference.meeting_space_id, event::POLL_RESULTS_UPDATED,
                    json!({ "pollId": poll.id, "responseCount": count, "results": manager_results })),
                Some(Self::managers()),
            )
            .await?;
        } else {
            // Anonymous: only the ballot count moves live (no per-vote aggregates).
            let count = self.ballot_count(&txn, poll.id).await?;
            emit_to(
                &txn,
                Self::record(
                    &poll,
                    conference.meeting_space_id,
                    event::POLL_RESULTS_UPDATED,
                    json!({ "pollId": poll.id, "responseCount": count }),
                ),
                Some(Self::managers()),
            )
            .await?;
        }
        txn.commit().await?;
        inc(&CM.poll_responses);
        let show_score =
            poll.quiz_mode && !poll.is_anonymous && visibility == ResultsVisibility::AfterVote;
        Ok(PollResponseReceipt {
            poll_id,
            recorded: true,
            idempotent_replay: false,
            score: show_score.then_some(s),
            max_score: show_score.then_some(max),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: PollQuestionType, options: Option<Vec<&str>>) -> PollQuestionInput {
        PollQuestionInput {
            question_type: t,
            prompt: "Q?".into(),
            options: options.map(|o| o.into_iter().map(String::from).collect()),
            required: true,
            min_selections: None,
            max_selections: None,
            max_length: None,
            rating_min: None,
            rating_max: None,
            correct: None,
            points: None,
        }
    }

    fn ans(id: &str) -> PollAnswerInput {
        PollAnswerInput {
            question_id: id.into(),
            option_ids: None,
            text: None,
            rating: None,
            ranking: None,
        }
    }

    #[test]
    fn lifecycle_is_strict() {
        use PollStatus::*;
        assert!(poll_transition_allowed(Draft, Open));
        assert!(poll_transition_allowed(Open, Closed));
        assert!(poll_transition_allowed(Closed, Archived));
        assert!(poll_transition_allowed(Draft, Archived));
        assert!(!poll_transition_allowed(Closed, Open), "never reopened");
        assert!(!poll_transition_allowed(Archived, Open));
        assert!(!poll_transition_allowed(Draft, Closed));
        assert!(!poll_transition_allowed(Open, Archived));
    }

    #[test]
    fn definition_validation() {
        assert!(build_definition(&[], false).is_err());
        assert!(
            build_definition(&[q(PollQuestionType::SingleChoice, Some(vec!["a"]))], false).is_err(),
            "≥2 options"
        );
        assert!(
            build_definition(
                &[q(PollQuestionType::SingleChoice, Some(vec!["a", "A"]))],
                false
            )
            .is_err(),
            "unique"
        );
        assert!(build_definition(
            &[q(PollQuestionType::ShortText, Some(vec!["a", "b"]))],
            false
        )
        .is_err());
        let mut quiz = q(PollQuestionType::SingleChoice, Some(vec!["a", "b"]));
        quiz.correct = Some(vec![1]);
        assert!(
            build_definition(&[quiz.clone()], false).is_err(),
            "answers only in quiz mode"
        );
        let def = build_definition(&[quiz], true).unwrap();
        assert_eq!(def.questions[0].correct, Some(vec!["o2".to_string()]));
        assert!(
            def.public().questions[0].correct.is_none(),
            "public projection strips answers"
        );
        assert!(
            build_definition(
                &[q(PollQuestionType::SingleChoice, Some(vec!["a", "b"]))],
                true
            )
            .is_err(),
            "quiz needs answers"
        );
    }

    #[test]
    fn response_validation_per_type() {
        let mut multi = q(PollQuestionType::MultipleChoice, Some(vec!["a", "b", "c"]));
        multi.max_selections = Some(2);
        let mut text = q(PollQuestionType::ShortText, None);
        text.max_length = Some(5);
        text.required = false;
        let def = build_definition(
            &[
                q(PollQuestionType::SingleChoice, Some(vec!["a", "b"])),
                multi,
                text,
                q(PollQuestionType::Rating, None),
                q(PollQuestionType::Ranking, Some(vec!["x", "y", "z"])),
            ],
            false,
        )
        .unwrap();
        let ok = vec![
            PollAnswerInput {
                option_ids: Some(vec!["o1".into()]),
                ..ans("q1")
            },
            PollAnswerInput {
                option_ids: Some(vec!["o1".into(), "o3".into()]),
                ..ans("q2")
            },
            PollAnswerInput {
                rating: Some(4),
                ..ans("q4")
            },
            PollAnswerInput {
                ranking: Some(vec!["o3".into(), "o1".into(), "o2".into()]),
                ..ans("q5")
            },
        ];
        assert!(
            validate_response(&def, &ok).is_ok(),
            "optional text may be omitted"
        );
        let mut bad = ok.clone();
        bad[0].option_ids = Some(vec!["o1".into(), "o2".into()]);
        assert!(
            validate_response(&def, &bad).is_err(),
            "single = exactly one"
        );
        let mut bad = ok.clone();
        bad[1].option_ids = Some(vec!["o1".into(), "o2".into(), "o3".into()]);
        assert!(validate_response(&def, &bad).is_err(), "bounded multiple");
        let mut bad = ok.clone();
        bad[2].rating = Some(9);
        assert!(validate_response(&def, &bad).is_err(), "rating range");
        let mut bad = ok.clone();
        bad[3].ranking = Some(vec!["o1".into(), "o1".into(), "o2".into()]);
        assert!(validate_response(&def, &bad).is_err(), "ranking unique");
        let mut bad = ok.clone();
        bad.push(PollAnswerInput {
            text: Some("toolong".into()),
            ..ans("q3")
        });
        assert!(validate_response(&def, &bad).is_err(), "text length");
        let mut bad = ok.clone();
        bad.remove(0);
        assert!(validate_response(&def, &bad).is_err(), "required question");
        let mut bad = ok;
        bad.push(PollAnswerInput {
            option_ids: Some(vec!["o1".into()]),
            ..ans("q9")
        });
        assert!(validate_response(&def, &bad).is_err(), "unknown question");
    }

    #[test]
    fn quiz_scoring_is_server_side() {
        let mut a = q(PollQuestionType::SingleChoice, Some(vec!["a", "b"]));
        a.correct = Some(vec![0]);
        a.points = Some(2.0);
        let mut b = q(PollQuestionType::MultipleChoice, Some(vec!["a", "b", "c"]));
        b.correct = Some(vec![0, 2]);
        let def = build_definition(&[a, b, q(PollQuestionType::ShortText, None)], true).unwrap();
        let answers = validate_response(
            &def,
            &[
                PollAnswerInput {
                    option_ids: Some(vec!["o1".into()]),
                    ..ans("q1")
                },
                PollAnswerInput {
                    option_ids: Some(vec!["o3".into(), "o1".into()]),
                    ..ans("q2")
                },
                PollAnswerInput {
                    text: Some("hi".into()),
                    ..ans("q3")
                },
            ],
        )
        .unwrap();
        assert_eq!(score(&def, &answers), (3.0, 3.0));
        let wrong = validate_response(
            &def,
            &[
                PollAnswerInput {
                    option_ids: Some(vec!["o2".into()]),
                    ..ans("q1")
                },
                PollAnswerInput {
                    option_ids: Some(vec!["o1".into()]),
                    ..ans("q2")
                },
                PollAnswerInput {
                    text: Some("hi".into()),
                    ..ans("q3")
                },
            ],
        )
        .unwrap();
        assert_eq!(score(&def, &wrong), (0.0, 3.0));
    }

    #[test]
    fn anonymous_projection_has_no_identity() {
        let mut t = q(PollQuestionType::ShortText, None);
        t.required = true;
        let def = build_definition(&[t], false).unwrap();
        let rows: Vec<ResponseRow> = (0..3)
            .map(|i| ResponseRow {
                participant_id: Some(Uuid::new_v4()),
                answers: [(
                    "q1".to_string(),
                    PollAnswer {
                        option_ids: None,
                        text: Some(format!("t{i}")),
                        rating: None,
                        ranking: None,
                    },
                )]
                .into(),
                score: None,
            })
            .collect();
        let r = aggregate(
            Uuid::new_v4(),
            PollStatus::Closed,
            true,
            &def,
            &rows,
            ResultsViewer {
                manager: true,
                reveal: true,
                me: None,
            },
        );
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("participantId"), "{json}");
        assert_eq!(r.questions[0].texts.as_ref().unwrap().len(), 3);
        let named = aggregate(
            Uuid::new_v4(),
            PollStatus::Closed,
            false,
            &def,
            &rows,
            ResultsViewer {
                manager: true,
                reveal: true,
                me: None,
            },
        );
        assert!(serde_json::to_string(&named)
            .unwrap()
            .contains("participantId"));
    }

    #[test]
    fn results_visibility_modes() {
        use PollStatus::*;
        use ResultsVisibility::*;
        // HOST_ONLY
        assert!(!results_visible(Open, HostOnly, false, false, true));
        assert!(results_visible(Open, HostOnly, false, true, false));
        assert!(!results_visible(Closed, HostOnly, false, false, true));
        // AFTER_VOTE
        assert!(!results_visible(Open, AfterVote, false, false, false));
        assert!(results_visible(Open, AfterVote, false, false, true));
        // AFTER_CLOSE
        assert!(!results_visible(Open, AfterClose, false, false, true));
        assert!(results_visible(Closed, AfterClose, false, false, false));
        // Anonymous: nobody sees live aggregates, hosts included.
        assert!(!results_visible(Open, AfterVote, true, true, true));
        assert!(results_visible(Closed, AfterVote, true, true, false));
        // Answers revealed only in the allowed state.
        assert!(!reveal_answers(Open, AfterVote, false));
        assert!(reveal_answers(Closed, AfterClose, false));
        assert!(!reveal_answers(Closed, HostOnly, false));
        assert!(reveal_answers(Open, HostOnly, true));
    }
}

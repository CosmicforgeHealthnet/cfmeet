//! Canonical call history (Part F §62, §130–§137; ADR 0025).
//!
//! A principal's history is the list of Calls it participates in, each with the outcome
//! derived FOR THAT LOGICAL PARTICIPANT from Call evidence only: the participant row (one
//! per principal per Call, whatever the number of devices), its ring offers on every
//! device, its sessions' connection intervals and the Call's own state. Nothing stores a
//! "missed" flag, and push delivery, realtime signals or client acknowledgements are not
//! inputs: a lost or failed notification cannot create, erase or change an entry, and a
//! client that reconnects reads the same answer.
//!
//! Derivation:
//! * OUTBOUND (the initiator): ANSWERED once the Call was answered, NOT_ANSWERED when it
//!   ended unanswered, IN_PROGRESS otherwise.
//! * INBOUND (a recipient), first match wins:
//!   1. ANSWERED — one of its offers was accepted (on any of its devices) or one of its
//!      sessions connected;
//!   2. DECLINED — it declined an offer;
//!   3. ANSWERED_ELSEWHERE — its offers were won by ANOTHER participant of the same answer
//!      group (a ring-group or queue colleague). A losing DEVICE of a participant that
//!      answered is case 1: the participant answered, nothing is missed twice;
//!   4. RINGING — an offer is still live;
//!   5. MISSED — it was offered on at least one device and every offer ended unanswered
//!      (expired, the caller cancelled, the Call ended, the ring moved on);
//!   6. NOT_REACHED — never offered and the Call is over; IN_PROGRESS otherwise.
//!
//! Every query is confined to the caller's scope and to one principal, walks the
//! `idx_call_participants_history` keyset (never the whole table), and returns safe
//! fields only: no provider, edge or contact data, no object key or URL.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, QueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{enterprise::stmt, AppCallService};
use crate::{
    calls::{
        aggregate::Call,
        command::CallScope,
        errors::{CallError, CallResult},
        ids::CallId,
        read_model::{ts, ts_opt, CallCursor, CallSummaryView},
        service::{CallPage, ListCallsQuery},
    },
    domain::PrincipalId,
    enterprise::{
        destinations::{Destination, DestinationType},
        voicemail::{mailbox_owners, mailbox_row},
        EnterpriseError,
    },
    models::calls,
};

text_enum!(
    /// Direction of a Call relative to the principal whose history it is.
    HistoryDirection {
        Inbound = "INBOUND",
        Outbound = "OUTBOUND",
    }
);

text_enum!(
    /// The outcome of a Call for one logical participant (derived, never stored).
    ParticipantOutcome {
        Answered = "ANSWERED",
        NotAnswered = "NOT_ANSWERED",
        Missed = "MISSED",
        Declined = "DECLINED",
        AnsweredElsewhere = "ANSWERED_ELSEWHERE",
        Ringing = "RINGING",
        NotReached = "NOT_REACHED",
        InProgress = "IN_PROGRESS",
    }
);

/// Enterprise destinations a history may be filtered by (never a public contact).
pub const HISTORY_DESTINATIONS: &[DestinationType] = &[
    DestinationType::Extension,
    DestinationType::RingGroup,
    DestinationType::Queue,
    DestinationType::Ivr,
    DestinationType::Voicemail,
];

/// Participant-relative filters (they need a principal).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    pub direction: Option<HistoryDirection>,
    pub outcome: Option<ParticipantOutcome>,
    pub missed: Option<bool>,
}

impl HistoryFilter {
    pub fn is_empty(&self) -> bool {
        self.direction.is_none() && self.outcome.is_none() && self.missed.is_none()
    }
}

/// The principal's own part in one Call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participation {
    pub participant_id: Uuid,
    pub direction: HistoryDirection,
    pub outcome: ParticipantOutcome,
    pub added_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counterpart {
    pub principal_id: Uuid,
    pub direction: HistoryDirection,
    pub display_name: Option<String>,
}

/// Safe enterprise labels of a Call (internal extension number or alias, labels).
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct EnterpriseLabels {
    pub call_id: Uuid,
    pub entry_extension_number: String,
    pub entry_label: String,
    pub phase: String,
    pub outcome: Option<String>,
    pub ring_group_label: Option<String>,
    pub queue_label: Option<String>,
}

/// A voicemail message left on the Call, shown only to an owner of its mailbox.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct VoicemailLink {
    pub call_id: Uuid,
    pub id: Uuid,
    pub mailbox_id: Uuid,
    pub state: String,
    pub read_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub call: Call,
    pub participation: Participation,
    pub counterparts: Vec<Counterpart>,
    pub enterprise: Option<EnterpriseLabels>,
    pub voicemail: Option<VoicemailLink>,
    /// A READY recording exists (participants may read its metadata; access to the
    /// audio goes through the audited recording routes).
    pub recording_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub principal_id: PrincipalId,
    pub entries: Vec<HistoryEntry>,
    pub next_cursor: Option<CallCursor>,
}

/// At most this many other participants are listed per entry.
const MAX_COUNTERPARTS: usize = 10;

const TERMINAL: &str = "('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')";

fn database(err: EnterpriseError) -> CallError {
    match err {
        EnterpriseError::Database {
            summary,
            connectivity,
        } => CallError::Database {
            summary,
            connectivity,
        },
        _ => CallError::Database {
            summary: "history enrichment".into(),
            connectivity: false,
        },
    }
}

/// Appends the bounded filters shared by both listings (`c` = calls).
fn push_call_filters(
    sql: &mut String,
    values: &mut Vec<sea_orm::Value>,
    query: &ListCallsQuery,
    destination: Option<Destination>,
) {
    if let Some(state) = query.state {
        values.push(state.as_str().into());
        sql.push_str(&format!(" AND c.current_state = ${}", values.len()));
    }
    if let Some(initiator) = query.initiator_principal_id {
        values.push(initiator.as_uuid().into());
        sql.push_str(&format!(
            " AND c.initiator_principal_id = ${}",
            values.len()
        ));
    }
    if let Some(destination) = destination {
        values.push(destination.kind.as_str().into());
        values.push(destination.id.into());
        let (t, d) = (values.len() - 1, values.len());
        sql.push_str(&format!(
            " AND (EXISTS (SELECT 1 FROM enterprise_routing_steps r WHERE r.call_id = c.id \
             AND r.tenant_id = $1 AND r.application_id = $2 AND r.environment_id = $3 \
             AND r.destination_type = ${t} AND r.destination_id = ${d})"
        ));
        if destination.kind == DestinationType::Extension {
            sql.push_str(&format!(
                " OR EXISTS (SELECT 1 FROM enterprise_call_contexts x WHERE x.call_id = c.id \
                 AND x.entry_extension_id = ${d})"
            ));
        }
        sql.push(')');
    }
}

/// Appends the keyset position and time bounds on `column` (a copy of the Call's
/// `created_at`) and `id_column`.
fn push_keyset(
    sql: &mut String,
    values: &mut Vec<sea_orm::Value>,
    query: &ListCallsQuery,
    column: &str,
    id_column: &str,
) {
    if let Some(after) = query.created_after {
        values.push(after.into());
        sql.push_str(&format!(" AND {column} >= ${}", values.len()));
    }
    if let Some(before) = query.created_before {
        values.push(before.into());
        sql.push_str(&format!(" AND {column} < ${}", values.len()));
    }
    if let Some(cursor) = &query.cursor {
        values.push(cursor.created_at().into());
        values.push(cursor.call_id().as_uuid().into());
        sql.push_str(&format!(
            " AND ({column}, {id_column}) < (${}, ${})",
            values.len() - 1,
            values.len()
        ));
    }
}

fn scope_values(scope: CallScope) -> Vec<sea_orm::Value> {
    vec![
        scope.tenant_id().as_uuid().into(),
        scope.application_id().as_uuid().into(),
        scope.environment_id().as_uuid().into(),
    ]
}

fn page_limit(query: &ListCallsQuery) -> CallResult<u64> {
    match query.limit {
        0 => Ok(ListCallsQuery::DEFAULT_LIMIT),
        n if n > ListCallsQuery::MAX_LIMIT => {
            Err(CallError::Validation("limit must be between 1 and 100"))
        }
        n => Ok(n),
    }
}

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> CallResult<T> {
    row.try_get("", column).map_err(|_| CallError::Database {
        summary: format!("history column {column}"),
        connectivity: false,
    })
}

/// The history query of one page (`limit` + 1 rows): the participant's keyset range of
/// `idx_call_participants_history`, the Calls, and the outcome derived per row. Public so
/// the index use can be certified with `EXPLAIN`.
pub fn history_statement(
    scope: CallScope,
    principal: PrincipalId,
    query: &ListCallsQuery,
    filter: HistoryFilter,
    destination: Option<Destination>,
    limit: u64,
) -> sea_orm::Statement {
    let mut values = scope_values(scope);
    values.push(principal.as_uuid().into());
    let mut sql = format!(
        "SELECT c.*, p.id AS h_participant_id, p.role AS h_role, p.created_at AS h_added_at, \
         p.ended_at AS h_left_at, a.answered_at AS h_answered_at, h.outcome AS h_outcome \
         FROM call_participants p \
         JOIN calls c ON c.id = p.call_id AND c.tenant_id = p.tenant_id \
           AND c.application_id = p.application_id AND c.environment_id = p.environment_id \
         CROSS JOIN LATERAL (SELECT count(*) AS offers, \
             count(*) FILTER (WHERE i.status IN ('PENDING', 'RINGING')) AS live, \
             count(*) FILTER (WHERE i.status = 'DECLINED') AS declined, \
             count(*) FILTER (WHERE i.status = 'ANSWERED_ELSEWHERE') AS elsewhere, \
             min(i.accepted_at) FILTER (WHERE i.status = 'ACCEPTED') AS accepted_at \
           FROM call_invitations i WHERE i.participant_id = p.id) o \
         CROSS JOIN LATERAL (SELECT min(v.connected_at) AS connected_at \
           FROM call_sessions cs JOIN call_session_intervals v ON v.session_id = cs.id \
           WHERE cs.participant_id = p.id) sx \
         CROSS JOIN LATERAL (SELECT CASE WHEN p.role = 'INITIATOR' THEN c.answered_at \
           ELSE COALESCE(o.accepted_at, sx.connected_at) END AS answered_at) a \
         CROSS JOIN LATERAL (SELECT CASE \
           WHEN p.role = 'INITIATOR' THEN CASE \
             WHEN c.answered_at IS NOT NULL THEN 'ANSWERED' \
             WHEN c.current_state IN {TERMINAL} THEN 'NOT_ANSWERED' \
             ELSE 'IN_PROGRESS' END \
           WHEN a.answered_at IS NOT NULL THEN 'ANSWERED' \
           WHEN o.declined > 0 THEN 'DECLINED' \
           WHEN o.elsewhere > 0 THEN 'ANSWERED_ELSEWHERE' \
           WHEN o.live > 0 THEN 'RINGING' \
           WHEN o.offers > 0 THEN 'MISSED' \
           WHEN c.current_state IN {TERMINAL} THEN 'NOT_REACHED' \
           ELSE 'IN_PROGRESS' END AS outcome) h \
         WHERE p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
           AND p.principal_id = $4"
    );
    push_keyset(
        &mut sql,
        &mut values,
        query,
        "p.call_created_at",
        "p.call_id",
    );
    push_call_filters(&mut sql, &mut values, query, destination);
    if let Some(direction) = filter.direction {
        sql.push_str(match direction {
            HistoryDirection::Outbound => " AND p.role = 'INITIATOR'",
            HistoryDirection::Inbound => " AND p.role = 'RECIPIENT'",
        });
    }
    if let Some(outcome) = filter.outcome {
        values.push(outcome.as_str().into());
        sql.push_str(&format!(" AND h.outcome = ${}", values.len()));
    }
    match filter.missed {
        Some(true) => sql.push_str(" AND h.outcome = 'MISSED'"),
        Some(false) => sql.push_str(" AND h.outcome <> 'MISSED'"),
        None => {}
    }
    sql.push_str(&format!(
        " ORDER BY p.call_created_at DESC, p.call_id DESC LIMIT {}",
        limit + 1
    ));
    stmt(&sql, values)
}

impl AppCallService {
    /// One page of `principal`'s history in `scope`, newest Call first (keyset on the
    /// Call's `(created_at, id)`, exactly like `GET /v1/calls`).
    pub async fn call_history(
        &self,
        scope: CallScope,
        principal: PrincipalId,
        query: &ListCallsQuery,
        filter: HistoryFilter,
        destination: Option<Destination>,
    ) -> CallResult<HistoryPage> {
        let limit = page_limit(query)?;
        let statement = history_statement(scope, principal, query, filter, destination, limit);
        let mut rows = self.db().query_all(statement).await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            match rows.last() {
                Some(last) => Some(CallCursor::new(
                    get(last, "created_at")?,
                    CallId::from_uuid(get(last, "id")?),
                )),
                None => None,
            }
        } else {
            None
        };
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            let call: Call = calls::Model::from_query_result(row, "")
                .map_err(|_| CallError::Database {
                    summary: "history call row".into(),
                    connectivity: false,
                })?
                .into();
            let role: String = get(row, "h_role")?;
            let outcome: String = get(row, "h_outcome")?;
            entries.push(HistoryEntry {
                call,
                participation: Participation {
                    participant_id: get(row, "h_participant_id")?,
                    direction: if role == "INITIATOR" {
                        HistoryDirection::Outbound
                    } else {
                        HistoryDirection::Inbound
                    },
                    outcome: ParticipantOutcome::parse(&outcome).ok_or(CallError::Database {
                        summary: "history outcome".into(),
                        connectivity: false,
                    })?,
                    added_at: get(row, "h_added_at")?,
                    answered_at: get(row, "h_answered_at")?,
                    left_at: get(row, "h_left_at")?,
                },
                counterparts: Vec::new(),
                enterprise: None,
                voicemail: None,
                recording_available: false,
            });
        }
        self.enrich(scope, principal, &mut entries).await?;
        Ok(HistoryPage {
            principal_id: principal,
            entries,
            next_cursor,
        })
    }

    /// One page of the Calls of `scope` routed through an enterprise `destination`
    /// (the application-wide listing; same order and cursor as `GET /v1/calls`).
    pub async fn calls_through(
        &self,
        scope: CallScope,
        query: &ListCallsQuery,
        destination: Destination,
    ) -> CallResult<CallPage> {
        let limit = page_limit(query)?;
        let mut values = scope_values(scope);
        let mut sql = "SELECT c.* FROM calls c WHERE c.tenant_id = $1 \
                       AND c.application_id = $2 AND c.environment_id = $3"
            .to_string();
        push_keyset(&mut sql, &mut values, query, "c.created_at", "c.id");
        push_call_filters(&mut sql, &mut values, query, Some(destination));
        sql.push_str(&format!(
            " ORDER BY c.created_at DESC, c.id DESC LIMIT {}",
            limit + 1
        ));
        let mut rows = calls::Model::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|last| CallCursor::new(last.created_at, CallId::from_uuid(last.id)))
        } else {
            None
        };
        Ok(CallPage {
            calls: rows.into_iter().map(Into::into).collect(),
            next_cursor,
        })
    }

    /// Adds counterparts, enterprise labels, the voicemail link (mailbox owners only) and
    /// recording availability to one page (bounded: one query each, ≤ 100 Calls).
    async fn enrich(
        &self,
        scope: CallScope,
        principal: PrincipalId,
        entries: &mut [HistoryEntry],
    ) -> CallResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let ids: Vec<Uuid> = entries.iter().map(|e| e.call.id.as_uuid()).collect();
        let with_ids = |extra: Vec<sea_orm::Value>| {
            let mut values = scope_values(scope);
            values.push(ids.clone().into());
            values.extend(extra);
            values
        };

        let mut others: HashMap<Uuid, Vec<Counterpart>> = HashMap::new();
        for row in self
            .db()
            .query_all(stmt(
                "SELECT call_id, principal_id, role, display_name FROM call_participants \
                 WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                 AND call_id = ANY($4) AND principal_id <> $5 ORDER BY call_id, created_at, id",
                with_ids(vec![principal.as_uuid().into()]),
            ))
            .await?
        {
            let list = others.entry(get(&row, "call_id")?).or_default();
            if list.len() < MAX_COUNTERPARTS {
                let role: String = get(&row, "role")?;
                list.push(Counterpart {
                    principal_id: get(&row, "principal_id")?,
                    direction: if role == "INITIATOR" {
                        HistoryDirection::Outbound
                    } else {
                        HistoryDirection::Inbound
                    },
                    display_name: get(&row, "display_name")?,
                });
            }
        }

        let labels: HashMap<Uuid, EnterpriseLabels> = EnterpriseLabels::find_by_statement(stmt(
            "SELECT x.call_id, x.entry_extension_number, x.entry_label, x.phase, x.outcome, \
             g.name AS ring_group_label, q.name AS queue_label \
             FROM enterprise_call_contexts x \
             LEFT JOIN enterprise_ring_groups g ON g.id = x.ring_group_id \
             LEFT JOIN enterprise_queue_entries e ON e.id = x.queue_entry_id \
             LEFT JOIN enterprise_call_queues q ON q.id = e.queue_id \
             WHERE x.tenant_id = $1 AND x.application_id = $2 AND x.environment_id = $3 \
             AND x.call_id = ANY($4)",
            with_ids(Vec::new()),
        ))
        .all(self.db())
        .await?
        .into_iter()
        .map(|l| (l.call_id, l))
        .collect();

        let messages = VoicemailLink::find_by_statement(stmt(
            "SELECT call_id, id, mailbox_id, state, read_state FROM voicemail_messages \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
             AND call_id = ANY($4) AND state <> 'DELETED'",
            with_ids(Vec::new()),
        ))
        .all(self.db())
        .await?;
        let mut owned: BTreeMap<Uuid, bool> = BTreeMap::new();
        for message in &messages {
            if owned.contains_key(&message.mailbox_id) {
                continue;
            }
            let mailbox = mailbox_row(self.db(), scope, message.mailbox_id, false)
                .await
                .map_err(database)?;
            let owners = mailbox_owners(self.db(), scope, &mailbox)
                .await
                .map_err(database)?;
            owned.insert(message.mailbox_id, owners.contains(&principal.as_uuid()));
        }
        let mut voicemail: HashMap<Uuid, VoicemailLink> = messages
            .into_iter()
            .filter(|m| owned.get(&m.mailbox_id).copied().unwrap_or(false))
            .map(|m| (m.call_id, m))
            .collect();

        let recorded: HashSet<Uuid> = self
            .db()
            .query_all(stmt(
                "SELECT DISTINCT call_id FROM call_recordings WHERE tenant_id = $1 \
                 AND application_id = $2 AND environment_id = $3 AND call_id = ANY($4) \
                 AND state = 'READY'",
                with_ids(Vec::new()),
            ))
            .await?
            .iter()
            .map(|row| get(row, "call_id"))
            .collect::<CallResult<_>>()?;

        let mut labels = labels;
        for entry in entries.iter_mut() {
            let id = entry.call.id.as_uuid();
            entry.counterparts = others.remove(&id).unwrap_or_default();
            entry.enterprise = labels.remove(&id);
            entry.voicemail = voicemail.remove(&id);
            entry.recording_available = recorded.contains(&id);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- views

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationView {
    pub participant_id: Uuid,
    pub direction: &'static str,
    pub outcome: &'static str,
    pub missed: bool,
    pub added_at: String,
    pub answered_at: Option<String>,
    pub left_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterpartView {
    pub principal_id: Uuid,
    pub direction: &'static str,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnterpriseHistoryView {
    pub entry_extension: String,
    pub entry_label: String,
    pub phase: String,
    pub outcome: Option<String>,
    pub ring_group_label: Option<String>,
    pub queue_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicemailLinkView {
    pub message_id: Uuid,
    pub state: String,
    pub read_state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHistoryEntryView {
    #[serde(flatten)]
    pub call: CallSummaryView,
    /// From CONNECTED to the end of the Call; `null` while it lasts or if it never
    /// connected.
    pub duration_ms: Option<i64>,
    pub participation: ParticipationView,
    pub counterparts: Vec<CounterpartView>,
    pub enterprise: Option<EnterpriseHistoryView>,
    pub voicemail: Option<VoicemailLinkView>,
    pub recording_available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHistoryListView {
    pub principal_id: Uuid,
    pub data: Vec<CallHistoryEntryView>,
    pub next_cursor: Option<String>,
}

impl From<&HistoryEntry> for CallHistoryEntryView {
    fn from(e: &HistoryEntry) -> Self {
        let t = &e.call.timestamps;
        let p = &e.participation;
        Self {
            call: CallSummaryView::from(&e.call),
            duration_ms: match (t.connected_at, t.ended_at) {
                (Some(connected), Some(ended)) => {
                    Some((ended - connected).num_milliseconds().max(0))
                }
                _ => None,
            },
            participation: ParticipationView {
                participant_id: p.participant_id,
                direction: p.direction.as_str(),
                outcome: p.outcome.as_str(),
                missed: p.outcome == ParticipantOutcome::Missed,
                added_at: ts(p.added_at),
                answered_at: ts_opt(p.answered_at),
                left_at: ts_opt(p.left_at),
            },
            counterparts: e
                .counterparts
                .iter()
                .map(|c| CounterpartView {
                    principal_id: c.principal_id,
                    direction: c.direction.as_str(),
                    display_name: c.display_name.clone(),
                })
                .collect(),
            enterprise: e.enterprise.as_ref().map(|l| EnterpriseHistoryView {
                entry_extension: l.entry_extension_number.clone(),
                entry_label: l.entry_label.clone(),
                phase: l.phase.clone(),
                outcome: l.outcome.clone(),
                ring_group_label: l.ring_group_label.clone(),
                queue_label: l.queue_label.clone(),
            }),
            voicemail: e.voicemail.as_ref().map(|v| VoicemailLinkView {
                message_id: v.id,
                state: v.state.clone(),
                read_state: v.read_state.clone(),
            }),
            recording_available: e.recording_available,
        }
    }
}

impl From<&HistoryPage> for CallHistoryListView {
    fn from(p: &HistoryPage) -> Self {
        Self {
            principal_id: p.principal_id.as_uuid(),
            data: p.entries.iter().map(Into::into).collect(),
            next_cursor: p.next_cursor.map(|c| c.encode()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::state::CallState;

    #[test]
    fn the_sql_terminal_set_is_the_domain_terminal_set() {
        let listed: Vec<String> = TERMINAL
            .trim_matches(|c| c == '(' || c == ')')
            .split(", ")
            .map(|s| s.trim_matches('\'').to_string())
            .collect();
        let domain: Vec<String> = CallState::TERMINAL
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        assert_eq!(listed, domain);
    }

    #[test]
    fn participant_filters_are_empty_by_default() {
        assert!(HistoryFilter::default().is_empty());
        assert!(!HistoryFilter {
            missed: Some(true),
            ..Default::default()
        }
        .is_empty());
    }
}

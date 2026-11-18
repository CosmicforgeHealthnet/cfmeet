//! W1 — the ingest processor (Document 4 §11): claims PENDING events (CRITICAL first, then
//! HIGH, then STANDARD), resolves recipients through the owners, evaluates preferences and
//! precedence, renders, and creates deliveries according to the service-side routing mode.
//!
//! Per recipient everything happens in one transaction keyed by
//! `UNIQUE (recipient_ref, dedupe_key)`: the notification, its deliveries, the inbox item and
//! the email ledger row. A retry after a crash or an owner outage therefore never creates a
//! second copy (P8: at least once internally, deduplicated at every boundary).
//!
//! Modes: `ACTIVE` channels deliver; `SHADOW` channels (and every producer shadow copy)
//! record `SHADOW` deliveries plus shadow observations with the render digest, and never
//! call a provider; `OFF` channels create nothing.
//!
//! Meeting reminders (Document 4 §9.9): before the takeover boundary `B` the Meeting scan
//! is the only reminder trigger and posts `conference.scheduled` with the reminder key as
//! `dedupeKey` (`reminder:<conference>:<start epoch>`), which renders the reminder here.
//! Any other `conference.scheduled` arms a reminder schedule only for starts ≥ `B`;
//! `conference.rescheduled` / `cancelled` cancel it (and re-arm on reschedule).

use std::{collections::BTreeMap, collections::HashMap, sync::Arc, time::Duration as StdDuration};

use cf_notification_client::{seal::OpeningKey, Envelope, Lane, CONTROL_EVENT};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    authorities::{AccountContact, Authorities, AuthorityError},
    catalogue::{self, Channel, PrefCategory, TypeRoute},
    config::EmailConfig,
    crypto::DataKeys,
    db,
    email::{self, NewEmail},
    inbox,
    metrics::{label, Heartbeats, Metrics},
    policy::{self, ChannelDecision, PolicyInput},
    preferences, push,
    routing::{self, Mode},
    templates::{self, MeetingFacts, RecipientFacts, Secrets},
};

pub const REMINDER_LEAD_MINUTES: i64 = 15;
const EVENT_LEASE_SECS: i64 = 60;
const MAX_EVENT_ATTEMPTS: i32 = 30;

/// One unit of work: an event (or a fired reminder schedule).
#[derive(Debug, Clone)]
pub struct Job {
    pub producer: String,
    pub event_id: String,
    pub event_type: String,
    pub route: &'static TypeRoute,
    pub template: &'static str,
    pub lane: Lane,
    pub data: BTreeMap<String, Value>,
    pub recipients: Vec<String>,
    pub dedupe_key: String,
    pub idempotency_key: Option<String>,
    pub coalesce_key: Option<String>,
    pub cancel_key: Option<String>,
    pub deliver_after: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub decision_id: Uuid,
    pub subject: Option<(String, Uuid)>,
    pub sealed: Option<cf_notification_client::Sealed>,
    pub shadow: bool,
    /// The envelope's (tenant, application, environment), when it names one.
    pub scope: Option<(Uuid, Uuid, Uuid)>,
}

fn parse_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

impl Job {
    pub fn from_envelope(env: &Envelope, shadow: bool) -> Option<Self> {
        let route = catalogue::route(&env.event_type)?;
        Some(Self {
            producer: env.producer.clone(),
            event_id: env.event_id.clone(),
            event_type: env.event_type.clone(),
            route,
            template: route.template,
            lane: route.lane(),
            data: env.data.clone(),
            recipients: env.recipients.iter().map(|r| r.reference.clone()).collect(),
            dedupe_key: env.delivery.dedupe_key.clone(),
            idempotency_key: env.delivery.idempotency_key.clone(),
            coalesce_key: env.delivery.coalesce_key.clone(),
            cancel_key: env.delivery.cancel_key.clone(),
            deliver_after: parse_time(env.delivery.deliver_after.as_deref()),
            expires_at: parse_time(env.delivery.expires_at.as_deref()),
            decision_id: env.route.decision_id,
            subject: env.subject.as_ref().map(|s| (s.kind.clone(), s.id)),
            sealed: env.sealed.clone(),
            shadow,
            scope: env
                .scope
                .as_ref()
                .map(|s| (s.tenant_id, s.application_id, s.environment_id)),
        })
    }

    fn data_uuid(&self, key: &str) -> Option<Uuid> {
        self.data
            .get(key)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    }

    fn data_str(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Value::as_str)
    }
}

/// Outcome of processing one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Processed {
    Done,
    /// An owner was unavailable; retry later (nothing partial is ever visible twice).
    Retry(&'static str),
}

pub struct Processor {
    pub db: DatabaseConnection,
    pub keys: Arc<DataKeys>,
    pub opening: Option<Arc<OpeningKey>>,
    pub authorities: Arc<dyn Authorities>,
    pub email: Arc<EmailConfig>,
    pub metrics: Arc<Metrics>,
    pub operator_emails: Vec<String>,
    /// ADR 0063: `platform-principal:<env>:<principal>` recipients are delivered (only
    /// while `NOTIFICATIONS_PARTNER_PUSH_ENABLED` is on).
    pub partner_push: bool,
}

#[derive(Debug, FromQueryResult)]
struct ClaimedEvent {
    producer: String,
    event_id: String,
    envelope: Value,
    shadow: bool,
    attempts: i32,
    lease_id: Option<Uuid>,
}

/// Whether an owner error means "retry later" (vs. "record as unresolved").
fn must_retry(err: &AuthorityError, mode_active: bool) -> bool {
    match err {
        AuthorityError::NotConfigured => mode_active,
        AuthorityError::Unavailable | AuthorityError::InvalidResponse => mode_active,
    }
}

impl Processor {
    fn open_secrets(&self, job: &Job) -> (Secrets, Option<String>) {
        let (Some(sealed), Some(key)) = (&job.sealed, &self.opening) else {
            return (Secrets::default(), None);
        };
        match cf_notification_client::seal::open(
            key,
            &job.producer,
            &job.event_id,
            &job.event_type,
            sealed,
        ) {
            Ok(p) => (
                Secrets {
                    code: p.code.clone(),
                    token: p.token.clone(),
                    address: p.address.clone(),
                    masked_address: p.masked_address.clone(),
                    request_client: p.request_client.clone(),
                    request_network: p.request_network.clone(),
                },
                p.address.clone(),
            ),
            Err(_) => {
                self.metrics.inc("notif_sealed_open_failures_total", "");
                (Secrets::default(), None)
            }
        }
    }

    async fn claim(&self, lane: Option<&'static str>) -> Result<Vec<ClaimedEvent>, DbErr> {
        let lease = Uuid::new_v4();
        db::all(
            &self.db,
            "UPDATE notif_event_inbox e SET lease_id = $1, lease_until = now() + make_interval(secs => $2), attempts = e.attempts + 1
              WHERE (e.producer, e.event_id) IN (
                    SELECT producer, event_id FROM notif_event_inbox
                     WHERE state = 'PENDING' AND (lease_until IS NULL OR lease_until < now())
                       AND ($3::text IS NULL OR lane = $3)
                     ORDER BY lane = 'CRITICAL' DESC, lane = 'HIGH' DESC, received_at
                     LIMIT 20 FOR UPDATE SKIP LOCKED)
          RETURNING e.producer, e.event_id, e.envelope, e.shadow, e.attempts, e.lease_id",
            vec![lease.into(), EVENT_LEASE_SECS.into(), lane.into()],
        )
        .await
    }

    async fn mark(&self, e: &ClaimedEvent, state: &str, error: Option<&str>) -> Result<(), DbErr> {
        db::exec(
            &self.db,
            "UPDATE notif_event_inbox SET state = $3, processed_at = now(), lease_id = NULL, lease_until = NULL, last_error_class = $4,
                    envelope = envelope - 'sealed'
              WHERE producer = $1 AND event_id = $2 AND lease_id IS NOT DISTINCT FROM $5",
            vec![e.producer.clone().into(), e.event_id.clone().into(), state.into(), error.map(str::to_string).into(), e.lease_id.into()],
        )
        .await?;
        Ok(())
    }

    async fn defer(&self, e: &ClaimedEvent, class: &str) -> Result<(), DbErr> {
        let wait = email::backoff(e.attempts.min(10));
        db::exec(
            &self.db,
            "UPDATE notif_event_inbox SET lease_id = NULL, lease_until = now() + make_interval(secs => $3), last_error_class = $4
              WHERE producer = $1 AND event_id = $2 AND lease_id IS NOT DISTINCT FROM $5",
            vec![e.producer.clone().into(), e.event_id.clone().into(), wait.num_seconds().into(), class.into(), e.lease_id.into()],
        )
        .await?;
        Ok(())
    }

    /// One pass over every lane (CRITICAL first). Returns the number of events handled.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<usize, DbErr> {
        let mut handled = 0;
        // CRITICAL is claimed on its own first so a STANDARD backlog never delays it.
        let mut batch = self.claim(Some("CRITICAL")).await?;
        if batch.is_empty() {
            batch = self.claim(None).await?;
        }
        for e in batch {
            handled += 1;
            let env: Envelope = match serde_json::from_value(e.envelope.clone()) {
                Ok(env) => env,
                Err(_) => {
                    self.mark(&e, "REJECTED", Some("STORED_ENVELOPE_INVALID"))
                        .await?;
                    continue;
                }
            };
            if env.event_type == CONTROL_EVENT {
                if let Some(key) = &env.delivery.cancel_key {
                    cancel(&self.db, key, now).await?;
                }
                self.mark(&e, "PROCESSED", None).await?;
                continue;
            }
            let Some(job) = Job::from_envelope(&env, e.shadow) else {
                self.mark(&e, "REJECTED", Some("TYPE_NOT_ROUTED")).await?;
                continue;
            };
            match self.process(&job, now).await {
                Ok(Processed::Done) => {
                    self.mark(&e, if e.shadow { "SHADOWED" } else { "PROCESSED" }, None)
                        .await?
                }
                Ok(Processed::Retry(class)) if e.attempts < MAX_EVENT_ATTEMPTS => {
                    self.defer(&e, class).await?
                }
                Ok(Processed::Retry(class)) => {
                    // Kept PENDING far in the future: visible to operators as a dead letter (I5).
                    self.metrics.inc(
                        "notif_event_dead_letters_total",
                        format!("class=\"{}\"", label(class)),
                    );
                    db::exec(
                        &self.db,
                        "UPDATE notif_event_inbox SET lease_id = NULL, lease_until = now() + interval '7 days', last_error_class = $3
                          WHERE producer = $1 AND event_id = $2",
                        vec![e.producer.clone().into(), e.event_id.clone().into(), class.into()],
                    )
                    .await?;
                }
                Err(err) => {
                    tracing::warn!(summary = %db::summarize_db_error(&err), "event processing failed; will retry");
                    self.defer(&e, "DATABASE").await?;
                }
            }
        }
        Ok(handled)
    }

    /// Processes one job (event or fired schedule).
    pub async fn process(&self, job: &Job, now: DateTime<Utc>) -> Result<Processed, DbErr> {
        if job.expires_at.is_some_and(|x| x <= now) {
            self.metrics.inc(
                "notif_events_expired_total",
                format!("category=\"{}\"", job.route.route_category),
            );
            return Ok(Processed::Done);
        }
        // ADR 0063 N-P4: partner end users never reach the first-party pipeline (no owner
        // lookups, preferences or CF Meet credentials).
        if job
            .recipients
            .iter()
            .any(|r| cf_notification_client::parse_platform_principal(r).is_some())
        {
            return self.process_partner(job, now).await;
        }
        let modes = routing::category(&self.db, job.route.route_category).await?;
        let mode_of = |c: Channel| -> (Mode, i32) {
            let (m, v) = modes.get(&c).copied().unwrap_or((Mode::Off, 1));
            if job.shadow {
                (Mode::Shadow, v)
            } else {
                (m, v)
            }
        };
        let any_active = job
            .route
            .channels
            .iter()
            .any(|c| mode_of(*c).0 == Mode::Active);

        // Meeting reminder arming / cancellation (§9.9).
        if job.route.route_category == "MEETING_NOTIFICATION" {
            if let Some(handled) = self.meeting_schedule(job, now).await? {
                return Ok(handled);
            }
        }

        // --- recipients and owner facts ---
        let mut recipients = job.recipients.clone();
        let mut meeting: Option<MeetingFacts> = None;
        let mut emails: HashMap<String, String> = HashMap::new();
        let mut pref_override: Option<PrefCategory> = None;
        let mut mentions: HashMap<String, bool> = HashMap::new();
        let mut private_e2ee = false;
        match job.route.route_category {
            "MEETING_NOTIFICATION" | "MEETING_INVITATION" => {
                if let Some(conf) = job.data_uuid("conferenceId") {
                    let kind = match job.event_type.as_str() {
                        "meeting-service.conference.rescheduled" => "RESCHEDULED",
                        "meeting-service.conference.cancelled" => "CANCELLED",
                        _ if job.template == "meeting.reminder" => "REMINDER",
                        _ => "SCHEDULED",
                    };
                    match self
                        .authorities
                        .conference(conf, kind, job.data_str("startsAt"))
                        .await
                    {
                        Ok(c) => {
                            if !c.still_true && kind == "REMINDER" {
                                return Ok(Processed::Done);
                            }
                            meeting = Some(MeetingFacts {
                                presented_title: c.presented_title.clone(),
                                join_url: format!(
                                    "{}{}",
                                    self.email.app_url,
                                    c.join_path.as_deref().unwrap_or("")
                                ),
                                starts_at: parse_time(Some(&c.starts_at)),
                                ends_at: parse_time(c.ends_at.as_deref()),
                                previous_start: parse_time(job.data_str("previousStartsAt")),
                                space_time_zone: c
                                    .time_zone
                                    .clone()
                                    .unwrap_or_else(|| "UTC".into()),
                            });
                            if recipients.is_empty() {
                                for r in &c.recipients {
                                    recipients.push(r.reference.clone());
                                    if let Some(e) = &r.email {
                                        emails.insert(r.reference.clone(), e.clone());
                                    }
                                }
                            }
                        }
                        Err(e) if must_retry(&e, any_active) => {
                            return Ok(Processed::Retry("MEETING_OWNER_UNAVAILABLE"))
                        }
                        Err(_) => {}
                    }
                }
            }
            "MESSAGE_WAKEUPS" => {
                let conv = job.data_uuid("conversationId");
                let seq = job.data.get("eventSequence").and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                });
                let sender = job.data_str("senderPrincipal").unwrap_or("");
                if let (Some(conv), Some(seq)) = (conv, seq) {
                    match self.authorities.wakeups(conv, seq, sender).await {
                        Ok(a) if a.conversation_active => {
                            private_e2ee = a.security_profile == "PRIVATE_E2EE";
                            pref_override = Some(if a.conversation_kind == "DIRECT" {
                                PrefCategory::DirectMessages
                            } else {
                                PrefCategory::GroupMessages
                            });
                            if recipients.is_empty() {
                                for p in a.entitled {
                                    if let Some(r) = p.account_ref {
                                        mentions.insert(r.clone(), p.mentions_recipient);
                                        recipients.push(r);
                                    }
                                }
                            }
                        }
                        Ok(_) => return Ok(Processed::Done),
                        Err(e) if must_retry(&e, any_active) => {
                            return Ok(Processed::Retry("MESSAGING_OWNER_UNAVAILABLE"))
                        }
                        Err(_) => {}
                    }
                }
                if pref_override.is_none() {
                    pref_override = Some(match job.data_str("conversationKind") {
                        Some("DIRECT") => PrefCategory::DirectMessages,
                        _ => PrefCategory::GroupMessages,
                    });
                }
                private_e2ee |= job.data_str("securityProfile") == Some("PRIVATE_E2EE");
            }
            "ORGANIZATION_UPDATES"
                if recipients.is_empty()
                    && job.event_type == "organizations.invitation.created" =>
            {
                if let Some(inv) = job.data_uuid("invitationId") {
                    match self.authorities.invitee(inv).await {
                        Ok((status, Some(r))) if status == "PENDING" => recipients.push(r),
                        Ok(_) => return Ok(Processed::Done),
                        Err(e) if must_retry(&e, any_active) => {
                            return Ok(Processed::Retry("ORGANIZATIONS_OWNER_UNAVAILABLE"))
                        }
                        Err(_) => {}
                    }
                }
            }
            "OPERATOR_ALERTS" if recipients.is_empty() => {
                recipients.push("operator-group:operators".into())
            }
            _ => {}
        }
        let (secrets, sealed_address) = self.open_secrets(job);
        if recipients.is_empty() {
            if let (Some(_), Some(inv)) = (&sealed_address, job.data_uuid("invitationId")) {
                // Email-only invitee (organization invitation): keyed by the invitation.
                recipients.push(format!("meeting-email-principal:{inv}"));
            }
        }
        if recipients.is_empty() {
            self.metrics.inc(
                "notif_events_without_recipients_total",
                format!("category=\"{}\"", job.route.route_category),
            );
            return Ok(Processed::Done);
        }

        // A CRITICAL code to an address that is not (yet) the account's (the email-change
        // code: one email principal, ACCOUNT subject): the subject account authorizes the
        // send and names the greeting; the address stays the sealed one.
        let subject_account = match &job.subject {
            Some((kind, id))
                if kind == "ACCOUNT"
                    && job.lane == Lane::Critical
                    && recipients
                        .iter()
                        .any(|r| r.starts_with("meeting-email-principal:")) =>
            {
                Some(format!("cfmeet-user:{id}"))
            }
            _ => None,
        };
        // X1 for account recipients (authorization, address, name, time zone).
        let mut accounts: Vec<String> = recipients
            .iter()
            .filter(|r| r.starts_with("cfmeet-user:"))
            .cloned()
            .collect();
        if let Some(a) = &subject_account {
            if !accounts.contains(a) {
                accounts.push(a.clone());
            }
        }
        let needs_email = job.route.has(Channel::Email) && mode_of(Channel::Email).0 != Mode::Off;
        let mut contacts: HashMap<String, AccountContact> = HashMap::new();
        let mut contacts_known = false;
        if !accounts.is_empty() {
            match self.authorities.contacts(&accounts).await {
                Ok(c) => {
                    contacts = c;
                    contacts_known = true;
                }
                Err(e)
                    if must_retry(
                        &e,
                        any_active && (needs_email || job.lane == Lane::Critical),
                    ) =>
                {
                    return Ok(Processed::Retry("ACCOUNT_OWNER_UNAVAILABLE"))
                }
                Err(_) => {}
            }
        }

        // X4 for social categories with an actor.
        let social = matches!(
            job.route.route_category,
            "CONTACT_REQUESTS"
                | "GROUP_INVITATIONS"
                | "MISSED_CALLS"
                | "VOICEMAIL"
                | "MESSAGE_WAKEUPS"
        );
        let mut restrictions = HashMap::new();
        if social && !accounts.is_empty() {
            let action = match job.route.route_category {
                "CONTACT_REQUESTS" if job.event_type.ends_with("connected") => "CONTACT_CONNECTED",
                "CONTACT_REQUESTS" => "CONTACT_REQUEST",
                // Direct messages use the Hub's direct-message rule; group messages the
                // group rule (no contact needed; blocks and holds still apply).
                "MESSAGE_WAKEUPS" if pref_override == Some(PrefCategory::DirectMessages) => {
                    "MESSAGE"
                }
                "MESSAGE_WAKEUPS" => "OTHER",
                "GROUP_INVITATIONS" => "GROUP_INVITATION",
                "MISSED_CALLS" | "VOICEMAIL" => "CALL",
                _ => "OTHER",
            };
            let actor = job
                .data_str("actorRef")
                .filter(|a| a.starts_with("cfmeet-user:"));
            match self
                .authorities
                .restrictions(actor, &accounts, action)
                .await
            {
                Ok(r) => restrictions = r,
                Err(e) if must_retry(&e, any_active) => {
                    return Ok(Processed::Retry("COMMUNICATIONS_OWNER_UNAVAILABLE"))
                }
                Err(_) => {}
            }
        }

        let params = templates::params_of(&job.data);
        let many = recipients.len() > 1;
        for recipient in &recipients {
            let on_behalf =
                recipient.starts_with("meeting-email-principal:") && subject_account.is_some();
            let contact = if on_behalf {
                subject_account.as_ref().and_then(|a| contacts.get(a))
            } else {
                contacts.get(recipient)
            };
            let authorized = match (recipient.starts_with("cfmeet-user:") || on_behalf, contact) {
                (true, Some(c)) => c.active(),
                // Unknown to Account: not authorized; unknown because X1 is not consulted
                // (shadow without owners): evaluated as authorized and recorded as such.
                (true, None) => !contacts_known,
                _ => true,
            };
            let pref = pref_override.unwrap_or(job.route.pref);
            let prefs = if recipient.starts_with("cfmeet-user:") {
                preferences::load(&self.db, recipient).await?
            } else {
                preferences::AccountPrefs::default()
            };
            let mute_level = match job.route.route_category {
                "MESSAGE_WAKEUPS" | "GROUP_INVITATIONS" => match job.data_uuid("conversationId") {
                    Some(c) => {
                        Some(preferences::conversation_level(&self.db, recipient, c, now).await?)
                    }
                    None => None,
                },
                "CONTACT_REQUESTS" => match job.data_str("actorRef") {
                    Some(a) => Some(preferences::contact_level(&self.db, recipient, a).await?),
                    None => None,
                },
                _ => None,
            };
            let tz = contact
                .and_then(|c| c.time_zone.clone())
                .unwrap_or_else(|| "UTC".into());
            let endpoints = if recipient.starts_with("cfmeet-user:") {
                push::endpoints_for(&self.db, recipient).await?
            } else {
                Vec::new()
            };
            let input = PolicyInput {
                category: Some(pref),
                channels: job
                    .route
                    .channels
                    .iter()
                    .copied()
                    .filter(|c| mode_of(*c).0 != Mode::Off)
                    .collect(),
                recipient_authorized: authorized,
                restriction: restrictions.get(recipient).copied(),
                quiet_window_end: prefs
                    .quiet_hours
                    .as_ref()
                    .and_then(|q| q.window_end(now, &tz)),
                prefs,
                mute_level,
                mentions_recipient: mentions.get(recipient).copied().unwrap_or(false),
                private_e2ee,
                has_push_endpoint: !endpoints.is_empty(),
                pushes_last_hour: if job.route.has(Channel::Push) {
                    push::pushes_last_hour(&self.db, recipient).await?
                } else {
                    0
                },
            };
            let outcome = policy::evaluate(&input);
            let address = match recipient.as_str() {
                r if r.starts_with("cfmeet-user:") => contact.and_then(|c| c.email.clone()),
                r if r.starts_with("meeting-email-principal:") => {
                    emails.get(r).cloned().or_else(|| sealed_address.clone())
                }
                _ => None,
            };
            let facts = RecipientFacts {
                display_name: contact.and_then(|c| c.display_name.clone()),
                time_zone: contact.and_then(|c| c.time_zone.clone()),
            };
            let result = self
                .deliver_one(
                    job,
                    recipient,
                    &outcome,
                    &mode_of,
                    &params,
                    address,
                    &facts,
                    &secrets,
                    meeting.as_ref(),
                    &endpoints,
                    many,
                    now,
                )
                .await?;
            if let Processed::Retry(c) = result {
                return Ok(Processed::Retry(c));
            }
        }
        Ok(Processed::Done)
    }

    #[allow(clippy::too_many_arguments)]
    async fn deliver_one(
        &self,
        job: &Job,
        recipient: &str,
        outcome: &policy::PolicyOutcome,
        mode_of: &(dyn Fn(Channel) -> (Mode, i32) + Sync),
        params: &Value,
        address: Option<String>,
        facts: &RecipientFacts,
        secrets: &Secrets,
        meeting: Option<&MeetingFacts>,
        endpoints: &[push::EndpointRef],
        many: bool,
        now: DateTime<Utc>,
    ) -> Result<Processed, DbErr> {
        let txn = self.db.begin().await?;
        let notification_id = Uuid::new_v4();
        let all_shadow = job
            .route
            .channels
            .iter()
            .all(|c| mode_of(*c).0 != Mode::Active);
        let state = if outcome.dropped.is_some() {
            "SUPPRESSED"
        } else if all_shadow {
            "SHADOW"
        } else {
            "ROUTED"
        };
        let category = job.route.spec().category.unwrap_or("OTHER");
        let inserted = db::exec(
            &txn,
            "INSERT INTO notif_notifications (id, recipient_ref, category, route_category, lane, dedupe_key, coalesce_key,
                 cancel_key, source_producer, source_event_id, subject_kind, subject_id, template_key, params,
                 policy_snapshot, state, deliver_after, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
             ON CONFLICT (recipient_ref, dedupe_key) DO NOTHING",
            vec![
                notification_id.into(),
                recipient.into(),
                category.into(),
                job.route.route_category.into(),
                job.lane.as_str().into(),
                job.dedupe_key.clone().into(),
                job.coalesce_key.clone().into(),
                job.cancel_key.clone().into(),
                job.producer.clone().into(),
                job.event_id.clone().into(),
                job.subject.as_ref().map(|s| s.0.clone()).into(),
                job.subject.as_ref().map(|s| s.1).into(),
                job.template.into(),
                params.clone().into(),
                serde_json::to_value(outcome).unwrap_or(json!({})).into(),
                state.into(),
                job.deliver_after.into(),
                job.expires_at.into(),
            ],
        )
        .await?;
        if inserted == 0 {
            // Already handled (retry, duplicate event, reroute replay): nothing new.
            txn.rollback().await?;
            self.metrics
                .inc("notif_notifications_deduplicated_total", "");
            return Ok(Processed::Done);
        }
        let recipient_digest = self.keys.digest("recipient", recipient);
        if job.shadow {
            observe(
                &txn,
                job,
                &recipient_digest,
                "ALL",
                "LEGACY",
                "SENT_BY_LEGACY",
                None,
            )
            .await?;
        }
        if let Some(reason) = outcome.dropped {
            for c in job.route.channels {
                if mode_of(*c).0 == Mode::Off {
                    continue;
                }
                delivery(
                    &txn,
                    notification_id,
                    *c,
                    job.lane,
                    "none",
                    "SUPPRESSED",
                    Some(reason),
                    mode_of(*c).1,
                    None,
                )
                .await?;
                if mode_of(*c).0 == Mode::Shadow {
                    observe(&txn, job, &recipient_digest, c.as_str(), "NS", reason, None).await?;
                }
            }
            txn.commit().await?;
            return Ok(Processed::Done);
        }
        for (name, decision) in &outcome.channels {
            let Some(channel) = Channel::parse(name) else {
                continue;
            };
            let (mode, version) = mode_of(channel);
            if mode == Mode::Off {
                continue;
            }
            let shadow = mode == Mode::Shadow;
            match decision {
                ChannelDecision::Suppress { reason } => {
                    delivery(
                        &txn,
                        notification_id,
                        channel,
                        job.lane,
                        "none",
                        "SUPPRESSED",
                        Some(reason),
                        version,
                        None,
                    )
                    .await?;
                    if shadow {
                        observe(&txn, job, &recipient_digest, name, "NS", reason, None).await?;
                    }
                }
                ChannelDecision::Deliver | ChannelDecision::Defer { .. } => {
                    let defer_until = match decision {
                        ChannelDecision::Defer { until } => Some(*until),
                        _ => job.deliver_after.filter(|d| *d > now),
                    };
                    match channel {
                        Channel::InApp => {
                            if shadow {
                                delivery(
                                    &txn,
                                    notification_id,
                                    channel,
                                    job.lane,
                                    "inbox",
                                    "SHADOW",
                                    None,
                                    version,
                                    None,
                                )
                                .await?;
                                observe(
                                    &txn,
                                    job,
                                    &recipient_digest,
                                    name,
                                    "NS",
                                    "WOULD_DELIVER",
                                    None,
                                )
                                .await?;
                            } else {
                                let expires = now + Duration::days(job.route.inbox_days);
                                inbox::write(
                                    &txn,
                                    inbox::NewItem {
                                        id: notification_id,
                                        recipient_ref: recipient,
                                        category,
                                        template: job.template,
                                        params,
                                        action_kind: job.route.action,
                                        action_ref: job
                                            .route
                                            .action_ref
                                            .and_then(|k| job.data_uuid(k)),
                                        group_key: job.coalesce_key.as_deref(),
                                        silent: outcome.silent_inbox,
                                        expires_at: expires,
                                    },
                                )
                                .await?;
                                delivery(
                                    &txn,
                                    notification_id,
                                    channel,
                                    job.lane,
                                    "inbox",
                                    "SENT",
                                    None,
                                    version,
                                    None,
                                )
                                .await?;
                                self.metrics.inc(
                                    "notif_inbox_written_total",
                                    format!("category=\"{category}\""),
                                );
                            }
                        }
                        Channel::Push => {
                            if endpoints.is_empty() {
                                continue;
                            }
                            for ep in endpoints {
                                let status = if shadow {
                                    "SHADOW"
                                } else if defer_until.is_some() {
                                    "DEFERRED"
                                } else {
                                    "PENDING"
                                };
                                delivery(
                                    &txn,
                                    notification_id,
                                    channel,
                                    job.lane,
                                    &ep.id.to_string(),
                                    status,
                                    None,
                                    version,
                                    defer_until,
                                )
                                .await?;
                            }
                            if shadow {
                                observe(
                                    &txn,
                                    job,
                                    &recipient_digest,
                                    name,
                                    "NS",
                                    "WOULD_DELIVER",
                                    None,
                                )
                                .await?;
                            }
                        }
                        Channel::Email | Channel::Operator => {
                            let targets: Vec<String> = if channel == Channel::Operator {
                                self.operator_emails.clone()
                            } else {
                                address.clone().into_iter().collect()
                            };
                            let rendered = templates::render_email(
                                job.template,
                                &job.data,
                                facts,
                                secrets,
                                meeting,
                                &self.email.app_url,
                            );
                            if shadow {
                                let digest = rendered.as_ref().ok().map(|r| r.digest());
                                let decision = if targets.is_empty() {
                                    "UNRESOLVED_ADDRESS"
                                } else {
                                    "WOULD_DELIVER"
                                };
                                delivery(
                                    &txn,
                                    notification_id,
                                    channel,
                                    job.lane,
                                    "address",
                                    "SHADOW",
                                    None,
                                    version,
                                    None,
                                )
                                .await?;
                                observe(&txn, job, &recipient_digest, name, "NS", decision, digest)
                                    .await?;
                                continue;
                            }
                            let rendered = match rendered {
                                Ok(r) => r,
                                Err(templates::RenderError::SecretUnavailable) => {
                                    txn.rollback().await?;
                                    return Ok(Processed::Retry("SEALED_PART_UNAVAILABLE"));
                                }
                                Err(templates::RenderError::MeetingUnavailable) => {
                                    txn.rollback().await?;
                                    return Ok(Processed::Retry("MEETING_FACTS_UNAVAILABLE"));
                                }
                                Err(templates::RenderError::NoEmail) => continue,
                            };
                            if targets.is_empty() {
                                delivery(
                                    &txn,
                                    notification_id,
                                    channel,
                                    job.lane,
                                    "address",
                                    "FAILED",
                                    Some("RECIPIENT_UNRESOLVED"),
                                    version,
                                    None,
                                )
                                .await?;
                                continue;
                            }
                            for (i, to) in targets.iter().enumerate() {
                                let target = if channel == Channel::Operator {
                                    format!("operator:{i}")
                                } else {
                                    "address".to_string()
                                };
                                let delivery_id = delivery(
                                    &txn,
                                    notification_id,
                                    channel,
                                    job.lane,
                                    &target,
                                    "PENDING",
                                    None,
                                    version,
                                    None,
                                )
                                .await?;
                                let key = match (
                                    &job.idempotency_key,
                                    many || channel == Channel::Operator,
                                ) {
                                    (Some(k), false) => k.clone(),
                                    (Some(k), true) => {
                                        format!("{k}:{}", &self.keys.email_digest(to)[..16])
                                    }
                                    (None, _) => format!("notif:{notification_id}:{target}"),
                                };
                                let lane = if job.lane == Lane::Critical {
                                    "CRITICAL"
                                } else {
                                    "STANDARD"
                                };
                                let expires = job.expires_at.unwrap_or_else(|| {
                                    let ttl = job
                                        .data
                                        .get("ttlMinutes")
                                        .and_then(Value::as_i64)
                                        .unwrap_or(2 * 24 * 60);
                                    now + Duration::minutes(ttl)
                                });
                                let r = email::enqueue(
                                    &txn,
                                    &self.keys,
                                    NewEmail {
                                        category: job
                                            .route
                                            .email_category
                                            .unwrap_or("NOTIFICATION")
                                            .to_string(),
                                        lane,
                                        idempotency_key: key,
                                        route_decision_id: (lane == "CRITICAL")
                                            .then_some(job.decision_id),
                                        sender_profile: job.route.sender_profile.to_string(),
                                        delivery_id: Some(delivery_id),
                                        to: to.clone(),
                                        to_name: facts.display_name.clone(),
                                        subject: rendered.subject.clone(),
                                        html: rendered.html.clone(),
                                        text: rendered.text.clone(),
                                        expires_at: expires,
                                    },
                                )
                                .await;
                                match r {
                                    Ok(e) if e.suppressed => {
                                        db::exec(&txn, "UPDATE notif_deliveries SET status = 'SUPPRESSED', last_error_class = 'RECIPIENT_SUPPRESSED', completed_at = now() WHERE id = $1", vec![delivery_id.into()]).await?;
                                    }
                                    Ok(_) => {}
                                    Err(email::EnqueueError::Db(e)) => return Err(e),
                                    Err(_) => {
                                        db::exec(&txn, "UPDATE notif_deliveries SET status = 'FAILED', last_error_class = 'INVALID_ADDRESS', completed_at = now() WHERE id = $1", vec![delivery_id.into()]).await?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        txn.commit().await?;
        self.metrics.inc(
            "notif_notifications_total",
            format!(
                "category=\"{}\",state=\"{state}\"",
                job.route.route_category
            ),
        );
        Ok(Processed::Done)
    }

    /// ADR 0063 N-P4: a metadata-only partner event (`messaging.message.created` to
    /// `platform-principal:<env>:<principal>` recipients). One notification per recipient
    /// (`UNIQUE (recipient_ref, dedupe_key)`), one `partner:<endpoint>` PUSH delivery per
    /// enabled APNS / FCM installation, dispatched by the partner dispatcher with that
    /// environment's own credential. Ids only; a newer message of the same conversation
    /// cancels older undelivered ones (and the provider collapses per conversation). Gated
    /// by the flag alone, not by the first-party routing modes (there is no legacy partner
    /// path to cut over from); never sent for producer shadow copies.
    async fn process_partner(&self, job: &Job, now: DateTime<Utc>) -> Result<Processed, DbErr> {
        if !self.partner_push || job.shadow {
            self.metrics.inc(
                "notif_partner_events_skipped_total",
                if job.shadow {
                    "reason=\"SHADOW\""
                } else {
                    "reason=\"DISABLED\""
                },
            );
            return Ok(Processed::Done);
        }
        let mut params = serde_json::Map::new();
        for key in ["conversationId", "eventSequence", "sequence"] {
            if let Some(v) = job.data.get(key) {
                params.insert(key.into(), v.clone());
            }
        }
        let params = Value::Object(params);
        let coalesce = job
            .data_uuid("conversationId")
            .map(|c| format!("partner-conversation:{c}"));
        let expires = job.expires_at.unwrap_or_else(|| push::default_expiry(now));
        let category = job.route.spec().category.unwrap_or("OTHER");
        for recipient in &job.recipients {
            let Some((environment, principal)) =
                cf_notification_client::parse_platform_principal(recipient)
            else {
                continue;
            };
            let endpoints =
                crate::partner::message_endpoints(&self.db, environment, principal, job.scope)
                    .await?;
            let txn = self.db.begin().await?;
            let notification_id = Uuid::new_v4();
            let inserted = db::exec(
                &txn,
                "INSERT INTO notif_notifications (id, recipient_ref, category, route_category, lane, dedupe_key, coalesce_key,
                     cancel_key, source_producer, source_event_id, subject_kind, subject_id, template_key, params,
                     policy_snapshot, state, deliver_after, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
                 ON CONFLICT (recipient_ref, dedupe_key) DO NOTHING",
                vec![
                    notification_id.into(),
                    recipient.clone().into(),
                    category.into(),
                    job.route.route_category.into(),
                    job.lane.as_str().into(),
                    job.dedupe_key.clone().into(),
                    coalesce.clone().into(),
                    job.cancel_key.clone().into(),
                    job.producer.clone().into(),
                    job.event_id.clone().into(),
                    job.subject.as_ref().map(|s| s.0.clone()).into(),
                    job.subject.as_ref().map(|s| s.1).into(),
                    job.template.into(),
                    params.clone().into(),
                    json!({ "partner": true }).into(),
                    if endpoints.is_empty() { "SUPPRESSED" } else { "ROUTED" }.into(),
                    job.deliver_after.into(),
                    Some(expires).into(),
                ],
            )
            .await?;
            if inserted == 0 {
                txn.rollback().await?;
                self.metrics
                    .inc("notif_notifications_deduplicated_total", "");
                continue;
            }
            if let Some(key) = &coalesce {
                db::exec(
                    &txn,
                    "UPDATE notif_deliveries SET status = 'CANCELLED', last_error_class = 'COLLAPSED', completed_at = now()
                      WHERE status IN ('PENDING', 'DEFERRED') AND channel = 'PUSH' AND target_ref LIKE 'partner:%'
                        AND notification_id IN (SELECT id FROM notif_notifications
                                                 WHERE recipient_ref = $1 AND coalesce_key = $2 AND id <> $3)",
                    vec![recipient.clone().into(), key.clone().into(), notification_id.into()],
                )
                .await?;
            }
            if endpoints.is_empty() {
                delivery(
                    &txn,
                    notification_id,
                    Channel::Push,
                    job.lane,
                    "none",
                    "SUPPRESSED",
                    Some("NO_REGISTRATION"),
                    1,
                    None,
                )
                .await?;
                self.metrics.inc(
                    "notif_partner_push_failed_total",
                    format!(
                        "environment=\"{environment}\",lane=\"message\",platform=\"NONE\",class=\"NO_REGISTRATION\""
                    ),
                );
            }
            let defer = job.deliver_after.filter(|d| *d > now);
            for ep in &endpoints {
                delivery(
                    &txn,
                    notification_id,
                    Channel::Push,
                    job.lane,
                    &format!("{}{}", crate::partner::TARGET_PREFIX, ep.id),
                    if defer.is_some() { "DEFERRED" } else { "PENDING" },
                    None,
                    1,
                    defer,
                )
                .await?;
            }
            txn.commit().await?;
            self.metrics.inc(
                "notif_notifications_total",
                format!(
                    "category=\"{}\",state=\"{}\"",
                    job.route.route_category,
                    if endpoints.is_empty() { "SUPPRESSED" } else { "ROUTED" }
                ),
            );
        }
        Ok(Processed::Done)
    }

    /// Reminder arming / cancellation. `Some(_)`: the job is fully handled here.
    async fn meeting_schedule(
        &self,
        job: &Job,
        now: DateTime<Utc>,
    ) -> Result<Option<Processed>, DbErr> {
        let Some(conf) = job.data_uuid("conferenceId") else {
            return Ok(None);
        };
        let cancel_key = format!("conference-reminder:{conf}");
        let starts = parse_time(job.data_str("startsAt"));
        match job.event_type.as_str() {
            "meeting-service.conference.scheduled" if !job.dedupe_key.starts_with("reminder:") => {
                if let Some(s) = starts {
                    self.arm_reminder(job, conf, s, now).await?;
                }
                Ok(Some(Processed::Done))
            }
            "meeting-service.conference.rescheduled" => {
                cancel_schedules(&self.db, &cancel_key).await?;
                if let Some(s) = starts {
                    self.arm_reminder(job, conf, s, now).await?;
                }
                Ok(None)
            }
            "meeting-service.conference.cancelled" => {
                cancel_schedules(&self.db, &cancel_key).await?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn arm_reminder(
        &self,
        job: &Job,
        conf: Uuid,
        starts: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        let boundary = routing::reminder_boundary(&self.db).await?;
        if !boundary.is_some_and(|b| starts >= b) || job.shadow {
            return Ok(());
        }
        let release = starts - Duration::minutes(REMINDER_LEAD_MINUTES);
        if release <= now {
            return Ok(());
        }
        db::exec(
            &self.db,
            "INSERT INTO notif_schedules (schedule_key, kind, release_at, cancel_key, payload)
             VALUES ($1, 'REMINDER', $2, $3, $4) ON CONFLICT (schedule_key) DO NOTHING",
            vec![
                format!("reminder:{conf}:{}", starts.timestamp()).into(),
                release.into(),
                format!("conference-reminder:{conf}").into(),
                json!({ "conferenceId": conf, "startsAt": starts, "producer": job.producer, "eventId": job.event_id, "decisionId": job.decision_id }).into(),
            ],
        )
        .await?;
        Ok(())
    }

    /// W4 part: fires due reminder schedules (after the takeover boundary).
    pub async fn fire_schedules(&self, now: DateTime<Utc>) -> Result<usize, DbErr> {
        #[derive(FromQueryResult)]
        struct Due {
            schedule_key: String,
            payload: Value,
        }
        let lease = Uuid::new_v4();
        let due: Vec<Due> = db::all(
            &self.db,
            "UPDATE notif_schedules s SET lease_id = $1, lease_until = now() + interval '60 seconds'
              WHERE s.schedule_key IN (SELECT schedule_key FROM notif_schedules
                     WHERE state = 'PENDING' AND release_at <= now() AND (lease_until IS NULL OR lease_until < now())
                     ORDER BY release_at LIMIT 20 FOR UPDATE SKIP LOCKED)
          RETURNING s.schedule_key, s.payload",
            vec![lease.into()],
        )
        .await?;
        let route = catalogue::route("meeting-service.conference.scheduled").expect("routed");
        let mut fired = 0;
        for d in due {
            let p = &d.payload;
            let job = Job {
                producer: p["producer"]
                    .as_str()
                    .unwrap_or("cfmeet.meeting-service")
                    .to_string(),
                event_id: p["eventId"].as_str().unwrap_or_default().to_string(),
                event_type: "meeting-service.conference.scheduled".into(),
                route,
                template: "meeting.reminder",
                lane: Lane::Standard,
                data: BTreeMap::from([
                    ("conferenceId".to_string(), p["conferenceId"].clone()),
                    ("startsAt".to_string(), p["startsAt"].clone()),
                ]),
                recipients: Vec::new(),
                dedupe_key: d.schedule_key.clone(),
                idempotency_key: Some(format!("meeting-notification:{}", d.schedule_key)),
                coalesce_key: None,
                cancel_key: Some(format!(
                    "conference-reminder:{}",
                    p["conferenceId"].as_str().unwrap_or_default()
                )),
                deliver_after: None,
                expires_at: parse_time(p["startsAt"].as_str()),
                decision_id: p["decisionId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .unwrap_or_else(Uuid::nil),
                subject: None,
                sealed: None,
                shadow: false,
                scope: None,
            };
            match self.process(&job, now).await? {
                Processed::Done => {
                    db::exec(&self.db, "UPDATE notif_schedules SET state = 'FIRED', fired_at = now(), lease_id = NULL, lease_until = NULL WHERE schedule_key = $1", vec![d.schedule_key.into()]).await?;
                    fired += 1;
                }
                Processed::Retry(_) => {
                    db::exec(&self.db, "UPDATE notif_schedules SET lease_id = NULL, lease_until = now() + interval '30 seconds' WHERE schedule_key = $1", vec![d.schedule_key.into()]).await?;
                }
            }
        }
        Ok(fired)
    }

    pub async fn run(
        &self,
        poll: StdDuration,
        heartbeats: Arc<Heartbeats>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tracing::info!("ingest processor started");
        loop {
            if *shutdown.borrow() {
                break;
            }
            heartbeats.beat("ingest");
            let busy = match self.tick(Utc::now()).await {
                Ok(n) => n > 0,
                Err(e) => {
                    tracing::error!(summary = %db::summarize_db_error(&e), "ingest tick failed");
                    false
                }
            };
            if !busy {
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn delivery<C: ConnectionTrait>(
    conn: &C,
    notification: Uuid,
    channel: Channel,
    lane: Lane,
    target: &str,
    status: &str,
    error: Option<&str>,
    routing_version: i32,
    defer_until: Option<DateTime<Utc>>,
) -> Result<Uuid, DbErr> {
    let id = Uuid::new_v4();
    let final_status = matches!(status, "SENT" | "SUPPRESSED" | "SHADOW" | "FAILED");
    db::exec(
        conn,
        "INSERT INTO notif_deliveries (id, notification_id, channel, lane, target_ref, status, last_error_class, routing_version,
             next_attempt_at, completed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9, now()), CASE WHEN $10 THEN now() END)
         ON CONFLICT (notification_id, channel, target_ref) DO NOTHING",
        vec![
            id.into(),
            notification.into(),
            channel.as_str().into(),
            lane.as_str().into(),
            target.into(),
            status.into(),
            error.map(str::to_string).into(),
            routing_version.into(),
            defer_until.into(),
            final_status.into(),
        ],
    )
    .await?;
    Ok(id)
}

async fn observe<C: ConnectionTrait>(
    conn: &C,
    job: &Job,
    recipient_digest: &str,
    channel: &str,
    side: &str,
    decision: &str,
    render_digest: Option<String>,
) -> Result<(), DbErr> {
    db::exec(
        conn,
        "INSERT INTO notif_shadow_observations (category, dedupe_key, side, recipient_digest, channel, decision, render_digest)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
        vec![
            job.route.route_category.into(),
            job.dedupe_key.clone().into(),
            side.into(),
            recipient_digest.into(),
            channel.into(),
            decision.chars().take(16).collect::<String>().into(),
            render_digest.into(),
        ],
    )
    .await?;
    Ok(())
}

async fn cancel_schedules<C: ConnectionTrait>(conn: &C, cancel_key: &str) -> Result<u64, DbErr> {
    db::exec(conn, "UPDATE notif_schedules SET state = 'CANCELLED' WHERE cancel_key = $1 AND state = 'PENDING'", vec![cancel_key.into()]).await
}

/// `notifications.cancel` / `cancelKey`: never-dispatched deliveries and schedules with the
/// key are cancelled; queued email for them never leaves.
pub async fn cancel<C: ConnectionTrait>(
    conn: &C,
    cancel_key: &str,
    _now: DateTime<Utc>,
) -> Result<u64, DbErr> {
    cancel_schedules(conn, cancel_key).await?;
    db::exec(
        conn,
        "UPDATE notif_email_messages SET status = 'FAILED', last_error_class = 'CANCELLED', body_sealed = NULL, final_at = now(), updated_at = now()
          WHERE status = 'QUEUED' AND delivery_id IN (
                SELECT d.id FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id
                 WHERE n.cancel_key = $1 AND d.status IN ('PENDING', 'DEFERRED'))",
        vec![cancel_key.into()],
    )
    .await?;
    db::exec(
        conn,
        "UPDATE notif_deliveries SET status = 'CANCELLED', completed_at = now()
          WHERE status IN ('PENDING', 'DEFERRED') AND notification_id IN (SELECT id FROM notif_notifications WHERE cancel_key = $1)",
        vec![cancel_key.into()],
    )
    .await?;
    db::exec(
        conn,
        "UPDATE notif_notifications SET state = 'CANCELLED' WHERE cancel_key = $1 AND state IN ('ROUTED', 'HELD')",
        vec![cancel_key.into()],
    )
    .await
}

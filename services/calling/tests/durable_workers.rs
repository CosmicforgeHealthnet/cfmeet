//! Durable work of Calling (§81, §82, §86): leg release committed with the transition,
//! Telephony command leases / claims / attempt cap, the outbox publisher's lease recovery
//! and cap, the generic call-setup deadline and the worker-backlog incident.
//!
//! "Crash" windows are simulated deterministically: a transition committed through the
//! call domain alone (the application's post-commit step never runs), a claim written
//! as a dead worker would have left it, a send held inside the scripted Telephony while
//! the clock moves past its lease.

mod common;

use std::sync::Mutex;

use axum::http::StatusCode;
use calling::{
    calls::{app::AppCallingConfig, ids::CallId, transitions::CallAction, CallState},
    domain::Clock,
    telephony::{events::TelephonyEvent, LegControl},
    webhooks::worker::{EventTransport, OUTBOX_MAX_ATTEMPTS},
};
use chrono::Duration;
use common::{
    public::PublicWorld,
    telephony::{leg_event, Answer},
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

/// A dialing app-to-phone Call: (call, public leg, Telephony leg).
async fn dialing(x: &PublicWorld, key: &str) -> (Uuid, Uuid, Uuid) {
    let r = x.call_to(x.contact, key).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let leg: Uuid =
        x.w.h()
            .conn()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!(
                "SELECT id FROM call_legs WHERE call_id = '{call}' AND leg_kind = 'PUBLIC_NETWORK'"
            ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
    let t = x
        .telephony
        .telephony_leg_of(leg)
        .expect("telephony accepted the originate");
    (call, leg, t)
}

/// Cancels `call` through the call domain only: the transition commits, the
/// application's post-commit leg release never runs (a crash right after the commit).
async fn cancel_then_crash(x: &PublicWorld, call: Uuid) {
    let (_, version) = x.w.state(call).await;
    x.w.w
        .calls()
        .transition(
            &x.w.w.cmd,
            CallId::from_uuid(call),
            version,
            CallAction::Cancel,
        )
        .await
        .unwrap();
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct Command {
    id: Uuid,
    status: String,
    attempts: i32,
    last_error_code: Option<String>,
    payload: Value,
}

async fn terminate_of(x: &PublicWorld, leg: Uuid) -> Option<Command> {
    use sea_orm::FromQueryResult;
    Command::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "SELECT id, status, attempts, last_error_code, payload FROM call_telephony_commands \
             WHERE call_leg_id = '{leg}' AND kind = 'TERMINATE'"
        ),
    ))
    .one(x.w.h().conn())
    .await
    .unwrap()
}

fn terminates_for(x: &PublicWorld, command: Uuid) -> usize {
    x.telephony
        .controls()
        .iter()
        .filter(|(_, action, body)| {
            *action == LegControl::Terminate && body["commandId"] == json!(command)
        })
        .count()
}

async fn event(x: &PublicWorld, value: Value) -> &'static str {
    let event: TelephonyEvent = serde_json::from_value(value).unwrap();
    x.w.h()
        .state
        .app
        .apply_telephony_event(&event)
        .await
        .unwrap()
        .as_str()
}

// ---------------------------------------------------------------------------
// §81: TERMINATE intent written in the SAME transaction as the transition.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_terminate_intent_commits_with_the_terminal_transition_and_survives_a_crash() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let (call, leg, t) = dialing(&x, "crash-1").await;
    cancel_then_crash(&x, call).await;
    assert_eq!(x.w.state(call).await.0, CallState::Cancelled);

    // The crash left the paid leg live at Telephony, but its TERMINATE is durable.
    assert!(x
        .telephony
        .controls()
        .iter()
        .all(|(_, a, _)| *a != LegControl::Terminate));
    let command = terminate_of(&x, leg).await.expect("durable TERMINATE");
    assert_eq!(command.status, "PENDING");
    assert_eq!(command.payload["commandId"], json!(command.id));
    assert_eq!(command.payload["reason"], "CALL_ENDED");
    // It carries the creating request's correlation id (not a fresh one).
    let correlation = x
        .count(&format!(
            "SELECT count(*) FROM calls WHERE id = '{call}' AND correlation_id = '{}'",
            command.payload["correlationId"].as_str().unwrap()
        ))
        .await;
    assert_eq!(correlation, 1);

    // The dispatch worker (any instance) sends it exactly once.
    x.w.h()
        .state
        .app
        .dispatch_telephony_commands(50)
        .await
        .unwrap();
    x.w.h()
        .state
        .app
        .dispatch_telephony_commands(50)
        .await
        .unwrap();
    assert_eq!(terminates_for(&x, command.id), 1);
    let sent = x
        .telephony
        .controls()
        .into_iter()
        .find(|(_, a, _)| *a == LegControl::Terminate)
        .unwrap();
    assert_eq!(sent.0, t);
    assert_eq!(sent.2["correlationId"], command.payload["correlationId"]);
    assert_eq!(terminate_of(&x, leg).await.unwrap().status, "ACCEPTED");

    // A leg the same transaction already ended gets no TERMINATE (the intent is decided
    // at commit, from the legs as the transaction leaves them).
    let (call2, leg2, t2) = dialing(&x, "crash-2").await;
    let attempt: Uuid =
        x.w.h()
            .conn()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT id FROM call_attempts WHERE leg_id = '{leg2}'"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
    let mut busy = leg_event("LEG_BUSY", call2, leg2, t2, Some(attempt), 1);
    busy["endReason"] = json!("BUSY");
    busy["legStatus"] = json!("FAILED");
    busy["definite"] = json!(true);
    assert_eq!(event(&x, busy).await, "APPLIED");
    assert_eq!(x.w.state(call2).await.0, CallState::Busy);
    assert!(terminate_of(&x, leg2).await.is_none());
}

// ---------------------------------------------------------------------------
// §81: command leases, claims and the attempt cap.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_dispatch_workers_send_each_command_exactly_once() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let mut commands = Vec::new();
    for round in 0..4 {
        let mut legs = Vec::new();
        for n in 0..3 {
            let (call, leg, _) = dialing(&x, &format!("race-{round}-{n}")).await;
            cancel_then_crash(&x, call).await;
            legs.push(leg);
        }
        let (a, b) = (x.w.h().state.app.clone(), x.w.h().state.app.clone());
        let first = tokio::spawn(async move { a.dispatch_telephony_commands(50).await });
        let second = tokio::spawn(async move { b.dispatch_telephony_commands(50).await });
        let (first, second) = (
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
        );
        assert_eq!(
            first + second,
            3,
            "round {round}: each command claimed once"
        );
        for leg in legs {
            let command = terminate_of(&x, leg).await.unwrap();
            assert_eq!(command.status, "ACCEPTED");
            commands.push(command.id);
        }
    }
    for command in &commands {
        assert_eq!(terminates_for(&x, *command), 1, "sent exactly once");
    }
}

#[tokio::test]
async fn a_claim_whose_worker_died_is_reclaimed_after_its_lease_and_sent_once() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let (call, leg, _) = dialing(&x, "lease-1").await;
    cancel_then_crash(&x, call).await;
    let command = terminate_of(&x, leg).await.unwrap();
    // A worker claimed it (30 s lease) and died before writing any outcome.
    let now = x.w.h().clock.now();
    common::exec(
        x.w.h().conn(),
        &format!(
            "UPDATE call_telephony_commands SET lease_until = '{}', lease_claim = gen_random_uuid() \
             WHERE id = '{}'",
            (now + Duration::seconds(30)).to_rfc3339(),
            command.id
        ),
    )
    .await
    .unwrap();
    let app = &x.w.h().state.app;
    assert_eq!(app.dispatch_telephony_commands(50).await.unwrap(), 0);
    assert_eq!(
        terminates_for(&x, command.id),
        0,
        "a live lease is respected"
    );
    x.w.h().clock.advance(Duration::seconds(31));
    assert_eq!(app.dispatch_telephony_commands(50).await.unwrap(), 1);
    assert_eq!(app.dispatch_telephony_commands(50).await.unwrap(), 0);
    assert_eq!(terminates_for(&x, command.id), 1);
    assert_eq!(terminate_of(&x, leg).await.unwrap().status, "ACCEPTED");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_worker_cannot_complete_or_reschedule_after_losing_its_lease() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let (call, leg, _) = dialing(&x, "stale-1").await;
    cancel_then_crash(&x, call).await;
    let command = terminate_of(&x, leg).await.unwrap().id;

    // Worker A sends and hangs inside Telephony past its lease; worker B reclaims the
    // command and gets "unavailable" (reschedule). A's late success must not complete it.
    let gate = x.telephony.gate_next_control(Answer::Accept);
    x.telephony.script_controls(&[Answer::Unavailable]);
    let a = x.w.h().state.app.clone();
    let stale = tokio::spawn(async move { a.dispatch_telephony_commands(50).await });
    gate.entered.notified().await;
    x.w.h().clock.advance(Duration::seconds(31));
    x.w.h()
        .state
        .app
        .dispatch_telephony_commands(50)
        .await
        .unwrap();
    gate.release.notify_one();
    stale.await.unwrap().unwrap();
    let row = terminate_of(&x, leg).await.unwrap();
    assert_eq!(
        row.status, "PENDING",
        "the stale success did not complete it"
    );
    assert_eq!(row.attempts, 1, "only the live claim's reschedule counted");
    assert_eq!(
        row.last_error_code.as_deref(),
        Some("TELEPHONY_UNAVAILABLE")
    );

    // Reverse: A hangs and then fails; B reclaims and succeeds. A's late failure must not
    // reschedule (or count an attempt on) the completed command.
    x.w.h().clock.advance(Duration::seconds(5));
    let gate = x.telephony.gate_next_control(Answer::Unavailable);
    let a = x.w.h().state.app.clone();
    let stale = tokio::spawn(async move { a.dispatch_telephony_commands(50).await });
    gate.entered.notified().await;
    x.w.h().clock.advance(Duration::seconds(31));
    x.w.h()
        .state
        .app
        .dispatch_telephony_commands(50)
        .await
        .unwrap();
    gate.release.notify_one();
    stale.await.unwrap().unwrap();
    let row = terminate_of(&x, leg).await.unwrap();
    assert_eq!(row.status, "ACCEPTED");
    assert_eq!(row.attempts, 1, "the stale failure changed nothing");
    // Every send carried the same command id / idempotency key (Telephony dedupes).
    assert_eq!(terminates_for(&x, command), 4);
}

#[tokio::test]
async fn a_command_at_its_attempt_cap_goes_dead_stops_retrying_and_raises_one_incident() {
    let Some(x) =
        PublicWorld::configured(|rt| rt.app_config.telephony_command_max_attempts = 3).await
    else {
        return;
    };
    let (call, leg, _) = dialing(&x, "cap-1").await;
    cancel_then_crash(&x, call).await;
    let command = terminate_of(&x, leg).await.unwrap().id;
    x.telephony.script_controls(&vec![Answer::Unavailable; 10]);
    for _ in 0..8 {
        x.w.h()
            .state
            .app
            .dispatch_telephony_commands(50)
            .await
            .unwrap();
        x.w.h().clock.advance(Duration::seconds(60));
    }
    let row = terminate_of(&x, leg).await.unwrap();
    assert_eq!(row.status, "DEAD");
    assert_eq!(row.attempts, 3);
    assert_eq!(
        row.last_error_code.as_deref(),
        Some("MAX_ATTEMPTS_EXCEEDED")
    );
    assert_eq!(terminates_for(&x, command), 3, "no retry after DEAD");
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM calling_incidents WHERE category = 'TELEPHONY_COMMAND_DEAD' \
             AND resource_type = 'TELEPHONY_COMMAND' AND resource_id = '{command}' \
             AND call_id = '{call}' AND status = 'OPEN' AND occurrences = 1"
        ))
        .await,
        1
    );
    assert_eq!(
        x.count("SELECT count(*) FROM calling_incidents").await,
        1,
        "one incident"
    );
    assert!(
        calling::metrics::counter(
            "calling_telephony_commands_dead_total",
            &[("kind", "TERMINATE"), ("reason", "MAX_ATTEMPTS_EXCEEDED")]
        ) >= 1
    );
}

// ---------------------------------------------------------------------------
// §81: outbox publisher lease recovery and cap.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Recording(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl EventTransport for Recording {
    async fn publish(&self, _producer: &str, envelope: &Value) -> Result<(), &'static str> {
        self.0.lock().unwrap().push(
            envelope["event_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        Ok(())
    }
}

impl Recording {
    fn times(&self, event_id: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| *e == event_id)
            .count()
    }
}

/// The oldest pending outbox event (id, event id).
async fn pending_outbox(x: &PublicWorld) -> (Uuid, String) {
    let row =
        x.w.h()
            .conn()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT id, event_id FROM calling_outbox_events WHERE status = 'PENDING' \
             AND lease_until IS NULL ORDER BY created_at, id LIMIT 1",
            ))
            .await
            .unwrap()
            .unwrap();
    (
        row.try_get_by_index(0).unwrap(),
        row.try_get_by_index(1).unwrap(),
    )
}

#[tokio::test]
async fn an_outbox_claim_lost_to_a_crash_is_reclaimed_after_its_lease_and_published_once() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    dialing(&x, "outbox-1").await;
    let (id, event_id) = pending_outbox(&x).await;
    // Claimed (60 s lease, one attempt counted) by a publisher that then crashed.
    let now = x.w.h().clock.now();
    common::exec(
        x.w.h().conn(),
        &format!(
            "UPDATE calling_outbox_events SET lease_until = '{}', attempt_count = attempt_count + 1 \
             WHERE id = '{id}'",
            (now + Duration::seconds(60)).to_rfc3339()
        ),
    )
    .await
    .unwrap();
    let transport = Recording::default();
    let webhooks = &x.w.h().state.webhooks;
    webhooks.publish_outbox_once(&transport, 500).await.unwrap();
    assert_eq!(transport.times(&event_id), 0, "the lease is respected");
    x.w.h().clock.advance(Duration::seconds(61));
    webhooks.publish_outbox_once(&transport, 500).await.unwrap();
    webhooks.publish_outbox_once(&transport, 500).await.unwrap();
    assert_eq!(
        transport.times(&event_id),
        1,
        "reclaimed and published once"
    );
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM calling_outbox_events WHERE id = '{id}' AND status = 'PUBLISHED' \
             AND attempt_count = 2 AND lease_until IS NULL"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn an_outbox_claim_crashing_at_its_attempt_cap_is_dead_lettered_not_reclaimed_forever() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    dialing(&x, "outbox-cap").await;
    let transport = Recording::default();
    let webhooks = &x.w.h().state.webhooks;
    // Two events whose publisher crashed on their last allowed attempt.
    let mut poisoned = Vec::new();
    for _ in 0..2 {
        let (id, event_id) = pending_outbox(&x).await;
        let now = x.w.h().clock.now();
        common::exec(
            x.w.h().conn(),
            &format!(
                "UPDATE calling_outbox_events SET lease_until = '{}', attempt_count = {OUTBOX_MAX_ATTEMPTS} \
                 WHERE id = '{id}'",
                (now + Duration::seconds(60)).to_rfc3339()
            ),
        )
        .await
        .unwrap();
        poisoned.push((id, event_id));
    }
    let report = webhooks.publish_outbox_once(&transport, 500).await.unwrap();
    assert_eq!(report.dead_lettered, 0, "live leases are respected");
    x.w.h().clock.advance(Duration::seconds(61));
    let report = webhooks.publish_outbox_once(&transport, 500).await.unwrap();
    assert_eq!(report.dead_lettered, 2);
    for (id, event_id) in &poisoned {
        assert_eq!(transport.times(event_id), 0, "never published");
        assert_eq!(
            x.count(&format!(
                "SELECT count(*) FROM calling_outbox_events WHERE id = '{id}' \
                 AND status = 'DEAD_LETTER' AND last_error_code = 'LEASE_EXPIRED_AT_CAP'"
            ))
            .await,
            1
        );
    }
    // One open incident for the environment, counting both.
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM calling_incidents WHERE category = 'OUTBOX_DEAD_LETTER' \
             AND status = 'OPEN' AND environment_id = '{}' AND occurrences = 2",
            x.environment()
        ))
        .await,
        1
    );
    x.w.h().clock.advance(Duration::seconds(3600));
    webhooks.publish_outbox_once(&transport, 500).await.unwrap();
    for (_, event_id) in &poisoned {
        assert_eq!(
            transport.times(event_id),
            0,
            "a dead letter is not reclaimed"
        );
    }
}

// ---------------------------------------------------------------------------
// §82: the generic call-setup deadline.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_stuck_before_connect_times_out_once_and_its_paid_leg_is_released() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let deadline = AppCallingConfig::default().setup_deadline;
    let (call, leg, t) = dialing(&x, "setup-1").await;
    // Telephony never reports anything: just below the deadline nothing happens.
    x.w.h().clock.advance(deadline - Duration::seconds(5));
    assert_eq!(
        x.w.h().state.app.expire_stalled_setups(50).await.unwrap(),
        0
    );
    // A Call that made progress recently is kept, even if it is old.
    let (call2, leg2, t2) = dialing(&x, "setup-2").await;
    x.w.h().clock.advance(deadline - Duration::seconds(5));
    assert_eq!(
        event(&x, leg_event("LEG_RINGING", call2, leg2, t2, None, 1)).await,
        "APPLIED"
    );
    x.w.h().clock.advance(Duration::seconds(10));
    // Two sweepers race: exactly one ends the stuck Call.
    let (a, b) = (x.w.h().state.app.clone(), x.w.h().state.app.clone());
    let first = tokio::spawn(async move { a.expire_stalled_setups(50).await });
    let second = tokio::spawn(async move { b.expire_stalled_setups(50).await });
    let ended = first.await.unwrap().unwrap() + second.await.unwrap().unwrap();
    assert_eq!(ended, 1);
    assert_eq!(x.w.state(call).await.0, CallState::Failed);
    assert_eq!(x.w.state(call2).await.0, CallState::Ringing);
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM calls WHERE id = '{call}' AND failure_category = 'TIMEOUT' \
             AND terminal_reason IS NOT NULL"
        ))
        .await,
        1
    );
    // Exactly one state change to FAILED, and the paid leg's TERMINATE was committed with
    // it and sent once.
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM call_events WHERE call_id = '{call}' AND to_state = 'FAILED'"
        ))
        .await,
        1
    );
    let command = terminate_of(&x, leg).await.expect("TERMINATE written");
    x.w.h()
        .state
        .app
        .dispatch_telephony_commands(50)
        .await
        .unwrap();
    assert_eq!(terminates_for(&x, command.id), 1);
    assert!(x
        .telephony
        .controls()
        .iter()
        .any(|(leg, a, _)| *leg == t && *a == LegControl::Terminate));
    assert_eq!(
        x.w.h().state.app.expire_stalled_setups(50).await.unwrap(),
        0
    );
    assert!(
        calling::metrics::counter(
            "calling_call_outcomes_total",
            &[("call_kind", "VOICE"), ("outcome", "timeout")]
        ) >= 1
    );
}

#[tokio::test]
async fn the_setup_deadline_cancels_outstanding_ring_offers_of_a_stuck_application_call() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let (call, _, callee) = x.w.ringing_call("VOICE").await;
    assert!(x.w.invitation_for(call, callee.endpoint).await.is_some());
    // The expiry worker never ran (its instance was down); the deadline sweep is the net.
    x.w.h()
        .clock
        .advance(AppCallingConfig::default().setup_deadline + Duration::seconds(1));
    assert_eq!(
        x.w.h().state.app.expire_stalled_setups(50).await.unwrap(),
        1
    );
    assert_eq!(x.w.state(call).await.0, CallState::Failed);
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' \
             AND status IN ('PENDING', 'RINGING')"
        ))
        .await,
        0
    );
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND status = 'CANCELLED'"
        ))
        .await,
        1
    );
}

#[test]
fn the_setup_deadline_never_cuts_a_legitimate_queue_wait() {
    // The longest event-free legitimate wait is one queue's maximum wait (3600 s).
    let get = |v: &'static str| {
        move |k: &str| (k == "CALLING_CALL_SETUP_DEADLINE_SECONDS").then(|| v.to_string())
    };
    assert!(AppCallingConfig::from_lookup(&get("3600")).is_err());
    assert!(AppCallingConfig::from_lookup(&get("600")).is_err());
    assert_eq!(
        AppCallingConfig::from_lookup(&get("3900"))
            .unwrap()
            .setup_deadline,
        Duration::seconds(3900)
    );
    let defaults = AppCallingConfig::from_lookup(&|_: &str| None).unwrap();
    assert!(defaults.setup_deadline > Duration::seconds(3600 + 300));
    assert!(defaults.new_calls_enabled && defaults.app_calling_enabled);
    assert_eq!(defaults.telephony_command_max_attempts, 40);
    let off = |k: &str| {
        (k == "CALLING_NEW_CALLS_ENABLED" || k == "CALLING_APP_CALLING_ENABLED")
            .then(|| "false".to_string())
    };
    let config = AppCallingConfig::from_lookup(&off).unwrap();
    assert!(!config.new_calls_enabled && !config.app_calling_enabled);
    assert!(AppCallingConfig::from_lookup(
        &|k: &str| (k == "CALLING_NEW_CALLS_ENABLED").then(|| "maybe".to_string())
    )
    .is_err());
}

// ---------------------------------------------------------------------------
// §84 / §86: backlog gauges and the WORKER_BACKLOG incident.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_worker_backlog_is_sampled_and_an_old_backlog_raises_one_platform_incident() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let (call, _, _) = dialing(&x, "backlog-1").await;
    cancel_then_crash(&x, call).await;
    let app = &x.w.h().state.app;
    app.sample_worker_backlog().await.unwrap();
    assert!(calling::metrics::gauge("calling_telephony_commands_pending", &[]).unwrap() >= 1);
    assert!(calling::metrics::gauge("calling_outbox_pending", &[]).is_some());
    assert!(calling::metrics::gauge("calling_webhook_deliveries_pending", &[]).is_some());
    assert!(calling::metrics::gauge("calling_telephony_commands_dead", &[]).is_some());
    assert_eq!(
        x.count("SELECT count(*) FROM calling_incidents").await,
        0,
        "a young backlog is not an incident"
    );
    x.w.h().clock.advance(Duration::seconds(601));
    app.sample_worker_backlog().await.unwrap();
    app.sample_worker_backlog().await.unwrap();
    assert!(
        calling::metrics::gauge("calling_telephony_commands_oldest_pending_age_seconds", &[])
            .unwrap()
            >= 601
    );
    // Platform-wide (no tenant scope), one per queue, deduplicated while open.
    assert_eq!(
        x.count(
            "SELECT count(*) FROM calling_incidents WHERE category = 'WORKER_BACKLOG' \
             AND tenant_id IS NULL AND status = 'OPEN' AND occurrences = 2 \
             AND restricted_evidence ->> 'queue' = 'telephony_command'"
        )
        .await,
        1
    );
    // A scoped category can never be unscoped (DB CHECK).
    assert!(common::exec(
        x.w.h().conn(),
        "INSERT INTO calling_incidents (id, dedupe_key, category, severity, status, occurrences, \
           restricted_evidence, first_seen_at, last_seen_at) VALUES (gen_random_uuid(), \
           'stuck_queue_entry:x', 'STUCK_QUEUE_ENTRY', 'LOW', 'OPEN', 1, '{}', now(), now())"
    )
    .await
    .is_err());
}

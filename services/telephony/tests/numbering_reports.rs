//! Gap C10: Telephony is the voice carrier that reports to Numbering (ADR 0043 S, T).
//!
//! * A finalized, billable DCN leg queues ONE `number-usage` report (voice seconds of its
//!   Number Resource and assignment) in the usage transaction; the worker delivers it;
//!   repeated sweeps never resend it. Relay-number legs and unbillable legs report
//!   nothing; with the switch off nothing is queued.
//! * Never blocking: a Numbering outage leaves the call, its usage and Calling's events
//!   untouched — the report waits and is delivered after backoff. A permanent refusal
//!   dead-letters it with one deduplicated incident.
//! * DCN call patterns raise local abuse incidents and, when enabled, `SPAM_CALLS`
//!   signals for Numbering, one per DCN / window bucket / magnitude band.
//!
//! Numbering is the deterministic fake of Telephony's port; the HTTP adapter's paths and
//! permissions are Numbering's contract (`reportNumberUsage`, `reportNumberRiskSignal`).

mod common;

use axum::http::{Method, StatusCode};
use common::{FakeDcn, Harness};
use serde_json::{json, Value};
use telephony::{
    commands::originate::dcn_operation_ref, domain::NormalizedEvent,
    numbering_reports::NumberingReportSettings, ports::DcnScope, ports::PortError,
};
use uuid::Uuid;

const DCN: &str = "+2342010000888";

fn scope_of(h: &Harness) -> DcnScope {
    DcnScope {
        tenant_id: h.consumer.tenant_id,
        application_id: h.consumer.application_id,
        environment_id: h.consumer.environment_id,
    }
}

fn dcn_of(h: &Harness, id: Uuid, assignment: Uuid) -> FakeDcn {
    FakeDcn {
        number_resource_id: id,
        assignment_id: assignment,
        routing_revision: 3,
        scope: scope_of(h),
        target: None,
        unavailable: None,
        inventory_class: Some("REAL".into()),
    }
}

async fn bind_dcn(h: &Harness, number_resource_id: Uuid) {
    let now = h.clock_now();
    h.sql(&format!(
        "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, number_kind, \
           number_resource_id, created_at, updated_at) \
         VALUES ('{DCN}', 'ng-a', ARRAY['VOICE_OUTBOUND','VOICE_INBOUND'], 'ENABLED', 'DCN', \
           '{number_resource_id}', '{now}', '{now}')"
    ))
    .await;
}

async fn with_reports(usage: bool, risk: bool) -> Option<Harness> {
    let mut h = Harness::new().await?;
    h.svc = h
        .svc
        .clone()
        .with_numbering_reports(NumberingReportSettings {
            usage_reports_enabled: usage,
            risk_signals_enabled: risk,
            dcn_call_velocity: (3, 10),
            dcn_nuisance: (20, 60),
        });
    Some(h)
}

/// Originates an executed outbound leg presenting the DCN; returns the telephony leg.
async fn originate_dcn(h: &Harness, dcn: &FakeDcn) -> Uuid {
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    let op = dcn_operation_ref(
        body["call"]["callLegId"].as_str().unwrap().parse().unwrap(),
        body["commandId"].as_str().unwrap().parse().unwrap(),
    );
    let grant = h.numbering.issue_caller_identity(DCN, dcn.clone(), &op);
    body["presentedIdentity"] =
        json!({ "numberResourceId": dcn.number_resource_id, "grant": grant });
    let r = h
        .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let leg: Uuid = r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.run().await;
    leg
}

async fn apply(h: &Harness, leg: Uuid, event: NormalizedEvent) {
    let correlation = h.correlation_of(leg).await;
    let e = h.provider_a.progress(&correlation, event, h.now());
    h.svc.ingest_provider_event("dp-a", e).await.unwrap();
}

/// Answers the leg, keeps it connected for `seconds`, ends it and sweeps (the outbox
/// delivers one event per leg and sweep, in order).
async fn talk(h: &Harness, leg: Uuid, seconds: i64) {
    apply(h, leg, NormalizedEvent::LegAnswered).await;
    apply(h, leg, NormalizedEvent::LegConnected).await;
    h.advance(seconds);
    apply(h, leg, NormalizedEvent::LegEnded).await;
    for _ in 0..8 {
        h.run().await;
    }
}

async fn report_status(h: &Harness, leg: Uuid) -> Option<String> {
    h.text(&format!(
        "SELECT status || '/' || attempts FROM telephony_numbering_reports WHERE telephony_leg_id = '{leg}'"
    ))
    .await
}

fn usage_reports(h: &Harness) -> Vec<Value> {
    h.numbering.usage_reports.lock().unwrap().clone()
}

#[tokio::test]
async fn a_billable_dcn_leg_reports_its_voice_seconds_to_numbering_exactly_once() {
    let Some(h) = with_reports(true, false).await else {
        return;
    };
    let (id, assignment) = (Uuid::new_v4(), Uuid::new_v4());
    bind_dcn(&h, id).await;
    let leg = originate_dcn(&h, &dcn_of(&h, id, assignment)).await;
    talk(&h, leg, 65).await;
    assert_eq!(h.leg_status(leg).await, "ENDED");
    let billable = h
        .scalar(&format!(
            "SELECT billable_seconds::bigint FROM telephony_usage_records WHERE telephony_leg_id = '{leg}'"
        ))
        .await;
    assert!(billable >= 65, "{billable}");
    let reports = usage_reports(&h);
    assert_eq!(reports.len(), 1, "{reports:?}");
    let r = &reports[0];
    assert_eq!(r["sourceKey"], json!(format!("voice-leg:{leg}")));
    assert_eq!(r["numberResourceId"], json!(id));
    assert_eq!(r["assignmentId"], json!(assignment));
    assert_eq!(r["usageKind"], "VOICE_OUTBOUND_SECONDS");
    assert_eq!(r["quantity"], json!(billable));
    assert!(r["periodStart"].as_str().unwrap() < r["periodEnd"].as_str().unwrap());
    // An estimate is never sent as provider cost evidence.
    assert!(r.get("providerCostMinor").is_none() && r.get("currency").is_none());
    // CF Meet ids, counts and timestamps only.
    let text = r.to_string();
    for forbidden in ["+234", "dp-a", "ng-a", "grant"] {
        assert!(!text.contains(forbidden), "{forbidden} leaked: {text}");
    }
    assert_eq!(report_status(&h, leg).await.as_deref(), Some("DELIVERED/1"));
    // Calling still received the usage fact; more sweeps never resend the report.
    assert!(h
        .calling
        .event_types_for(leg)
        .contains(&"USAGE_FINALIZED".to_string()));
    h.advance(600);
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(usage_reports(&h).len(), 1);
}

#[tokio::test]
async fn relay_unbillable_and_switched_off_legs_report_nothing() {
    // Switch off: a billable DCN leg queues nothing.
    let Some(off) = with_reports(false, false).await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&off, id).await;
    let leg = originate_dcn(&off, &dcn_of(&off, id, Uuid::new_v4())).await;
    talk(&off, leg, 30).await;
    assert_eq!(off.leg_status(leg).await, "ENDED");
    assert_eq!(
        off.scalar("SELECT count(*) FROM telephony_numbering_reports")
            .await,
        0
    );
    assert!(usage_reports(&off).is_empty());

    // Switch on: a relay-number leg (no DCN) and an unanswered DCN leg queue nothing.
    let Some(h) = with_reports(true, true).await else {
        return;
    };
    let (relay_leg, _) = h.originate().await;
    h.run().await;
    talk(&h, relay_leg, 30).await;
    assert_eq!(h.leg_status(relay_leg).await, "ENDED");
    let id = Uuid::new_v4();
    bind_dcn(&h, id).await;
    let unanswered = originate_dcn(&h, &dcn_of(&h, id, Uuid::new_v4())).await;
    apply(&h, unanswered, NormalizedEvent::LegNoAnswer).await;
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_numbering_reports WHERE report_kind = 'USAGE'")
            .await,
        0
    );
    assert!(usage_reports(&h).is_empty());
}

#[tokio::test]
async fn a_numbering_outage_delays_the_report_but_never_the_call() {
    let Some(h) = with_reports(true, false).await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&h, id).await;
    let leg = originate_dcn(&h, &dcn_of(&h, id, Uuid::new_v4())).await;
    h.numbering.set_available(false);
    talk(&h, leg, 20).await;
    // The call ended, its usage was finalized and Calling heard about it.
    assert_eq!(h.leg_status(leg).await, "ENDED");
    assert!(h
        .calling
        .event_types_for(leg)
        .contains(&"USAGE_FINALIZED".to_string()));
    let pending = report_status(&h, leg).await.unwrap();
    assert!(pending.starts_with("PENDING/"), "{pending}");
    assert!(usage_reports(&h).is_empty());
    // Numbering returns: the report is delivered after its backoff.
    h.numbering.set_available(true);
    h.advance(400);
    h.run().await;
    assert_eq!(usage_reports(&h).len(), 1);
    assert!(report_status(&h, leg)
        .await
        .unwrap()
        .starts_with("DELIVERED/"));
}

#[tokio::test]
async fn a_permanent_refusal_dead_letters_the_report_with_one_incident() {
    let Some(h) = with_reports(true, false).await else {
        return;
    };
    h.numbering
        .refuse_reports(Some(PortError::Denied("USAGE_SOURCE_CONFLICT".into())));
    let now = h.clock_now();
    for _ in 0..2 {
        let (leg, nr) = (Uuid::new_v4(), Uuid::new_v4());
        h.sql(&format!(
            "INSERT INTO telephony_numbering_reports (id, report_kind, source_key, number_resource_id, \
               telephony_leg_id, payload, status, attempts, next_attempt_at, created_at) \
             VALUES ('{}', 'USAGE', 'voice-leg:{leg}', '{nr}', '{leg}', '{{}}', 'PENDING', 0, '{now}', '{now}')",
            Uuid::new_v4()
        ))
        .await;
    }
    let out = h.svc.deliver_numbering_reports_once(10).await.unwrap();
    assert_eq!(out.dead_lettered, 2);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_numbering_reports WHERE status = 'DEAD_LETTER' \
             AND last_error = 'USAGE_SOURCE_CONFLICT'"
        )
        .await,
        2
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'OUTBOX_DEAD_LETTER' \
             AND dedupe_key = 'numbering-report-dead-letter:USAGE:NUMBERING_REFUSED'"
        )
        .await,
        1,
        "one deduplicated incident per reason"
    );
    // Dead letters are final.
    h.numbering.refuse_reports(None);
    h.advance(3600);
    assert_eq!(
        h.svc
            .deliver_numbering_reports_once(10)
            .await
            .unwrap()
            .delivered,
        0
    );
}

#[tokio::test]
async fn a_dcn_calling_too_fast_is_signalled_to_numbering_as_spam_calls() {
    let Some(h) = with_reports(false, true).await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&h, id).await;
    let assignment = Uuid::new_v4();
    for _ in 0..3 {
        originate_dcn(&h, &dcn_of(&h, id, assignment)).await;
    }
    // The abuse scan runs at most once a minute per instance.
    h.advance(61);
    h.run().await;
    let signals = h.numbering.risk_signals.lock().unwrap().clone();
    assert_eq!(signals.len(), 1, "{signals:?}");
    assert_eq!(signals[0]["numberResourceId"], json!(id));
    assert_eq!(signals[0]["signalKind"], "SPAM_CALLS");
    assert_eq!(signals[0]["magnitude"], 50, "at the threshold");
    assert!(signals[0]["sourceKey"]
        .as_str()
        .unwrap()
        .starts_with("dcn-spam-calls:"));
    assert!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'ABUSE_SIGNAL' \
             AND restricted_evidence->>'kind' = 'DCN_CALL_VELOCITY' \
             AND restricted_evidence->>'subject' = '{id}'"
        ))
        .await
            >= 1
    );
    // The same pattern in the same window bucket is not signalled again.
    let bucket = |t: chrono::DateTime<chrono::Utc>| t.timestamp() / 600;
    let before = bucket(h.now());
    h.advance(61);
    h.run().await;
    let expected = if bucket(h.now()) == before { 1 } else { 2 };
    assert_eq!(h.numbering.risk_signals.lock().unwrap().len(), expected);
    // No usage report is queued while usage reporting is off.
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_numbering_reports WHERE report_kind = 'USAGE'")
            .await,
        0
    );
}

#[tokio::test]
async fn with_risk_signals_off_the_pattern_stays_a_local_incident() {
    let Some(h) = with_reports(false, false).await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&h, id).await;
    for _ in 0..3 {
        originate_dcn(&h, &dcn_of(&h, id, Uuid::new_v4())).await;
    }
    h.advance(61);
    h.run().await;
    assert!(h.numbering.risk_signals.lock().unwrap().is_empty());
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_numbering_reports")
            .await,
        0
    );
    assert!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'ABUSE_SIGNAL' \
             AND restricted_evidence->>'kind' = 'DCN_CALL_VELOCITY'"
        )
        .await
            >= 1
    );
}

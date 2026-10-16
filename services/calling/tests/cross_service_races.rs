//! Part K §106: cross-service races, reproduced at exact points with deterministic
//! barriers (tests/common/gates.rs, row locks observed in `pg_stat_activity`) and repeated
//! for many rounds:
//!
//! 1. Privacy Relay revokes the communication context while a public (PSTN) leg is being
//!    originated — at three points: the Contact Resolution Grant was just issued, the
//!    originate command is on its way to Telephony, Telephony already accepted it. No paid
//!    command is ever ISSUED after the revocation, and the only possible outcome of a
//!    command issued before it is the controlled one: the leg is terminated, the Call is
//!    terminal, nothing is left pending.
//! 2. A context participant is reassigned while the former participant's device is
//!    ringing: whatever order the reassignment and the former's answer arrive in, the
//!    former ends up without authority (no answer after enforcement, no media
//!    credential, the Call is over) and the replacement only holds authority Privacy
//!    Relay newly granted (a new Call, authorized at the new context version).
//! 3. Usage finalization delivered concurrently, after a simulated crash mid-application
//!    (the worker died holding a partial transaction): exactly one usage fact, one
//!    settlement, one recorded event.
//!
//! Related races certified elsewhere (not duplicated here): single-winner answers and
//! accept-vs-cancel (app_races.rs), transfer ownership races (enterprise_transfer.rs),
//! identical concurrent callback requests (public_callback_relay.rs), concurrent spend
//! reservations (public_spend.rs), duplicate / concurrent enforcement replays
//! (privacy_enforcement.rs).

mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use calling::{calls::CallState, telephony::LegControl};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
    ServiceTokenSigner,
};
use common::{
    app::{AppWorld, Device},
    deployment::Deployment,
    gates::OriginatePoint,
    privacy::{FakePrivacyRelay, Mode},
    scalar_i64,
    telephony::leg_event,
    TestResponse,
};
use futures_util::future::join_all;
use http_body_util::BodyExt;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const ROUNDS: usize = 12;

fn delegated(w: &AppWorld) -> DelegatedPlatformContext {
    let s = w.w.scope();
    DelegatedPlatformContext::new(
        s.tenant_id().as_uuid(),
        s.application_id().as_uuid(),
        s.environment_id().as_uuid(),
        ApplicationEnvironmentKind::Production,
        w.w.consumer.sa(w.w.kind).as_uuid(),
    )
}

fn signed_headers(
    signer: &ServiceTokenSigner,
    delegated: Option<DelegatedPlatformContext>,
) -> Vec<(String, String)> {
    let mut outbound = OutboundServiceRequest::new_operation(ServiceId::calling());
    if let Some(d) = delegated {
        outbound = outbound.with_delegated_context(d);
    }
    signer
        .sign(&outbound)
        .unwrap()
        .headers()
        .iter()
        .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

/// One POST through a router (owned: usable from a spawned task).
async fn post_raw(
    router: Router,
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/json");
    for (k, v) in &headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    let response = router
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Privacy Relay's enforcement command to Calling (signed, delegated scope).
async fn enforce(w: &AppWorld, body: Value) -> (StatusCode, Value) {
    let headers = signed_headers(&w.h().peers.privacy_relay, Some(delegated(w)));
    post_raw(
        w.h().router(),
        "/internal/v1/privacy-relay/enforcement".into(),
        headers,
        body,
    )
    .await
}

fn enforcement(context: Uuid, version: i64, reason: &str, affected: &[Uuid]) -> Value {
    json!({
        "enforcementEventId": Uuid::new_v4(),
        "contextId": context,
        "contextVersion": version,
        "relaySessionIds": [],
        "reason": reason,
        "affectedPrincipalIds": affected,
        "terminateActiveCalls": true,
    })
}

async fn strings(w: &AppWorld, sql: &str) -> Vec<String> {
    common::enterprise::strings(w.h().conn(), sql).await
}

/// Polls (condition-based, never a fixed delay) until a backend of this database waits
/// on a lock while running a statement that mentions `table`.
async fn wait_for_lock_wait(w: &AppWorld, table: &str) {
    for _ in 0..10_000 {
        let waiting = scalar_i64(
            w.h().conn(),
            &format!(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
                   AND wait_event_type = 'Lock' AND query LIKE '%{table}%'"
            ),
        )
        .await;
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("no backend ever waited on {table}");
}

// ---------------------------------------------------------------------------------------
// 1. Revocation during public origination.
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum RevokeAt {
    /// Privacy Relay issued the grant; Calling has not received it yet.
    GrantIssued,
    /// Calling issued the originate; Telephony has not received it yet.
    OriginateInFlight,
    /// Telephony accepted the originate (a paid leg exists); the answer is not back yet.
    OriginateAccepted,
}

/// Drives due durable Telephony commands until none of `call` is pending.
async fn drain_commands(d: &Deployment, call: Uuid) {
    for _ in 0..40 {
        let pending = scalar_i64(
            d.h().conn(),
            &format!(
                "SELECT count(*) FROM call_telephony_commands WHERE call_id = '{call}' AND status = 'PENDING'"
            ),
        )
        .await;
        if pending == 0 {
            return;
        }
        d.h().clock.advance(chrono::Duration::seconds(31));
        d.h()
            .state
            .app
            .dispatch_telephony_commands(50)
            .await
            .unwrap();
    }
    panic!("telephony commands of {call} never drained");
}

/// The command was already on its way to (or accepted by) Telephony when the context
/// was revoked: the only outcome is the controlled one — the executed leg is terminated
/// exactly once, the Call is terminal, nothing stays pending, and nothing further is
/// originated.
#[tokio::test]
async fn context_revoked_while_the_originate_is_in_flight_ends_in_the_controlled_outcome() {
    revocation_rounds(&[RevokeAt::OriginateInFlight, RevokeAt::OriginateAccepted]).await;
}

/// Privacy Relay issued the Contact Resolution Grant and then revoked the context before
/// Calling sent the originate: Calling must not send it (no paid command after the
/// revocation) — or, at the very least, must terminate what it sent.
#[tokio::test]
async fn context_revoked_after_the_grant_was_issued_issues_no_paid_command() {
    revocation_rounds(&[RevokeAt::GrantIssued]).await;
}

async fn revocation_rounds(points: &[RevokeAt]) {
    let Some(d) = Deployment::new().await else {
        return;
    };
    let a = d.a();
    let mut version = 1i64;
    let mut violations: Vec<String> = Vec::new();
    for round in 0..ROUNDS {
        let point = points[round % points.len()];
        let round_start = d.x.timeline.record(format!("round {round}"));
        // A fresh grant of authority at a new context version (the previous round's
        // enforcement superseded the old one).
        d.x.privacy.set_mode(Mode::Policy);
        d.x.privacy.set_versions(version, 1);
        let gate = match point {
            RevokeAt::GrantIssued => d.x.gated_privacy.arm_grant(),
            RevokeAt::OriginateInFlight => d.x.gated_telephony.arm(OriginatePoint::BeforeTelephony),
            RevokeAt::OriginateAccepted => d.x.gated_telephony.arm(OriginatePoint::AfterAccepted),
        };
        let revoked_at = std::sync::Mutex::new(0u64);
        let key = format!("revoke-race-{round}");
        let create = d.call_contact(&key);
        let revoke = async {
            gate.wait_entered().await;
            // Privacy Relay revokes the context: no further authorization or grant...
            d.x.privacy.set_mode(Mode::Deny("CONTEXT_REVOKED"));
            *revoked_at.lock().unwrap() = d.x.timeline.record("revoked");
            // ...and tells Calling.
            let (status, body) = enforce(
                a,
                enforcement(d.x.privacy.context_id, version + 1, "CONTEXT_REVOKED", &[]),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            gate.open();
        };
        let (created, ()) = tokio::join!(create, revoke);
        let marker = *revoked_at.lock().unwrap();
        version += 2;
        // The create request either answered with the Call or was refused safely.
        let call: Uuid = match created.json["call"]["callId"].as_str() {
            Some(id) => id.parse().unwrap(),
            None => panic!("{point:?}: {}", created.text),
        };
        drain_commands(&d, call).await;
        let (leg, _) = d.public_leg(call).await;

        // No paid command issued after the revocation.
        let late = d.x.timeline.after(marker, "originate:");
        if !late.is_empty() {
            violations.push(format!(
                "round {round} ({point:?}): originate issued after the revocation: {late:?}; round timeline {:?}",
                d.x.timeline.after(round_start, "")
            ));
        }
        // At most the controlled outcome: a leg Telephony executed is terminated.
        let executed: Vec<Uuid> =
            d.x.telephony
                .executions()
                .iter()
                .filter(|(_, _, v)| v["call"]["callLegId"] == json!(leg))
                .map(|(_, t, _)| *t)
                .collect();
        assert!(executed.len() <= 1, "{point:?}: {executed:?}");
        if let Some(t) = executed.first() {
            let terminations =
                d.x.telephony
                    .controls()
                    .iter()
                    .filter(|(leg, action, _)| leg == t && *action == LegControl::Terminate)
                    .count();
            if terminations != 1 {
                violations.push(format!(
                    "round {round} ({point:?}): the executed leg {t} was terminated {terminations} times (expected exactly once)"
                ));
            }
            // Telephony confirms the terminated execution.
            let attempt = strings(
                a,
                &format!("SELECT id::text FROM call_attempts WHERE leg_id = '{leg}' ORDER BY attempt_number DESC LIMIT 1"),
            )
            .await
            .pop()
            .map(|s| s.parse::<Uuid>().unwrap());
            let mut ended = leg_event("LEG_ENDED", call, leg, *t, attempt, 1);
            ended["endReason"] = json!("CANCELLED");
            ended["legStatus"] = json!("ENDED");
            let event: calling::telephony::events::TelephonyEvent =
                serde_json::from_value(ended).unwrap();
            d.h().state.app.apply_telephony_event(&event).await.unwrap();
        } else {
            assert_eq!(
                scalar_i64(
                    d.h().conn(),
                    &format!(
                        "SELECT count(*) FROM call_spend_reservations WHERE call_id = '{call}' \
                           AND status = 'RESERVED'"
                    )
                )
                .await,
                0,
                "{point:?}: nothing executed, nothing stays reserved"
            );
        }
        let (state, _) = a.state(call).await;
        assert!(state.is_terminal(), "{point:?}: {state}");
        let leg_state = strings(
            a,
            &format!("SELECT state FROM call_legs WHERE id = '{leg}'"),
        )
        .await;
        assert!(
            ["CANCELLED", "ENDED", "FAILED"].contains(&leg_state[0].as_str()),
            "{point:?}: leg {leg_state:?}"
        );
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

// ---------------------------------------------------------------------------------------
// 2. Participant reassignment while the former participant's device rings.
// ---------------------------------------------------------------------------------------

async fn signaling(a: &AppWorld, device: &Device) -> String {
    a.signaling_token(device).await
}

#[tokio::test]
async fn reassignment_while_the_former_participant_rings_leaves_no_authority_behind() {
    let fake = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let Some(a) = AppWorld::with_privacy_relay(fake.clone()).await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "caller-main").await;
    let session = Uuid::new_v4();
    let mut version = 1i64;
    for round in 0..ROUNDS {
        let former = a.principal(&format!("agent-{round}")).await;
        let replacement = a.principal(&format!("agent-{round}-next")).await;
        let former_phone = a.device(&former, &format!("agent-{round}-phone")).await;
        let replacement_phone = a
            .device(&replacement, &format!("agent-{round}-next-phone"))
            .await;
        // The context's participants: the initiator and the former.
        *fake.allowed.lock().unwrap() = vec![a.w.initiator.id().as_uuid(), former.id().as_uuid()];
        fake.set_mode(Mode::Policy);
        fake.set_versions(version, 1);
        let r = a
            .create_protected_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&former],
                session,
                &format!("ring-{round}"),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let invitation = a.invitation_for(call, former_phone.endpoint).await.unwrap();
        let former_token = signaling(&a, &former_phone).await;

        // Privacy Relay reassigns: the replacement takes the former's place at a new
        // context version.
        *fake.allowed.lock().unwrap() =
            vec![a.w.initiator.id().as_uuid(), replacement.id().as_uuid()];
        fake.set_versions(version + 1, 1);
        let command = enforcement(
            fake.context_id,
            version + 1,
            "PARTICIPANT_REMOVED",
            &[former.id().as_uuid()],
        );
        let accept_path = format!("/v1/calling/invitations/{invitation}/accept");
        let (_, v) = a.state(call).await;
        // The former's device answers with the Call version it last saw.
        let answer = |version: i64| {
            a.as_endpoint(
                &former_token,
                Method::POST,
                &accept_path,
                Some(json!({ "expectedVersion": version })),
                &[],
            )
        };
        // Orders: reassignment first (the device then answers with the CURRENT version,
        // so only authority can refuse it), answer first, then truly concurrent.
        let (accepted, enforced): (TestResponse, (StatusCode, Value)) = match round % 3 {
            0 => {
                let e = enforce(&a, command).await;
                let (_, current) = a.state(call).await;
                (answer(current).await, e)
            }
            1 => {
                let r = answer(v).await;
                (r, enforce(&a, command).await)
            }
            _ => {
                let barrier = tokio::sync::Barrier::new(2);
                tokio::join!(
                    async {
                        barrier.wait().await;
                        answer(v).await
                    },
                    async {
                        barrier.wait().await;
                        enforce(&a, command).await
                    }
                )
            }
        };
        assert_eq!(enforced.0, StatusCode::OK, "{}", enforced.1);
        let answered_first = accepted.status == StatusCode::OK;
        if round % 3 == 0 {
            assert!(
                !answered_first,
                "after the reassignment the former cannot answer: {}",
                accepted.text
            );
        }
        // Provider cleanup of an answered Call (ENDING -> ENDED).
        a.h().clock.advance(chrono::Duration::seconds(30));
        a.h().state.app.reconcile_due(100).await.unwrap();
        let (state, _) = a.state(call).await;
        assert!(
            state.is_terminal(),
            "round {round}: the Call of the former is over ({state}; answered first: {answered_first})"
        );
        if !answered_first {
            assert_eq!(state, CallState::Cancelled);
        }
        // The former holds no authority now: no (re-)answer, no media credential. (A
        // former who answered BEFORE the reassignment gets the idempotent replay of that
        // answer — it changes nothing.)
        let (_, current) = a.state(call).await;
        let sessions_before = scalar_i64(
            a.h().conn(),
            &format!("SELECT count(*) FROM call_sessions WHERE call_id = '{call}'"),
        )
        .await;
        let again = answer(current).await;
        if answered_first {
            assert_eq!(a.state(call).await, (state, current), "{}", again.text);
            assert_eq!(
                scalar_i64(
                    a.h().conn(),
                    &format!("SELECT count(*) FROM call_sessions WHERE call_id = '{call}'"),
                )
                .await,
                sessions_before
            );
        } else {
            assert!(!again.status.is_success(), "{}", again.text);
        }
        let sessions = strings(
            &a,
            &format!(
                "SELECT s.id::text FROM call_sessions s JOIN call_participants p ON p.id = s.participant_id \
                  WHERE s.call_id = '{call}' AND p.principal_id = '{}'",
                former.id()
            ),
        )
        .await;
        for s in sessions {
            let r = a
                .as_endpoint(
                    &former_token,
                    Method::POST,
                    &format!("/v1/calling/sessions/{s}/media-credentials"),
                    Some(json!({})),
                    &[],
                )
                .await;
            assert!(
                !r.status.is_success(),
                "former got media authority: {}",
                r.text
            );
        }
        assert_eq!(
            scalar_i64(
                a.h().conn(),
                &format!(
                    "SELECT count(*) FROM call_participants WHERE call_id = '{call}' \
                       AND principal_id = '{}' AND ended_at IS NULL",
                    former.id()
                )
            )
            .await,
            0,
            "no live participation of the former"
        );
        // The replacement never inherits the former's invitation or Call...
        let replacement_token = signaling(&a, &replacement_phone).await;
        let r = a
            .as_endpoint(
                &replacement_token,
                Method::POST,
                &accept_path,
                Some(json!({ "expectedVersion": current })),
                &[],
            )
            .await;
        assert!(!r.status.is_success(), "{}", r.text);
        assert_eq!(
            scalar_i64(
                a.h().conn(),
                &format!(
                    "SELECT count(*) FROM call_participants WHERE call_id = '{call}' \
                       AND principal_id = '{}'",
                    replacement.id()
                )
            )
            .await,
            0
        );
        // ...and holds exactly the authority newly granted: a new Call, authorized at the
        // new context version. The former can no longer be called under the context.
        let r = a
            .create_protected_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&replacement],
                session,
                &format!("ring-{round}-next"),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let next: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            strings(
                &a,
                &format!(
                    "SELECT context_version::text FROM call_privacy_authorizations WHERE call_id = '{next}'"
                )
            )
            .await,
            vec![(version + 1).to_string()]
        );
        let calls_before = scalar_i64(a.h().conn(), "SELECT count(*) FROM calls").await;
        let r = a
            .create_protected_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&former],
                session,
                &format!("ring-{round}-former"),
            )
            .await;
        assert!(!r.status.is_success(), "{}", r.text);
        assert_eq!(
            scalar_i64(a.h().conn(), "SELECT count(*) FROM calls").await,
            calls_before
        );
        let (_, v) = a.state(next).await;
        a.command(next, "cancel", v).await;
        version += 2;
    }
}

// ---------------------------------------------------------------------------------------
// 3. Usage finalization: concurrent, and retried after a crash mid-application.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn usage_finalization_after_a_crash_and_under_concurrency_records_one_fact() {
    let Some(d) = Deployment::new().await else {
        return;
    };
    let a = d.a();
    let h = d.h();
    let apply = |v: Value| async move {
        let event: calling::telephony::events::TelephonyEvent = serde_json::from_value(v).unwrap();
        h.state.app.apply_telephony_event(&event).await.unwrap()
    };
    for round in 0..ROUNDS {
        let r = d.call_contact(&format!("usage-race-{round}")).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let (leg, attempt) = d.public_leg(call).await;
        let t = d.x.telephony.telephony_leg_of(leg).expect("originated");
        apply(leg_event("LEG_RINGING", call, leg, t, Some(attempt), 1)).await;
        apply(leg_event("LEG_ANSWERED", call, leg, t, Some(attempt), 2)).await;
        let mut ended = leg_event("LEG_ENDED", call, leg, t, Some(attempt), 3);
        ended["endReason"] = json!("COMPLETED");
        ended["legStatus"] = json!("ENDED");
        apply(ended).await;
        let cost = 1000 + round as i64;
        let mut usage = leg_event("USAGE_FINALIZED", call, leg, t, Some(attempt), 4);
        usage["usage"] = json!({ "connectedDurationMs": 42_000, "billableSeconds": 42,
                                 "costMinor": cost, "costKnown": true, "currency": "NGN",
                                 "destinationCountry": "NG" });

        // Crash: the first delivery dies holding a partial transaction (Call locked,
        // dedupe checked, blocked in the settlement) — it is aborted, never committed.
        let blocker = d.h().conn().begin().await.unwrap();
        blocker
            .execute_unprepared(&format!(
                "SELECT 1 FROM call_spend_reservations WHERE call_leg_id = '{leg}' FOR UPDATE"
            ))
            .await
            .unwrap();
        let headers = signed_headers(&d.h().peers.telephony, None);
        let crashed = tokio::spawn(post_raw(
            d.h().router(),
            "/internal/v1/telephony/events".into(),
            headers,
            usage.clone(),
        ));
        wait_for_lock_wait(a, "call_spend_reservations").await;
        crashed.abort();
        assert!(crashed.await.unwrap_err().is_cancelled());
        blocker.rollback().await.unwrap();
        // Nothing of the crashed delivery survived.
        assert_eq!(
            scalar_i64(
                d.h().conn(),
                &format!(
                    "SELECT count(*) FROM call_usage_records WHERE call_id = '{call}' AND leg_id = '{leg}'"
                )
            )
            .await,
            0,
            "a crashed application leaves no partial fact"
        );

        // Telephony retries: the same event several times, concurrently with a
        // re-emitted copy (new event id, later sequence).
        let mut reemitted = usage.clone();
        reemitted["eventId"] = json!(Uuid::new_v4());
        reemitted["sequence"] = json!(5);
        let deliveries = (0..6).map(|i| {
            let body = if i % 3 == 2 {
                reemitted.clone()
            } else {
                usage.clone()
            };
            let headers = signed_headers(&d.h().peers.telephony, None);
            post_raw(
                d.h().router(),
                "/internal/v1/telephony/events".into(),
                headers,
                body,
            )
        });
        let results = join_all(deliveries).await;
        assert!(
            results.iter().all(|(s, _)| *s == StatusCode::OK),
            "{results:?}"
        );
        // Exactly one usage fact, one settlement, one record of each event id.
        assert_eq!(
            strings(
                a,
                &format!(
                    "SELECT usage_type || ':' || quantity || ':' || (finalized_at IS NOT NULL) \
                       FROM call_usage_records WHERE call_id = '{call}' AND leg_id = '{leg}'"
                )
            )
            .await,
            vec!["LEG_CONNECTED_TIME:42000:true".to_string()],
            "round {round}"
        );
        assert_eq!(
            strings(
                a,
                &format!(
                    "SELECT status || ':' || settled_minor FROM call_spend_reservations WHERE call_leg_id = '{leg}'"
                )
            )
            .await,
            vec![format!("SETTLED:{cost}")]
        );
        for id in [&usage["eventId"], &reemitted["eventId"]] {
            assert_eq!(
                scalar_i64(
                    d.h().conn(),
                    &format!(
                        "SELECT count(*) FROM call_telephony_events WHERE event_id = '{}'",
                        id.as_str().unwrap()
                    )
                )
                .await,
                1
            );
        }
        assert_eq!(
            scalar_i64(
                d.h().conn(),
                &format!(
                    "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                       AND event_type = 'CALL_USAGE_FINALIZED' AND payload::text LIKE '%LEG_CONNECTED_TIME%'"
                )
            )
            .await,
            1
        );
    }
}

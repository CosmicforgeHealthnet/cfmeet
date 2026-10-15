//! §85 trace correlation: one public app → phone call through the scripted Privacy Relay
//! and Telephony, from the customer request to usage. Every step is linked by the call id
//! and the creating request's correlation id — in the database (call events of every
//! step, the Telephony commands sent, the applied Telephony events, the usage facts) and
//! in the structured logs (captured JSON) — using only safe identifiers: no telephone
//! number appears anywhere.

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::StatusCode;
use calling::{
    calls::CallState, media::provider::ProviderParticipant, telephony::events::TelephonyEvent,
};
use common::{app::AppWorld, public::PublicWorld, telephony::leg_event};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn lines(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
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

async fn one<T: sea_orm::TryGetable>(x: &PublicWorld, sql: &str) -> T {
    x.w.h()
        .conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap()
}

#[tokio::test]
async fn one_public_call_is_linked_end_to_end_by_call_id_and_request_correlation() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(capture.clone()),
    );
    let _guard = tracing::subscriber::set_default(subscriber);

    let Some(x) = PublicWorld::new().await else {
        return;
    };
    // 1. The customer request.
    let r = x.call_to(x.contact, "trace-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // The request's correlation id, as the request span logged it.
    let request_id = r
        .headers
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let correlation = capture
        .lines()
        .into_iter()
        .find_map(|l| {
            let spans = l["spans"].as_array()?;
            spans.iter().find_map(|s| {
                (s["request_id"] == request_id.as_str())
                    .then(|| s["correlation_id"].as_str().map(str::to_string))
                    .flatten()
            })
        })
        .expect("the request span carries its correlation id");
    assert!(correlation.starts_with("corr_"));
    // 2. The Call records it.
    let stored: String = one(
        &x,
        &format!("SELECT correlation_id FROM calls WHERE id = '{call}'"),
    )
    .await;
    assert_eq!(stored, correlation);

    // 3. Route, leg, telephony command.
    let leg: Uuid = one(
        &x,
        &format!(
            "SELECT id FROM call_legs WHERE call_id = '{call}' AND leg_kind = 'PUBLIC_NETWORK'"
        ),
    )
    .await;
    let attempt: Uuid = one(
        &x,
        &format!("SELECT id FROM call_attempts WHERE leg_id = '{leg}'"),
    )
    .await;
    let t = x.telephony.telephony_leg_of(leg).unwrap();
    let (_, originate) = x.telephony.originates().pop().unwrap();
    assert_eq!(originate["correlationId"], correlation.as_str());
    assert_eq!(originate["call"]["callId"], json!(call));

    // 4. Telephony events drive the Call to CONNECTED.
    assert_eq!(
        event(&x, leg_event("LEG_RINGING", call, leg, t, Some(attempt), 1)).await,
        "APPLIED"
    );
    assert_eq!(
        event(
            &x,
            leg_event("LEG_ANSWERED", call, leg, t, Some(attempt), 2)
        )
        .await,
        "APPLIED"
    );
    let bridge = x.telephony.bridges().pop().expect("a bridge was requested");
    assert_eq!(bridge["correlationId"], correlation.as_str());
    let attached = x.media.attached.lock().unwrap().clone();
    let identity = attached[0].1.clone();
    let p = ProviderParticipant {
        identity: identity.clone(),
        generation: None,
        permissions: calling::media::MediaPermissions::NONE,
        active: true,
        published_audio: true,
        published_video: false,
        joined_at: Some(chrono::Utc::now()),
    };
    x.w.media().inject(&AppWorld::room(call), p.clone());
    let (auth, body) =
        x.w.media()
            .signed_event("participant_joined", &AppWorld::room(call), Some(&p));
    assert_eq!(x.w.callback(&auth, body).await.status, StatusCode::OK);
    assert_eq!(
        event(
            &x,
            leg_event("LEG_CONNECTED", call, leg, t, Some(attempt), 3)
        )
        .await,
        "APPLIED"
    );
    let bridge_id: Uuid = one(
        &x,
        &format!("SELECT telephony_bridge_id FROM call_public_bridges WHERE call_id = '{call}'"),
    )
    .await;
    let mut established = leg_event("BRIDGE_ESTABLISHED", call, leg, t, None, 1);
    established.as_object_mut().unwrap().remove("callLegId");
    established["bridgeId"] = json!(bridge_id);
    assert_eq!(event(&x, established).await, "APPLIED");
    let session = x.w.session_on(call, x.caller.endpoint).await.unwrap();
    x.w.connect(call, session).await;
    assert_eq!(x.w.state(call).await.0, CallState::Connected);

    // 5. Termination (the phone hangs up) and usage.
    x.w.h().clock.advance(chrono::Duration::seconds(30));
    let mut ended = leg_event("LEG_ENDED", call, leg, t, Some(attempt), 4);
    ended["endReason"] = json!("COMPLETED");
    ended["legStatus"] = json!("ENDED");
    assert_eq!(event(&x, ended).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Ended);
    let mut usage = leg_event("USAGE_FINALIZED", call, leg, t, Some(attempt), 5);
    usage["usage"] = json!({ "connectedDurationMs": 30_000, "billableSeconds": 30, "costMinor": 100,
                             "costKnown": true, "currency": "NGN", "destinationCountry": "NG" });
    assert_eq!(event(&x, usage).await, "APPLIED");

    // --- Database: every step is linked by call id + the request's correlation id. ---
    // The creating request's own events, and EVERY event Calling wrote by itself for the
    // Call afterwards (Telephony events, media evidence, termination, usage), carry the
    // creating request's correlation id. (A later customer request on the Call — e.g. a
    // media credential — is its own operation with its own correlation id.)
    let events: Vec<(String, String, String, String)> =
        x.w.h()
            .conn()
            .query_all(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT event_type, correlation_id, actor_kind, request_id FROM call_events \
                 WHERE call_id = '{call}' ORDER BY sequence"
                ),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|r| {
                (
                    r.try_get_by_index(0).unwrap(),
                    r.try_get_by_index(1).unwrap(),
                    r.try_get_by_index(2).unwrap(),
                    r.try_get_by_index(3).unwrap(),
                )
            })
            .collect();
    for required in [
        "CALL_REQUESTED",
        "CALL_ROUTE_RECORDED",
        "CALL_LEG_RECORDED",
        "CALL_LEG_STATE_CHANGED",
        "CALL_STATE_CHANGED",
        "CALL_USAGE_FINALIZED",
    ] {
        assert!(
            events
                .iter()
                .any(|(e, c, ..)| e == required && *c == correlation),
            "{required}: {events:?}"
        );
    }
    let foreign: Vec<_> = events
        .iter()
        .filter(|(_, c, actor, request)| {
            (actor == "INTERNAL_SERVICE" || *request == request_id) && *c != correlation
        })
        .collect();
    assert!(
        foreign.is_empty(),
        "every step of the Call carries its correlation: {foreign:?}"
    );
    assert!(
        events
            .iter()
            .filter(|(_, _, actor, _)| actor == "INTERNAL_SERVICE")
            .count()
            >= 10,
        "{events:?}"
    );
    // Commands sent to Telephony, the applied Telephony events and the usage facts.
    let commands: i64 = one(
        &x,
        &format!(
            "SELECT count(*) FROM call_telephony_commands WHERE call_id = '{call}' \
             AND payload ->> 'correlationId' <> '{correlation}'"
        ),
    )
    .await;
    assert_eq!(commands, 0);
    let applied: i64 = one(
        &x,
        &format!("SELECT count(*) FROM call_telephony_events WHERE call_id = '{call}'"),
    )
    .await;
    assert!(applied >= 6);
    let leg_usage: i64 = one(
        &x,
        &format!(
            "SELECT count(*) FROM call_usage_records WHERE call_id = '{call}' \
             AND usage_type = 'LEG_CONNECTED_TIME' AND leg_id = '{leg}'"
        ),
    )
    .await;
    assert_eq!(leg_usage, 1);

    // --- Logs: the correlation id is on the log lines of every step. ---
    let lines = capture.lines();
    let text = |l: &Value| l.to_string();
    let with = |message: &str| -> Vec<String> {
        lines
            .iter()
            .filter(|l| l["fields"]["message"] == message)
            .map(text)
            .filter(|l| l.contains(&call.to_string()))
            .collect()
    };
    for (step, message) in [
        ("request", "call requested"),
        ("call / route / leg", "call state changed"),
        ("telephony command", "telephony originate accepted"),
        ("telephony event", "telephony event processed"),
        ("usage", "call usage finalized"),
        ("leg usage", "usage fact finalized"),
    ] {
        let found = with(message);
        assert!(
            !found.is_empty(),
            "{step}: no `{message}` line for the call"
        );
        assert!(
            found.iter().all(|l| l.contains(&correlation)),
            "{step}: `{message}` without the correlation id: {found:?}"
        );
    }
    // Only safe identifiers: no telephone number, grant secret or join code in any line.
    let all = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    for needle in ["+234", "grant-secret", "cfmj"] {
        assert!(!all.contains(needle), "{needle} logged");
    }
}

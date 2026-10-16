//! §84: call lifecycle and RTC metrics (bounded labels only — the registry-wide label
//! guard is `metric_safety.rs`), and the end-of-call quality summary recorded from the
//! Call's real session evidence (admitted sessions, reconnects, connected participants).
//! Only data Calling observes is recorded: no jitter / RTT / packet loss is invented.

mod common;

use axum::http::StatusCode;
use calling::{calls::ids::CallId, metrics};
use common::app::AppWorld;
use sea_orm::ConnectionTrait;
use serde_json::json;

fn counter(name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
    metrics::counter(name, labels)
}

#[tokio::test]
async fn the_call_lifecycle_is_measured_with_bounded_labels() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let requests = counter("calling_call_requests_total", &[("kind", "application")]);
    let refused = counter(
        "calling_call_request_refusals_total",
        &[("kind", "application"), ("reason", "VALIDATION_ERROR")],
    );
    let setups = metrics::observations("calling_call_setup_seconds", &[("call_kind", "VOICE")]);
    let rings = metrics::observations("calling_call_ring_seconds", &[("call_kind", "VOICE")]);
    let durations =
        metrics::observations("calling_call_connected_seconds", &[("call_kind", "VOICE")]);
    let answered = counter(
        "calling_call_outcomes_total",
        &[("call_kind", "VOICE"), ("outcome", "answered")],
    );
    let declined = counter(
        "calling_call_outcomes_total",
        &[("call_kind", "VOICE"), ("outcome", "declined")],
    );
    let cancelled = counter(
        "calling_call_outcomes_total",
        &[("call_kind", "VOICE"), ("outcome", "cancelled")],
    );
    let admitted = counter(
        "calling_media_admissions_total",
        &[("source", "provider"), ("result", "admitted")],
    );

    // Answered → connected → ended.
    let (call, _, _) = w.connected_call("VOICE").await;
    w.h().clock.advance(chrono::Duration::seconds(3));
    let (_, v) = w.state(call).await;
    assert_eq!(w.command(call, "end", v).await.status, StatusCode::OK);
    // Declined.
    let (call, _, callee) = w.ringing_call("VOICE").await;
    let invitation = w.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, v) = w.state(call).await;
    assert_eq!(
        w.decline(call, invitation, &callee, v).await.status,
        StatusCode::OK
    );
    // Cancelled.
    let (call, _, _) = w.ringing_call("VOICE").await;
    let (_, v) = w.state(call).await;
    assert_eq!(w.command(call, "cancel", v).await.status, StatusCode::OK);
    // A refused request (bounded public error code as the reason).
    let caller = w.device(&w.w.initiator, "caller-main").await;
    let r = w
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": w.w.initiator.id(),
                "initiatorEndpointId": caller.endpoint,
                "targetPrincipalIds": [],
            }),
            &[("idempotency-key", "refused-1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    assert!(counter("calling_call_requests_total", &[("kind", "application")]) >= requests + 4);
    assert!(
        counter(
            "calling_call_request_refusals_total",
            &[("kind", "application"), ("reason", "VALIDATION_ERROR")]
        ) > refused
    );
    assert!(
        metrics::observations("calling_call_setup_seconds", &[("call_kind", "VOICE")]) > setups
    );
    assert!(
        metrics::observations("calling_call_ring_seconds", &[("call_kind", "VOICE")]) >= rings + 3
    );
    assert!(
        metrics::observations("calling_call_connected_seconds", &[("call_kind", "VOICE")])
            > durations
    );
    for (outcome, before) in [
        ("answered", answered),
        ("declined", declined),
        ("cancelled", cancelled),
    ] {
        assert!(
            counter(
                "calling_call_outcomes_total",
                &[("call_kind", "VOICE"), ("outcome", outcome)]
            ) > before,
            "{outcome}"
        );
    }
    assert!(
        counter(
            "calling_media_admissions_total",
            &[("source", "provider"), ("result", "admitted")]
        ) >= admitted + 2
    );
    // Nothing identifying in the rendered text of these series.
    let rendered = metrics::render();
    for line in rendered
        .lines()
        .filter(|l| l.starts_with("calling_call_") || l.starts_with("calling_media_admissions"))
    {
        assert!(!line.contains(&call.to_string()), "{line}");
        assert!(!line.contains(&w.w.initiator.id().to_string()), "{line}");
    }
}

#[tokio::test]
async fn the_end_of_call_quality_summary_carries_the_real_admitted_and_reconnect_counts() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let reconnects = counter("calling_media_reconnects_total", &[("call_kind", "VOICE")]);
    let (call, caller, _) = w.connected_call("VOICE").await;
    // The caller's connection drops (the provider reports it gone) and comes back.
    let session = w.session_on(call, caller.endpoint).await.unwrap();
    let identity: String = w
        .h()
        .conn()
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT media_identity FROM call_sessions WHERE id = '{session}'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let left = w
        .media()
        .participant(&AppWorld::room(call), &identity)
        .expect("the caller is connected at the provider");
    w.media().leave(&AppWorld::room(call), &identity);
    let (auth, body) =
        w.media()
            .signed_event("participant_left", &AppWorld::room(call), Some(&left));
    assert_eq!(w.callback(&auth, body).await.status, StatusCode::OK);
    let state = |sql_state: &'static str| {
        let conn = w.h().conn().clone();
        async move {
            common::scalar_i64(
                &conn,
                &format!(
                    "SELECT count(*) FROM call_sessions WHERE call_id = '{call}' AND state = '{sql_state}'"
                ),
            )
            .await
        }
    };
    assert_eq!(state("DISCONNECTED").await, 1, "the session dropped");
    w.connect(call, session).await;
    assert_eq!(state("CONNECTED").await, 2, "reconnected");
    w.h().clock.advance(chrono::Duration::seconds(4));
    let (_, v) = w.state(call).await;
    assert_eq!(w.command(call, "end", v).await.status, StatusCode::OK);
    let quality =
        w.w.calls()
            .quality_records(w.w.scope(), CallId::from_uuid(call))
            .await
            .unwrap();
    let summary = quality
        .iter()
        .find(|q| q.metrics.get("media_sessions_admitted").is_some())
        .expect("an end-of-call quality summary");
    assert_eq!(summary.metrics.get("media_sessions_admitted"), Some(2.0));
    assert_eq!(summary.metrics.get("session_reconnects"), Some(1.0));
    assert_eq!(summary.metrics.get("connected_participants"), Some(2.0));
    // Only server-observed facts: nothing the provider did not expose is invented.
    for invented in ["jitter_ms", "rtt_ms", "packet_loss", "mos_estimate"] {
        assert!(summary.metrics.get(invented).is_none(), "{invented}");
    }
    assert!(counter("calling_media_reconnects_total", &[("call_kind", "VOICE")]) > reconnects);
}

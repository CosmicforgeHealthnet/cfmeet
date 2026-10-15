//! SIP / media edge unavailable (Part I §87, §107), deterministic — the scripted edge
//! is switched DOWN (unreachable control plane; health UNAVAILABLE); no Asterisk is
//! needed (the real edge's signalling is certified by `sip_certification.rs`).
//!
//! * Public SIP-trunk originations fail safely: definite NOT-transmitted failures,
//!   nothing dialled, no usage, Calling told definitely; once the health refresh saw the
//!   edge down, ONE `SIP_EDGE_UNAVAILABLE` incident is open, the health gauge says so,
//!   and the SIP routes are skipped before any edge request. The edge back: the incident
//!   resolves and calls are dialled again — exactly once each.
//! * Enterprise SIP commands (internal, never paid) retry with backoff and stop at their
//!   attempt cap (DEAD_LETTER); a TERMINATE is never abandoned: unresolved, its leg is
//!   held in ENDING and reconciliation hangs the channel up once the edge is back.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{all_on, edge::ScriptedEdge, Harness, CALLING_PERMISSIONS};
use serde_json::json;
use telephony::{
    domain::LegStatus,
    sip::{EdgeEnterpriseDialog, EdgeEvent, EdgeEventKind},
};
use uuid::Uuid;

async fn world() -> Option<(Harness, ScriptedEdge)> {
    let edge = ScriptedEdge::default();
    let mut permissions = CALLING_PERMISSIONS.to_vec();
    permissions.push("telephony-leg.media");
    let h = Harness::build(
        &permissions,
        all_on().with_enterprise_sip(true),
        Some(Arc::new(edge.clone())),
    )
    .await?;
    let now = h.clock_now();
    h.sql(&format!(
        "INSERT INTO telephony_enterprise_devices (id, device_ref, edge_endpoint, tenant_id, application_id, \
           environment_id, status, created_at, updated_at) \
         VALUES ('{}', 'dev-front-desk-01', 'frontdesk', '{}', '{}', '{}', 'ENABLED', '{now}', '{now}')",
        Uuid::new_v4(),
        h.consumer.tenant_id,
        h.consumer.application_id,
        h.consumer.environment_id,
    ))
    .await;
    Some((h, edge))
}

async fn open_edge_incidents(h: &Harness) -> i64 {
    h.scalar(
        "SELECT count(*) FROM telephony_incidents WHERE category = 'SIP_EDGE_UNAVAILABLE' \
         AND status = 'OPEN' AND dedupe_key = 'sip-edge-health'",
    )
    .await
}

fn unhealthy_metric() -> u64 {
    telephony::metrics::counter(
        "telephony_sip_edge_unhealthy_total",
        &[("state", "UNAVAILABLE")],
    )
}

#[tokio::test]
async fn public_sip_operations_fail_safely_with_no_paid_effect_while_the_edge_is_down() {
    let Some((h, edge)) = world().await else {
        return;
    };
    let unhealthy_before = unhealthy_metric();
    edge.set_down(true);

    // An origination reaching the down edge: definite, nothing transmitted / dialled.
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "FAILED");
    assert_eq!(edge.dialled(), 0);
    assert_eq!(edge.live(), 0);
    assert_eq!(
        h.text(&format!(
            "SELECT failure_category FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "NETWORK"
    );
    let failed = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == json!(leg) && e["eventType"] == "LEG_FAILED")
        .expect("LEG_FAILED delivered");
    assert_eq!(failed["definite"], true);
    // No paid effect: no unknown outcome, no billable usage.
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_commands WHERE status = 'UNKNOWN_OUTCOME'")
            .await,
        0
    );
    h.advance(5);
    h.run().await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_usage_records WHERE billable_seconds > 0")
            .await,
        0
    );

    // The health refresh observes the outage: ONE incident (repeats are occurrences),
    // the bounded metric, the gauge and readiness; the SIP interconnects are UNAVAILABLE.
    h.svc.refresh_provider_health().await.unwrap();
    h.svc.refresh_provider_health().await.unwrap();
    assert_eq!(open_edge_incidents(&h).await, 1);
    assert_eq!(
        h.scalar(
            "SELECT occurrences::bigint FROM telephony_incidents WHERE category = 'SIP_EDGE_UNAVAILABLE' AND status = 'OPEN'"
        )
        .await,
        2
    );
    assert!(unhealthy_metric() >= unhealthy_before + 2);
    assert_eq!(
        h.scalar("SELECT count(*) FROM telecom_providers WHERE health = 'UNAVAILABLE'")
            .await,
        2
    );
    let metrics = h.svc.metrics_text().await.unwrap();
    assert!(
        metrics.contains("telephony_sip_edge_health{state=\"UNAVAILABLE\"} 1"),
        "{metrics}"
    );
    assert!(
        metrics.contains("telephony_open_incidents{state=\"SIP_EDGE_UNAVAILABLE\"} 1"),
        "{metrics}"
    );
    for forbidden in ["sip-a", "sip-b", "ng-a", "peer-", "+234", "frontdesk"] {
        assert!(!metrics.contains(forbidden), "{forbidden} in metrics");
    }
    assert_eq!(h.svc.readiness().await["sipEdge"], "UNAVAILABLE");

    // Further originations skip the unavailable SIP routes before any edge request.
    let calls = edge.originate_calls();
    let (second, _) = h.originate().await;
    h.run().await;
    assert_eq!(h.leg_status(second).await, "FAILED");
    assert_eq!(edge.originate_calls(), calls, "no request reached the edge");
    assert_eq!(edge.dialled(), 0);

    // The edge is back: the incident resolves and a new call is dialled exactly once.
    edge.set_down(false);
    h.svc.refresh_provider_health().await.unwrap();
    assert_eq!(open_edge_incidents(&h).await, 0);
    assert_eq!(
        h.scalar("SELECT count(*) FROM telecom_providers WHERE health = 'HEALTHY'")
            .await,
        2
    );
    let (third, _) = h.originate().await;
    h.run().await;
    h.run().await;
    assert_eq!(h.leg_status(third).await, "ORIGINATING");
    assert_eq!(edge.dialled(), 1);
    let metrics = h.svc.metrics_text().await.unwrap();
    assert!(
        metrics.contains("telephony_sip_edge_health{state=\"HEALTHY\"} 1"),
        "{metrics}"
    );
}

fn event(h: &Harness, channel: &str, kind: EdgeEventKind) -> EdgeEvent {
    EdgeEvent {
        event_id: format!("t-{}", Uuid::new_v4().simple()),
        channel: channel.to_string(),
        application_media: false,
        correlation_key: None,
        kind,
        at: h.now(),
    }
}

async fn command(h: &Harness, leg: Uuid, action: &str) -> Uuid {
    let id = Uuid::new_v4();
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{leg}/{action}"),
            Some(json!({
                "schemaVersion": "1",
                "commandId": id,
                "idempotencyKey": format!("{action}-{}", id.simple()),
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    id
}

async fn command_state(h: &Harness, id: Uuid) -> (String, String, i64) {
    (
        h.text(&format!(
            "SELECT status FROM telephony_commands WHERE id = '{id}'"
        ))
        .await
        .unwrap(),
        h.text(&format!(
            "SELECT coalesce(result_code, '') FROM telephony_commands WHERE id = '{id}'"
        ))
        .await
        .unwrap(),
        h.scalar(&format!(
            "SELECT attempts::bigint FROM telephony_commands WHERE id = '{id}'"
        ))
        .await,
    )
}

#[tokio::test]
async fn enterprise_sip_commands_stop_at_their_cap_and_a_terminate_completes_after_the_outage() {
    let Some((h, edge)) = world().await else {
        return;
    };
    // An enterprise device's dialog, offered to Calling while the edge was up.
    let channel = format!("chan-{}", Uuid::new_v4().simple());
    edge.add_live(&channel, LegStatus::Ringing);
    h.svc
        .handle_edge_event(event(
            &h,
            &channel,
            EdgeEventKind::EnterpriseInbound(EdgeEnterpriseDialog {
                endpoint: "frontdesk".into(),
                extension: "6000".into(),
            }),
        ))
        .await
        .unwrap();
    let leg: Uuid = h
        .text(&format!(
            "SELECT id::text FROM telephony_legs WHERE edge_channel_ref = '{channel}'"
        ))
        .await
        .expect("enterprise leg")
        .parse()
        .unwrap();
    assert_eq!(h.leg_status(leg).await, "RINGING");

    edge.set_down(true);
    // ANSWER cannot reach the edge: retried with backoff, then DEAD_LETTER at the cap.
    let answer = command(&h, leg, "answer").await;
    let mut rounds = 0;
    while command_state(&h, answer).await.0 != "DEAD_LETTER" {
        h.advance(61);
        h.svc.run_commands_once(10).await.unwrap();
        rounds += 1;
        assert!(rounds <= 12, "{:?}", command_state(&h, answer).await);
    }
    assert_eq!(
        command_state(&h, answer).await,
        ("DEAD_LETTER".into(), "ATTEMPTS_EXHAUSTED".into(), 10)
    );
    assert_eq!(h.leg_status(leg).await, "RINGING", "never answered");
    // Internal SIP never has a paid effect.
    assert_eq!(edge.dialled(), 0);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_legs WHERE id = '{leg}' AND provider_id IS NULL AND spend_reserved_minor IS NULL"
        ))
        .await,
        1
    );

    // TERMINATE during the outage is never dead-lettered: unresolved, the leg is held in
    // ENDING with an incident.
    let terminate = command(&h, leg, "terminate").await;
    let mut rounds = 0;
    while command_state(&h, terminate).await.0 == "PENDING" {
        h.advance(61);
        h.svc.run_commands_once(10).await.unwrap();
        rounds += 1;
        assert!(rounds <= 12, "{:?}", command_state(&h, terminate).await);
    }
    assert_eq!(command_state(&h, terminate).await.0, "UNKNOWN_OUTCOME");
    assert_eq!(h.leg_status(leg).await, "ENDING");
    assert_eq!(edge.live(), 1, "the channel is still up");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_incidents WHERE dedupe_key = 'unknown-termination:{leg}' AND status = 'OPEN'"
        ))
        .await,
        1
    );

    // The edge is back: reconciliation hangs the channel up and the leg ends.
    edge.set_down(false);
    for _ in 0..4 {
        h.advance(61);
        h.run().await;
    }
    assert_eq!(edge.live(), 0, "the channel was hung up");
    assert_eq!(h.leg_status(leg).await, "ENDED");
    assert!(h
        .calling
        .event_types_for(leg)
        .contains(&"LEG_ENDED".to_string()));
}

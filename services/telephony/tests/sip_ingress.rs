//! Public SIP ingress at Telephony (Part E §45, Part H §75), driven through
//! `handle_edge_event` with inbound dialogs from a scripted edge: only a configured,
//! enabled interconnect may deliver a call, only from its declared networks, only to a
//! relay number bound to it, within its dialog bound and per-minute rate. Every
//! rejection happens before ringing / Privacy Relay / Calling, and no incident evidence
//! ever carries a telephone number.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use common::{edge::ScriptedEdge, Harness, RELAY_NUMBER};
use telephony::{
    domain::LegStatus,
    inbound::MAX_INBOUND_PER_PEER_PER_MINUTE,
    metrics,
    ports::InboundMapping,
    providers::ExecutionNumber,
    sip::{ChannelState, EdgeEvent, EdgeEventKind, EdgeInbound},
};
use uuid::Uuid;

/// The (public) number of the calling party presented by the carrier.
const CALLER: &str = "+2348057770001";
/// The peer the harness binds to route ng-a (allowed source 192.0.2.0/24).
const PEER: &str = "peer-ng-a";
const ALLOWED_SOURCE: &str = "192.0.2.10";

struct World {
    h: Harness,
    edge: ScriptedEdge,
}

impl World {
    async fn new() -> Option<Self> {
        let edge = ScriptedEdge::default();
        let h = Harness::with_edge(Arc::new(edge.clone())).await?;
        h.privacy.mappings.lock().unwrap().insert(
            RELAY_NUMBER.to_string(),
            InboundMapping {
                decision_id: Uuid::new_v4(),
                context_id: Uuid::new_v4(),
                context_version: 1,
                relay_session_id: Uuid::new_v4(),
                relay_session_version: 1,
                assignment_id: Uuid::new_v4(),
                assignment_generation: 1,
                tenant_id: h.consumer.tenant_id,
                application_id: h.consumer.application_id,
                environment_id: h.consumer.environment_id,
                valid_until: h.now() + chrono::Duration::hours(1),
            },
        );
        Some(Self { h, edge })
    }

    /// Delivers one inbound dialog; returns its channel.
    async fn dialog(&self, peer: Option<&str>, source: Option<&str>, called: &str) -> String {
        let channel = format!("chan-{}", Uuid::new_v4().simple());
        self.edge.add_live(&channel, LegStatus::Ringing);
        self.h
            .svc
            .handle_edge_event(EdgeEvent {
                event_id: format!("t-{}", Uuid::new_v4().simple()),
                channel: channel.clone(),
                application_media: false,
                correlation_key: None,
                kind: EdgeEventKind::Inbound(EdgeInbound {
                    peer: peer.map(str::to_string),
                    source_address: source.map(|s| s.parse().unwrap()),
                    called: ExecutionNumber::parse(called),
                    caller: ExecutionNumber::parse(CALLER),
                    media_join_code: None,
                }),
                at: self.h.now(),
            })
            .await
            .unwrap();
        channel
    }

    async fn rejected(&self, channel: &str) -> bool {
        matches!(
            self.h.svc.edge().query(&channel.to_string()).await,
            ChannelState::Gone
        )
    }

    fn offers(&self) -> usize {
        self.h.calling.offers.lock().unwrap().len()
    }

    async fn inbound_legs(&self) -> i64 {
        self.h
            .scalar("SELECT count(*) FROM telephony_legs WHERE direction = 'INBOUND'")
            .await
    }

    async fn incidents(&self, category: &str, key: &str) -> i64 {
        self.h
            .scalar(&format!(
                "SELECT count(*) FROM telephony_incidents WHERE category = '{category}' AND dedupe_key = '{key}'"
            ))
            .await
    }

    /// A rejected dialog: refused at the edge, nothing offered, no leg, no caller
    /// protected (Privacy Relay never saw the number).
    async fn assert_rejected(&self, channel: &str, offers: usize, legs: i64, what: &str) {
        assert!(self.rejected(channel).await, "{what}: refused at the edge");
        assert_eq!(self.offers(), offers, "{what}: nothing offered to Calling");
        assert_eq!(self.inbound_legs().await, legs, "{what}: no leg");
        assert_eq!(
            self.h.privacy.protected.lock().unwrap().len(),
            offers,
            "{what}: the caller never reached the Contact Vault"
        );
    }

    /// No incident evidence (or key) carries a number.
    async fn assert_no_numbers_in_incidents(&self) {
        let text = self
            .h
            .text(
                "SELECT coalesce(string_agg(dedupe_key || ' ' || restricted_evidence::text, ' '), '') \
                 FROM telephony_incidents",
            )
            .await
            .unwrap_or_default();
        for forbidden in [
            "+234",
            &CALLER[1..],
            &RELAY_NUMBER[1..],
            "2342010000099",
            ALLOWED_SOURCE,
            "198.51.100",
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} in incidents: {text}"
            );
        }
    }
}

#[tokio::test]
async fn a_bound_relay_number_from_the_peers_network_is_offered() {
    let Some(w) = World::new().await else {
        return;
    };
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    assert!(!w.rejected(&channel).await);
    assert_eq!(w.offers(), 1);
    assert_eq!(w.inbound_legs().await, 1);
    assert_eq!(
        w.h.text("SELECT status FROM telephony_legs WHERE direction = 'INBOUND'")
            .await
            .as_deref(),
        Some("RINGING")
    );
    // The offer carries the protected caller reference, never the number.
    let offer = w.h.calling.offers.lock().unwrap()[0].to_string();
    assert!(!offer.contains(&CALLER[1..]), "{offer}");
    // Without a reported source address the edge's own `identify` match is the check.
    let channel = w.dialog(Some(PEER), None, RELAY_NUMBER).await;
    assert!(!w.rejected(&channel).await);
    assert_eq!(w.offers(), 2);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM telephony_incidents").await,
        0
    );
}

#[tokio::test]
async fn unbound_numbers_and_unknown_peers_are_rejected_as_sip_peer_rejected() {
    let Some(w) = World::new().await else {
        return;
    };
    // A known peer to a number that is not a relay number bound to it.
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), "+2342010000099")
        .await;
    w.assert_rejected(&channel, 0, 0, "unbound number").await;
    assert_eq!(
        w.incidents("SIP_PEER_REJECTED", &format!("sip-peer:{PEER}"))
            .await,
        1
    );
    // A relay number bound only for OUTBOUND use on this peer is unbound for inbound.
    w.h.sql(&format!(
        "UPDATE telecom_number_bindings SET capabilities = ARRAY['VOICE_OUTBOUND'] WHERE route_id = 'ng-a' AND public_e164 = '{RELAY_NUMBER}'"
    ))
    .await;
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    w.assert_rejected(&channel, 0, 0, "outbound-only binding")
        .await;
    // An unknown peer: one incident under the literal `unknown` subject.
    for peer in ["ghost-peer", "another-ghost"] {
        let channel = w
            .dialog(Some(peer), Some(ALLOWED_SOURCE), RELAY_NUMBER)
            .await;
        w.assert_rejected(&channel, 0, 0, "unknown peer").await;
    }
    assert_eq!(
        w.incidents("SIP_PEER_REJECTED", "sip-peer:unknown").await,
        1
    );
    assert!(
        !w.h.dump().await.contains("ghost"),
        "the claimed peer is never stored"
    );
    // No peer at all (the edge could not identify it): rejected.
    let channel = w.dialog(None, Some(ALLOWED_SOURCE), RELAY_NUMBER).await;
    w.assert_rejected(&channel, 0, 0, "no peer").await;
    w.assert_no_numbers_in_incidents().await;
}

#[tokio::test]
async fn a_source_outside_the_peers_networks_is_rejected_with_an_incident() {
    let Some(w) = World::new().await else {
        return;
    };
    let before = metrics::counter(
        "telephony_sip_ingress_rejections_total",
        &[("reason", "SOURCE_NOT_ALLOWED")],
    );
    for source in ["198.51.100.7", "2001:db8::7"] {
        let channel = w.dialog(Some(PEER), Some(source), RELAY_NUMBER).await;
        w.assert_rejected(&channel, 0, 0, source).await;
    }
    assert_eq!(
        w.incidents("SIP_PEER_REJECTED", &format!("sip-peer-source:{PEER}"))
            .await,
        1
    );
    assert!(
        metrics::counter(
            "telephony_sip_ingress_rejections_total",
            &[("reason", "SOURCE_NOT_ALLOWED")]
        ) >= before + 2
    );
    w.assert_no_numbers_in_incidents().await;
}

#[tokio::test]
async fn the_peers_dialog_bound_is_enforced() {
    let Some(w) = World::new().await else {
        return;
    };
    w.h.sql(&format!(
        "UPDATE sip_peer_config_refs SET max_concurrent_dialogs = 1 WHERE id = '{PEER}'"
    ))
    .await;
    let before = metrics::counter(
        "telephony_sip_ingress_rejections_total",
        &[("reason", "PEER_CAPACITY")],
    );
    let first = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    assert!(!w.rejected(&first).await);
    let second = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    w.assert_rejected(&second, 1, 1, "peer at capacity").await;
    assert!(
        metrics::counter(
            "telephony_sip_ingress_rejections_total",
            &[("reason", "PEER_CAPACITY")]
        ) > before
    );
    // The other peer is unaffected by this peer's bound.
    let other = w
        .dialog(Some("peer-ng-b"), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    assert!(!w.rejected(&other).await);
    assert_eq!(w.offers(), 2);
}

#[tokio::test]
async fn a_disabled_route_or_provider_rejects_its_peers_dialogs() {
    let Some(w) = World::new().await else {
        return;
    };
    w.h.sql(
        "UPDATE telecom_routes SET status = 'DISABLED', version = version + 1 WHERE id = 'ng-a'",
    )
    .await;
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    w.assert_rejected(&channel, 0, 0, "route disabled").await;
    w.h.sql(
        "UPDATE telecom_routes SET status = 'ENABLED', version = version + 1 WHERE id = 'ng-a'",
    )
    .await;
    w.h.sql("UPDATE telecom_providers SET status = 'DISABLED', version = version + 1 WHERE id = 'sip-a'")
        .await;
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    w.assert_rejected(&channel, 0, 0, "provider disabled").await;
    assert_eq!(
        w.incidents("SIP_PEER_REJECTED", &format!("sip-peer:{PEER}"))
            .await,
        1
    );
    // Re-enabled: accepted again.
    w.h.sql(
        "UPDATE telecom_providers SET status = 'ENABLED', version = version + 1 WHERE id = 'sip-a'",
    )
    .await;
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    assert!(!w.rejected(&channel).await);
    assert_eq!(w.offers(), 1);
    w.assert_no_numbers_in_incidents().await;
}

#[tokio::test]
async fn a_rate_limited_burst_raises_exactly_one_incident_whose_occurrences_grow() {
    let Some(w) = World::new().await else {
        return;
    };
    // The peer already delivered its per-minute allowance this minute.
    let now: DateTime<Utc> = w.h.now();
    let window = DateTime::<Utc>::from_timestamp(now.timestamp() - now.timestamp() % 60, 0)
        .unwrap()
        .to_rfc3339();
    w.h.sql(&format!(
        "INSERT INTO telephony_ingress_windows (subject_kind, subject_id, window_start, hits) \
         VALUES ('SIP_INGRESS', '{PEER}', '{window}', {MAX_INBOUND_PER_PEER_PER_MINUTE})"
    ))
    .await;
    let before = metrics::counter(
        "telephony_sip_ingress_rejections_total",
        &[("reason", "RATE_LIMITED")],
    );
    for i in 1..=4 {
        let channel = w
            .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
            .await;
        w.assert_rejected(&channel, 0, 0, "rate limited").await;
        assert_eq!(
            w.incidents(
                "SIP_INGRESS_RATE_LIMITED",
                &format!("sip-ingress-rate:{PEER}")
            )
            .await,
            1,
            "one incident after {i}"
        );
        assert_eq!(
            w.h.scalar(
                "SELECT occurrences::bigint FROM telephony_incidents WHERE category = 'SIP_INGRESS_RATE_LIMITED'"
            )
            .await,
            i,
            "occurrences grow"
        );
    }
    assert!(
        metrics::counter(
            "telephony_sip_ingress_rejections_total",
            &[("reason", "RATE_LIMITED")]
        ) >= before + 4
    );
    // Another peer is not limited by this one's burst.
    let other = w
        .dialog(Some("peer-ng-b"), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    assert!(!w.rejected(&other).await);
    // The next minute starts a new allowance.
    w.h.advance(60);
    let channel = w
        .dialog(Some(PEER), Some(ALLOWED_SOURCE), RELAY_NUMBER)
        .await;
    assert!(!w.rejected(&channel).await);
    assert_eq!(w.offers(), 2);
    w.assert_no_numbers_in_incidents().await;
}

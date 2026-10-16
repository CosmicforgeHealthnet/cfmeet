//! REAL lawful-interconnect certification (Part E §43, §225–§230).
//!
//! This is the only test that can certify a real carrier. It places a real call through a
//! real SIP trunk on a deployed edge to an operator-owned test line, and needs
//! credentials that are NEVER committed:
//!
//! * `TELEPHONY_SIP_EDGE=asterisk-ari` plus `TELEPHONY_ARI_URL`, `TELEPHONY_ARI_USERNAME`,
//!   `TELEPHONY_ARI_PASSWORD_FILE` (the deployed edge, with the trunk configured from the
//!   secret store);
//! * `TELEPHONY_REAL_CERT_TRUNK` — the PJSIP endpoint of the carrier trunk;
//! * `TELEPHONY_REAL_CERT_DESTINATION` — an operator-owned test line (E.164) that answers;
//! * `TELEPHONY_REAL_CERT_CALLER_ID` — a relay number the carrier authorized for the trunk.
//!
//! Without them the test reports `NOT RUN — REAL LAWFUL TELECOM INTERCONNECT
//! CERTIFICATION BLOCKED` and does not pretend to pass anything (it asserts nothing about
//! a carrier). `TELEPHONY_REAL_CERT_REQUIRED=1` turns the missing configuration into a
//! failure, for the certification pipeline of a real deployment.

use std::time::Duration;

use telephony::{
    domain::{Health, LegStatus, ProviderCapability},
    providers::{
        sip_trunk::SipTrunkProvider, ControlOutcome, ExecutionNumber, OriginateOutcome,
        OriginateRequest, QueryKey, QueryOutcome, TelecomInterconnectProvider,
    },
    sip::{edge_from_env, EdgeEventKind},
};
use uuid::Uuid;

fn required(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::test]
async fn real_lawful_interconnect_certification() {
    let config = (
        required("TELEPHONY_SIP_EDGE").filter(|e| e == "asterisk-ari"),
        required("TELEPHONY_REAL_CERT_TRUNK"),
        required("TELEPHONY_REAL_CERT_DESTINATION").and_then(|n| ExecutionNumber::parse(&n)),
        required("TELEPHONY_REAL_CERT_CALLER_ID").and_then(|n| ExecutionNumber::parse(&n)),
    );
    let (Some(_), Some(trunk), Some(destination), Some(caller)) = config else {
        let msg = "real interconnect certification: NOT RUN — REAL LAWFUL TELECOM INTERCONNECT \
                   CERTIFICATION BLOCKED (no lawful interconnect credentials / test line \
                   configured; nothing about a real carrier is certified)";
        if std::env::var("TELEPHONY_REAL_CERT_REQUIRED").as_deref() == Ok("1") {
            panic!("{msg}");
        }
        eprintln!("{msg}");
        return;
    };
    let edge =
        edge_from_env(cf_service_auth::RuntimeEnvironment::Staging).expect("edge configuration");
    let mut events = edge.subscribe();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(edge.health().await, Health::Healthy, "edge must be healthy");
    let provider = SipTrunkProvider::new(
        "real-cert",
        vec![
            ProviderCapability::OutboundVoice,
            ProviderCapability::SipTrunk,
            ProviderCapability::StatusQuery,
        ],
        edge.clone(),
    );
    let correlation = format!("cfm-{}", Uuid::new_v4().simple());
    let outcome = provider
        .originate(OriginateRequest {
            route_ref: "real-cert".into(),
            sip_peer_config_ref: Some(trunk),
            caller_identity: caller,
            destination,
            command_id: Uuid::new_v4(),
            idempotency_key: correlation.clone(),
            correlation_key: correlation.clone(),
            ring_timeout: Duration::from_secs(45),
            request_timeout: Duration::from_secs(5),
        })
        .await;
    let OriginateOutcome::Accepted {
        provider_call_ref, ..
    } = outcome
    else {
        panic!("the carrier did not accept the origination: {outcome:?}");
    };
    // Ringing (180/183) then answer (200) from the real network.
    let mut rang = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let answered = loop {
        let Ok(Ok(event)) = tokio::time::timeout_at(deadline, events.recv()).await else {
            break false;
        };
        if event.channel != provider_call_ref {
            continue;
        }
        match event.kind {
            EdgeEventKind::Ringing => rang = true,
            EdgeEventKind::Answered | EdgeEventKind::OriginatedAnswered => break true,
            EdgeEventKind::Hangup { .. } => break false,
            _ => {}
        }
    };
    assert!(
        answered,
        "the test line did not answer (ringing seen: {rang})"
    );
    let key = QueryKey {
        provider_call_ref: Some(provider_call_ref.clone()),
        correlation_key: correlation,
        transmitted_at: None,
    };
    assert!(matches!(
        provider.query_call(&key).await,
        QueryOutcome::Active {
            status: LegStatus::Connected,
            ..
        }
    ));
    tokio::time::sleep(Duration::from_secs(5)).await;
    let media = edge.media_stats(&provider_call_ref).await;
    assert!(
        matches!(media, Some((rx, _)) if rx > 0),
        "no RTP from the carrier: {media:?}"
    );
    assert_eq!(
        provider.terminate(&provider_call_ref).await,
        ControlOutcome::Done
    );
    eprintln!("real interconnect certification: outbound call answered, RTP received, BYE sent");
}

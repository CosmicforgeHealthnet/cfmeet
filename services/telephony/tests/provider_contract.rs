//! Generic interconnect provider contract suite (Part E §42–§43).
//!
//! Every adapter of `TelecomInterconnectProvider` must pass the SAME suite. It runs here
//! against the deterministic test adapter and against the SIP-trunk adapter over a
//! scripted edge; the SIP-trunk adapter over the real Asterisk edge is exercised by
//! `sip_certification.rs`. A real carrier behind the SIP trunk is NOT exercised by any
//! of these — that is the pending real-provider certification (ADR 0016).

mod common;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use common::edge::{EdgeScript, ScriptedEdge};
use telephony::{
    domain::{EndReason, Health, LegStatus, ProviderCapability},
    providers::{
        deterministic::{DeterministicProvider, Script, SIGNATURE_HEADER},
        sip_trunk::SipTrunkProvider,
        ControlOutcome, EventRejection, ExecutionNumber, OriginateOutcome, OriginateRequest,
        ProviderIngress, QueryKey, QueryOutcome, TelecomInterconnectProvider, UnknownReason,
    },
};
use uuid::Uuid;

const DESTINATION: &str = "+2348031234567";
const CALLER_ID: &str = "+2342010000001";

/// What the next origination should do, in adapter-neutral terms.
#[derive(Debug, Clone, Copy)]
enum Behaviour {
    Accept,
    /// Definitely nothing dialled, and nothing transmitted (safe retry).
    RefusedBeforeSend,
    /// Definitely nothing dialled, but the request reached the provider.
    RefusedAfterSend,
    /// Transmitted, no answer, and no call was created.
    LostWithoutEffect,
    /// Transmitted, no answer, but the call WAS created.
    LostWithEffect,
}

trait Fixture {
    fn name(&self) -> &'static str;
    fn provider(&self) -> Arc<dyn TelecomInterconnectProvider>;
    fn next(&self, behaviour: Behaviour);
    /// The remote party ends the call without any event reaching Telephony.
    fn end_silently(&self, provider_call_ref: &str);
    /// Whether the adapter's absence evidence covers requests sent after `since`.
    fn evidence_from(&self, since: Option<DateTime<Utc>>);
    /// A correctly authenticated callback for `body`, if the adapter takes HTTP callbacks.
    fn authenticated(&self, body: &[u8]) -> Option<ProviderIngress>;
    /// The number of provider-side call objects ever created.
    fn effects(&self) -> usize;
    /// The remote party answers.
    fn connect(&self, provider_call_ref: &str);
}

struct Deterministic(DeterministicProvider);

impl Fixture for Deterministic {
    fn name(&self) -> &'static str {
        "DETERMINISTIC_TEST"
    }
    fn provider(&self) -> Arc<dyn TelecomInterconnectProvider> {
        Arc::new(self.0.clone())
    }
    fn next(&self, behaviour: Behaviour) {
        self.0.script(&[match behaviour {
            Behaviour::Accept => Script::Accept,
            Behaviour::RefusedBeforeSend => Script::Fail {
                transmitted: false,
                reason: EndReason::Failed,
            },
            Behaviour::RefusedAfterSend => Script::Fail {
                transmitted: true,
                reason: EndReason::Failed,
            },
            Behaviour::LostWithoutEffect => Script::LoseRequest(UnknownReason::TimeoutAfterSend),
            Behaviour::LostWithEffect => {
                Script::AcceptButLoseResponse(UnknownReason::TimeoutAfterSend)
            }
        }]);
    }
    fn end_silently(&self, provider_call_ref: &str) {
        let key = provider_call_ref
            .strip_prefix("dp-")
            .expect("deterministic refs");
        self.0.end_silently(key, EndReason::Completed);
    }
    fn evidence_from(&self, _: Option<DateTime<Utc>>) {
        // The deterministic provider is authoritative for its own call objects.
    }
    fn authenticated(&self, body: &[u8]) -> Option<ProviderIngress> {
        let mut ingress = ingress(body);
        ingress
            .headers
            .insert(SIGNATURE_HEADER.to_string(), self.0.sign(body));
        Some(ingress)
    }
    fn effects(&self) -> usize {
        self.0.effects().len()
    }
    fn connect(&self, provider_call_ref: &str) {
        let key = provider_call_ref
            .strip_prefix("dp-")
            .expect("deterministic refs");
        self.0.progress(
            key,
            telephony::domain::NormalizedEvent::LegConnected,
            Utc::now(),
        );
    }
}

struct SipTrunk {
    edge: ScriptedEdge,
    provider: Arc<SipTrunkProvider>,
}

impl SipTrunk {
    fn new() -> Self {
        let edge = ScriptedEdge::default();
        edge.set_complete_since(Some(Utc::now() - chrono::Duration::hours(1)));
        let provider = Arc::new(SipTrunkProvider::new(
            "sip-contract",
            vec![
                ProviderCapability::OutboundVoice,
                ProviderCapability::InboundVoice,
                ProviderCapability::SipTrunk,
                ProviderCapability::StatusQuery,
                ProviderCapability::Events,
                ProviderCapability::Dtmf,
                ProviderCapability::CallerIdSelection,
            ],
            Arc::new(edge.clone()),
        ));
        Self { edge, provider }
    }
}

impl Fixture for SipTrunk {
    fn name(&self) -> &'static str {
        "SIP_TRUNK"
    }
    fn provider(&self) -> Arc<dyn TelecomInterconnectProvider> {
        self.provider.clone()
    }
    fn next(&self, behaviour: Behaviour) {
        self.edge.script(&[match behaviour {
            Behaviour::Accept => EdgeScript::Accept,
            Behaviour::RefusedBeforeSend => EdgeScript::Unavailable,
            Behaviour::RefusedAfterSend => EdgeScript::Refused,
            Behaviour::LostWithoutEffect => EdgeScript::TimeoutWithoutEffect,
            Behaviour::LostWithEffect => EdgeScript::TimeoutWithEffect,
        }]);
    }
    fn end_silently(&self, provider_call_ref: &str) {
        self.edge
            .end_remote(provider_call_ref, EndReason::Completed, true);
    }
    fn evidence_from(&self, since: Option<DateTime<Utc>>) {
        self.edge.set_complete_since(since);
    }
    fn authenticated(&self, _: &[u8]) -> Option<ProviderIngress> {
        None
    }
    fn effects(&self) -> usize {
        self.edge.dialled()
    }
    fn connect(&self, provider_call_ref: &str) {
        self.edge
            .set_status(provider_call_ref, LegStatus::Connected);
    }
}

fn fixtures() -> Vec<Box<dyn Fixture>> {
    vec![
        Box::new(Deterministic(DeterministicProvider::new(
            "dp-contract",
            b"contract-suite-callback-secret-0123456789",
        ))),
        Box::new(SipTrunk::new()),
    ]
}

fn ingress(body: &[u8]) -> ProviderIngress {
    ProviderIngress {
        provider_id: "contract".into(),
        path_token: None,
        headers: BTreeMap::new(),
        body: body.to_vec(),
        received_at: Utc::now(),
    }
}

fn request() -> OriginateRequest {
    let correlation = format!("cfm-{}", Uuid::new_v4().simple());
    OriginateRequest {
        route_ref: "ng-a".into(),
        sip_peer_config_ref: Some("peer-ng-a".into()),
        caller_identity: ExecutionNumber::parse(CALLER_ID).unwrap(),
        destination: ExecutionNumber::parse(DESTINATION).unwrap(),
        command_id: Uuid::new_v4(),
        idempotency_key: correlation.clone(),
        correlation_key: correlation,
        ring_timeout: Duration::from_secs(30),
        request_timeout: Duration::from_millis(500),
    }
}

fn key(r: &OriginateRequest, provider_call_ref: Option<&str>) -> QueryKey {
    QueryKey {
        provider_call_ref: provider_call_ref.map(str::to_string),
        correlation_key: r.correlation_key.clone(),
        transmitted_at: Some(Utc::now()),
    }
}

async fn accepted(f: &dyn Fixture) -> (OriginateRequest, String) {
    f.next(Behaviour::Accept);
    let r = request();
    match f.provider().originate(r.clone()).await {
        OriginateOutcome::Accepted {
            provider_call_ref, ..
        } => (r, provider_call_ref),
        other => panic!("{}: {other:?}", f.name()),
    }
}

#[tokio::test]
async fn adapters_advertise_capabilities_and_health_in_the_neutral_vocabulary() {
    for f in fixtures() {
        let p = f.provider();
        assert!(!p.id().is_empty());
        assert!(
            p.supports(ProviderCapability::OutboundVoice),
            "{}",
            f.name()
        );
        assert!(p.supports(ProviderCapability::StatusQuery), "{}", f.name());
        for c in p.capabilities() {
            assert!(ProviderCapability::parse(c.as_str()).is_some());
        }
        assert!(matches!(
            p.health().await,
            Health::Healthy | Health::Degraded | Health::Unavailable
        ));
        let _ = p.capacity().await;
    }
}

#[tokio::test]
async fn accepted_origination_is_readable_back_by_reference_and_by_correlation() {
    for f in fixtures() {
        let (r, call_ref) = accepted(f.as_ref()).await;
        assert!(!call_ref.is_empty());
        // A provider reference never carries the numbers.
        assert!(!call_ref.contains(&DESTINATION[1..]) && !call_ref.contains(&CALLER_ID[1..]));
        let p = f.provider();
        for k in [key(&r, Some(&call_ref)), key(&r, None)] {
            match p.query_call(&k).await {
                QueryOutcome::Active {
                    provider_call_ref,
                    status,
                } => {
                    assert_eq!(provider_call_ref, call_ref, "{}", f.name());
                    assert!(!status.is_terminal());
                }
                other => panic!("{}: {other:?}", f.name()),
            }
        }
        assert_eq!(f.effects(), 1, "{}", f.name());
    }
}

#[tokio::test]
async fn termination_is_idempotent_and_ends_the_call() {
    for f in fixtures() {
        let (r, call_ref) = accepted(f.as_ref()).await;
        let p = f.provider();
        assert_eq!(
            p.terminate(&call_ref).await,
            ControlOutcome::Done,
            "{}",
            f.name()
        );
        assert_eq!(
            p.terminate(&call_ref).await,
            ControlOutcome::AlreadyGone,
            "{}",
            f.name()
        );
        assert!(
            matches!(
                p.query_call(&key(&r, Some(&call_ref))).await,
                QueryOutcome::Ended { .. }
            ),
            "{}",
            f.name()
        );
        assert!(matches!(
            p.terminate("never-existed").await,
            ControlOutcome::AlreadyGone
        ));
    }
}

#[tokio::test]
async fn definite_failures_create_nothing_and_say_whether_they_were_transmitted() {
    for f in fixtures() {
        let p = f.provider();
        for (b, sent) in [
            (Behaviour::RefusedBeforeSend, false),
            (Behaviour::RefusedAfterSend, true),
        ] {
            f.next(b);
            let r = request();
            match p.originate(r.clone()).await {
                OriginateOutcome::DefiniteFailure { transmitted, .. } => {
                    assert_eq!(transmitted, sent, "{} {b:?}", f.name())
                }
                other => panic!("{} {b:?}: {other:?}", f.name()),
            }
            assert_eq!(
                p.query_call(&key(&r, None)).await,
                QueryOutcome::Absent,
                "{} {b:?}",
                f.name()
            );
        }
        assert_eq!(f.effects(), 0, "{}", f.name());
    }
}

#[tokio::test]
async fn a_lost_response_is_unknown_never_failed_and_read_back_finds_the_truth() {
    for f in fixtures() {
        let p = f.provider();
        // Lost, and the call WAS created: read-back must find it (late binding).
        f.next(Behaviour::LostWithEffect);
        let r = request();
        assert!(
            matches!(
                p.originate(r.clone()).await,
                OriginateOutcome::UnknownOutcome { .. }
            ),
            "{}",
            f.name()
        );
        assert!(
            matches!(
                p.query_call(&key(&r, None)).await,
                QueryOutcome::Active { .. }
            ),
            "{}",
            f.name()
        );
        // Lost, nothing created: with complete evidence, Absent.
        f.next(Behaviour::LostWithoutEffect);
        let r = request();
        assert!(matches!(
            p.originate(r.clone()).await,
            OriginateOutcome::UnknownOutcome { .. }
        ));
        assert_eq!(p.query_call(&key(&r, None)).await, QueryOutcome::Absent);
        assert_eq!(f.effects(), 1, "{}", f.name());
    }
}

#[tokio::test]
async fn absence_is_never_claimed_without_evidence_covering_the_transmission() {
    // The SIP edge cannot prove "never created" for a request sent while its event
    // history had a gap (e.g. a restart): the answer must be Unknown, so reconciliation
    // waits instead of letting a second leg be dialled.
    let f = SipTrunk::new();
    let p = f.provider();
    f.next(Behaviour::LostWithoutEffect);
    let r = request();
    let _ = p.originate(r.clone()).await;
    f.evidence_from(None);
    assert_eq!(p.query_call(&key(&r, None)).await, QueryOutcome::Unknown);
    f.evidence_from(Some(Utc::now() + chrono::Duration::seconds(5)));
    assert_eq!(p.query_call(&key(&r, None)).await, QueryOutcome::Unknown);
    // Transmission instant unknown to the caller: no proof either.
    let unknown_time = QueryKey {
        transmitted_at: None,
        ..key(&r, None)
    };
    f.evidence_from(Some(Utc::now() - chrono::Duration::hours(1)));
    assert_eq!(p.query_call(&unknown_time).await, QueryOutcome::Unknown);
    // Covered: Absent.
    assert_eq!(p.query_call(&key(&r, None)).await, QueryOutcome::Absent);
}

#[tokio::test]
async fn a_call_that_ended_unobserved_is_reported_ended_not_absent() {
    for f in fixtures() {
        let (r, call_ref) = accepted(f.as_ref()).await;
        f.end_silently(&call_ref);
        let p = f.provider();
        assert!(
            matches!(
                p.query_call(&key(&r, Some(&call_ref))).await,
                QueryOutcome::Ended { .. }
            ),
            "{}",
            f.name()
        );
        assert!(
            matches!(
                p.query_call(&key(&r, None)).await,
                QueryOutcome::Ended { .. }
            ),
            "{}",
            f.name()
        );
    }
}

#[tokio::test]
async fn hold_and_resume_follow_the_call_state() {
    for f in fixtures() {
        let (_, call_ref) = accepted(f.as_ref()).await;
        let p = f.provider();
        // Not connected yet: hold is refused, never faked.
        assert_eq!(
            p.hold(&call_ref).await,
            ControlOutcome::DefiniteFailure,
            "{}",
            f.name()
        );
        f.connect(&call_ref);
        assert_eq!(
            p.hold(&call_ref).await,
            ControlOutcome::Done,
            "{}",
            f.name()
        );
        assert_eq!(
            p.hold(&call_ref).await,
            ControlOutcome::DefiniteFailure,
            "{}",
            f.name()
        );
        assert_eq!(
            p.resume(&call_ref).await,
            ControlOutcome::Done,
            "{}",
            f.name()
        );
        assert_eq!(
            p.resume(&call_ref).await,
            ControlOutcome::DefiniteFailure,
            "{}",
            f.name()
        );
        assert_eq!(p.terminate(&call_ref).await, ControlOutcome::Done);
        assert_eq!(p.hold("never-existed").await, ControlOutcome::AlreadyGone);
        assert_eq!(p.resume("never-existed").await, ControlOutcome::AlreadyGone);
    }
}

#[tokio::test]
async fn callbacks_are_authenticated_before_parsing_and_parsed_strictly() {
    let body = br#"{"eventId":"ev-1","callRef":"dp-x","correlationKey":"cfm-x","event":"answered","occurredAt":"2026-10-08T10:00:00Z","sequence":1}"#;
    for f in fixtures() {
        let p = f.provider();
        // Unsigned, tampered or wrongly-signed callbacks are refused.
        assert_eq!(
            p.verify_event(&ingress(body)),
            Err(EventRejection::Unauthenticated),
            "{}",
            f.name()
        );
        let Some(good) = f.authenticated(body) else {
            // This adapter takes no HTTP callbacks at all (events come over the private
            // edge control plane): every HTTP callback is refused, even a "signed" one.
            let mut forged = ingress(body);
            forged
                .headers
                .insert(SIGNATURE_HEADER.into(), "v1=00".into());
            assert!(p.verify_event(&forged).is_err());
            assert!(p.normalize_event(&forged).is_err());
            continue;
        };
        assert_eq!(p.verify_event(&good), Ok(()));
        let mut tampered = good.clone();
        tampered.body = body.to_vec();
        tampered.body[10] ^= 1;
        assert_eq!(
            p.verify_event(&tampered),
            Err(EventRejection::Unauthenticated)
        );
        let events = p.normalize_event(&good).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.target_status(), Some(LegStatus::Answered));
        // Strict parser: unknown fields / statuses are malformed, not guessed.
        for bad in [
            &br#"{"eventId":"ev-2","event":"answered","occurredAt":"2026-10-08T10:00:00Z","extra":1}"#[..],
            br#"{"eventId":"ev-3","event":"probably-answered","occurredAt":"2026-10-08T10:00:00Z"}"#,
            br#"{"eventId":"ev-4","event":"completed","occurredAt":"2026-10-08T10:00:00Z","costMinor":-1,"currency":"NGN"}"#,
            b"not json",
        ] {
            let signed = f.authenticated(bad).unwrap();
            assert_eq!(
                p.normalize_event(&signed).map(|_| ()),
                Err(EventRejection::Malformed)
            );
        }
    }
}

#[tokio::test]
async fn requests_never_reveal_numbers_in_diagnostics() {
    let r = request();
    let debug = format!("{r:?}");
    assert!(!debug.contains(&DESTINATION[1..]));
    assert!(!debug.contains(&CALLER_ID[1..]));
}

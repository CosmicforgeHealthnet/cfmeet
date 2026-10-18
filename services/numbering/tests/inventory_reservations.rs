//! Part IV Workstream H: provider-neutral inventory search and reservation — canonical
//! ingestion (repeated, concurrent, stale), search filters / capabilities / KYC /
//! freshness / pagination / quotas, eligibility-bound idempotent reservations through the
//! deterministic provider, contention, cancellation and expiry with evidence-only provider
//! releases, unknown outcomes, environment separation, database guards, migration
//! rollback and the customer API (scopes, isolation, the served OpenAPI document).
mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::{
    acquisition as ax, catalog, contracts::conforms, exec, kyc as fx, providers as px, scalar,
    scope, strings, Harness,
};
use numbering::{
    acquisition::{
        reservations::{Expiry, ReservationRequest},
        search::{SearchOutcome, SearchPage, SearchRequest},
        Acquisition, AcquisitionSettings,
    },
    domain::RequestContext,
    numbers::{
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        inventory::NewObservation,
        kyc::SubjectKind,
        lifecycle::TransitionRequest,
        Actor, Capability as C, DomainError, EnvironmentKind, InventorySource, LifecycleState as S,
        NumberType, OwnerScope, OwnershipClass,
    },
    providers::{
        commands::CommandStatus, deterministic::Fault, InventoryQuery, ProviderOperation as Op,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

const COMMON: &str = "numbering/common/v1.schema.json#/$defs/";

fn ctx() -> RequestContext {
    RequestContext::generate()
}

struct H {
    h: Harness,
    p: px::Providers,
    a: Acquisition,
    runner: numbering::infra::jobs::JobRunner,
}

async fn setup_with(settings: AcquisitionSettings) -> Option<H> {
    let h = Harness::new().await?;
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let a = ax::of(&h, &p, settings);
    let runner = ax::runner(&h, &p, &a);
    Some(H { h, p, a, runner })
}

async fn setup() -> Option<H> {
    setup_with(AcquisitionSettings::default()).await
}

fn search(country: &str) -> SearchRequest {
    SearchRequest {
        country: country.into(),
        ..Default::default()
    }
}

impl H {
    async fn page(&self, s: &OwnerScope, r: &SearchRequest) -> SearchPage {
        match self.a.search(&ctx(), s, false, r).await.unwrap() {
            SearchOutcome::Page(p) => *p,
            SearchOutcome::Throttled { .. } => panic!("throttled"),
        }
    }

    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    /// Runs the reconciliation that a timeout scheduled (after its read-back delay).
    async fn reconcile(&self) {
        self.h.clock.advance(Duration::seconds(31));
        self.drain().await;
    }

    async fn id_of(&self, external_ref: &str) -> Uuid {
        Uuid::parse_str(
            &strings(
                self.h.conn(),
                &format!("SELECT number_resource_id::text FROM number_provider_inventory_refs WHERE external_ref = '{external_ref}'"),
            )
            .await
            .pop()
            .unwrap_or_else(|| panic!("{external_ref} not ingested")),
        )
        .unwrap()
    }

    async fn state(&self, id: Uuid) -> S {
        self.h.numbers.resource(id).await.unwrap().lifecycle_state()
    }

    /// Ingests the synthetic inventory (one search) and returns the number of `external_ref`.
    async fn ingested(&self, s: &OwnerScope, external_ref: &str) -> Uuid {
        self.page(s, &search("US")).await;
        self.id_of(external_ref).await
    }

    async fn reserve(
        &self,
        s: &OwnerScope,
        id: Uuid,
        caps: &[C],
        key: &str,
    ) -> Result<numbering::acquisition::reservations::Reserved, DomainError> {
        self.a
            .create_reservation(
                &ctx(),
                s,
                ReservationRequest {
                    number_resource_id: id,
                    capabilities: caps.to_vec(),
                    hold_seconds: None,
                    eligibility_decision_id: None,
                },
                key,
            )
            .await
    }

    async fn held(&self, s: &OwnerScope, external_ref: &str) -> (Uuid, Uuid) {
        let id = self.ingested(s, external_ref).await;
        let r = self
            .reserve(
                s,
                id,
                &[C::VoiceInbound],
                &format!("key-{}", Uuid::new_v4().simple()),
            )
            .await
            .unwrap();
        assert_eq!(r.reservation.status, "PENDING");
        self.drain().await;
        let r = self
            .h
            .numbers
            .reservation(s, r.reservation.id)
            .await
            .unwrap();
        assert_eq!(r.status, "HELD");
        assert_eq!(self.state(id).await, S::Reserved);
        (id, r.id)
    }

    async fn commands(&self, reservation: Uuid) -> Vec<(String, String)> {
        self.p
            .commands
            .reservation_commands(reservation)
            .await
            .unwrap()
            .into_iter()
            .map(|c| (c.operation, c.status))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

fn sandbox() -> OwnerScope {
    scope(EnvironmentKind::Sandbox)
}

// ---------------------------------------------------------------------------------------
// Step 1: canonical ingestion
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn repeated_and_concurrent_ingestion_yields_one_canonical_resource_per_number() {
    let Some(t) = setup().await else {
        return;
    };
    let q = InventoryQuery {
        country: "US".into(),
        number_type: None,
        capabilities: vec![],
        region: None,
        limit: 100,
    };
    // Four concurrent ingestions of the same offers, then a repeat.
    let c = &t.p.commands;
    let (c1, c2, c3, c4) = (ctx(), ctx(), ctx(), ctx());
    let (a, b, d, e) = tokio::join!(
        c.ingest_inventory(&c1, "synthetic", &q),
        c.ingest_inventory(&c2, "synthetic", &q),
        c.ingest_inventory(&c3, "synthetic", &q),
        c.ingest_inventory(&c4, "synthetic", &q),
    );
    for r in [a, b, d, e] {
        assert_eq!(r.unwrap().len(), 7);
    }
    let again = c.ingest_inventory(&ctx(), "synthetic", &q).await.unwrap();
    assert_eq!(t.h.scalar("SELECT count(*) FROM number_resources").await, 7);
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_provider_inventory_refs")
            .await,
        7
    );
    // Same canonical ids on every observation, all AVAILABLE, each with one current
    // capability record per offered capability (never duplicated by a lost race).
    let ids: std::collections::BTreeSet<Uuid> = again.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 7);
    assert!(again.iter().all(|r| r.state == "AVAILABLE"));
    assert_eq!(
        t.h.scalar(
            "SELECT count(*) FROM (SELECT number_resource_id, capability FROM number_capabilities \
              WHERE effective_to IS NULL GROUP BY 1, 2 HAVING count(*) > 1) d"
        )
        .await,
        0
    );
    // An unavailable provider object is never ingested.
    assert!(t
        .h
        .scalar("SELECT count(*) FROM number_provider_inventory_refs WHERE external_ref = 'syn-restricted'")
        .await
        == 0);
    // A stale (older) observation is recorded but never makes the number look fresher.
    let id = t.id_of("syn-voice-only").await;
    let before = t.h.numbers.resource(id).await.unwrap();
    let mut stale = NewObservation::offered(
        InventorySource::SyntheticTest,
        t.h.now() - Duration::hours(2),
        Duration::hours(3),
    );
    stale.provider_id = None;
    let after = t.h.numbers.observe(&ctx(), id, stale).await.unwrap();
    assert_eq!(after.last_observed_at, before.last_observed_at);
    assert_eq!(
        after.observation_fresh_until,
        before.observation_fresh_until
    );
}

// ---------------------------------------------------------------------------------------
// Step 2: search
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn search_filters_by_country_region_type_capability_and_prefix_with_kyc_and_provenance() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let all = t.page(&s, &search("US")).await;
    assert_eq!(all.inventory.refresh, "REFRESHED");
    assert_eq!(all.results.len(), 7);
    assert!(all.results.iter().all(|r| r.availability == "AVAILABLE"));
    let refs = |p: &SearchPage| -> Vec<String> {
        let mut v: Vec<String> = p.results.iter().map(|r| r.resource.e164.clone()).collect();
        v.sort();
        v
    };
    // Capabilities: SMS_OUTBOUND is excluded by the GEOGRAPHIC catalog: mobiles only.
    let sms = t
        .page(
            &s,
            &SearchRequest {
                capabilities: vec![C::SmsOutbound],
                ..search("US")
            },
        )
        .await;
    assert_eq!(refs(&sms), vec!["+15550100003", "+15550100006"]);
    for r in &sms.results {
        assert!(r.resolution.get(C::SmsOutbound).supported);
        assert!(r.resolution.get(C::SmsOutbound).provenance.synthetic);
        assert!(
            !r.resolution
                .get(C::SmsOutbound)
                .provenance
                .carrier_confirmed
        );
        // MOBILE: KYC for PERSONAL and ORGANIZATION ownership (F requirements).
        assert_eq!(r.kyc.status, "REQUIRED_FOR_OWNERSHIP_CLASSES");
        assert_eq!(r.kyc.ownership_classes, vec!["ORGANIZATION", "PERSONAL"]);
    }
    assert!(sms
        .results
        .iter()
        .any(|r| r.kyc.number_flagged && r.resource.e164 == "+15550100006"));
    // Voice outbound: the inbound-only and toll-free numbers never match.
    let voice = t
        .page(
            &s,
            &SearchRequest {
                capabilities: vec![C::VoiceOutbound],
                ..search("US")
            },
        )
        .await;
    assert_eq!(
        refs(&voice),
        vec![
            "+15550100001",
            "+15550100003",
            "+15550100006",
            "+15550100008"
        ]
    );
    // Region, type and prefix.
    let ca = t
        .page(
            &s,
            &SearchRequest {
                region: Some("US-CA".into()),
                ..search("US")
            },
        )
        .await;
    assert_eq!(refs(&ca), vec!["+15550100001"]);
    let toll = t
        .page(
            &s,
            &SearchRequest {
                number_type: Some(NumberType::TollFree),
                ..search("US")
            },
        )
        .await;
    assert_eq!(refs(&toll), vec!["+15550100005"]);
    assert_eq!(toll.results[0].kyc.status, "NOT_REQUIRED");
    let prefix = t
        .page(
            &s,
            &SearchRequest {
                prefix: Some("1555010000".into()),
                ..search("US")
            },
        )
        .await;
    assert_eq!(prefix.results.len(), 7);
    let none = t
        .page(
            &s,
            &SearchRequest {
                prefix: Some("+1556".into()),
                ..search("US")
            },
        )
        .await;
    assert!(none.results.is_empty());
    // Another country: the provider offers nothing, nothing is invented.
    assert!(t.page(&s, &search("NG")).await.results.is_empty());
    // A repeated query within the refresh interval does not call the provider again.
    let searches =
        t.p.sim
            .calls()
            .iter()
            .filter(|(op, _, _)| *op == Op::Search)
            .count();
    assert_eq!(t.page(&s, &search("US")).await.inventory.refresh, "CACHED");
    assert_eq!(
        t.p.sim
            .calls()
            .iter()
            .filter(|(op, _, _)| *op == Op::Search)
            .count(),
        searches
    );
}

#[tokio::test]
async fn stale_inventory_and_provider_outages_are_reported_never_presented_as_live_stock() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    t.page(&s, &search("US")).await;
    // The provider becomes unreachable and the observations expire.
    t.p.sim.set_outage(true);
    t.h.clock.advance(Duration::minutes(20));
    let page = t.page(&s, &search("US")).await;
    assert_eq!(page.inventory.refresh, "PROVIDER_UNAVAILABLE");
    assert_eq!(page.results.len(), 7);
    assert!(page.results.iter().all(|r| r.availability == "STALE"));
    // A stale number is not reservable until it is observed again.
    let id = t.id_of("syn-voice-only").await;
    assert_eq!(
        t.reserve(&s, id, &[], "stale-key-0001")
            .await
            .unwrap_err()
            .code(),
        "INVENTORY_NOT_FRESH"
    );
    // The provider is back: after the back-off the next search refreshes.
    t.p.sim.set_outage(false);
    t.h.clock.advance(Duration::seconds(31));
    let page = t.page(&s, &search("US")).await;
    assert_eq!(page.inventory.refresh, "REFRESHED");
    assert!(page.results.iter().all(|r| r.availability == "AVAILABLE"));
    assert_eq!(t.h.scalar("SELECT count(*) FROM number_resources").await, 7);
}

#[tokio::test]
async fn search_pages_are_bounded_bound_to_their_query_and_quota_limited() {
    let Some(t) = setup_with(AcquisitionSettings {
        search_requests_per_minute: 8,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let s = sandbox();
    let first = t
        .page(
            &s,
            &SearchRequest {
                limit: Some(3),
                ..search("US")
            },
        )
        .await;
    assert_eq!(first.results.len(), 3);
    let mut seen: Vec<String> = first
        .results
        .iter()
        .map(|r| r.resource.e164.clone())
        .collect();
    let mut cursor = first.next_cursor.clone();
    while let Some(c) = cursor {
        let next = t
            .page(
                &s,
                &SearchRequest {
                    limit: Some(3),
                    cursor: Some(c),
                    ..search("US")
                },
            )
            .await;
        assert_eq!(next.inventory.refresh, "NOT_REQUESTED");
        seen.extend(next.results.iter().map(|r| r.resource.e164.clone()));
        cursor = next.next_cursor;
    }
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!((seen.len(), sorted.len()), (7, 7));
    // A cursor of another query, malformed input and an oversized page are refused.
    let other =
        t.a.search(
            &ctx(),
            &s,
            false,
            &SearchRequest {
                limit: Some(3),
                number_type: Some(NumberType::Mobile),
                cursor: first.next_cursor.clone(),
                ..search("US")
            },
        )
        .await
        .unwrap_err();
    assert_eq!(other.code(), "VALIDATION_ERROR");
    for bad in [
        search("us"),
        search("USA"),
        SearchRequest {
            limit: Some(51),
            ..search("US")
        },
        SearchRequest {
            prefix: Some("12ab".into()),
            ..search("US")
        },
        SearchRequest {
            region: Some("x".repeat(101)),
            ..search("US")
        },
        SearchRequest {
            number_type: Some(NumberType::Unclassified),
            ..search("US")
        },
    ] {
        assert_eq!(
            t.a.search(&ctx(), &s, false, &bad)
                .await
                .unwrap_err()
                .code(),
            "VALIDATION_ERROR"
        );
    }
    // The quota (8 per minute, every search counts) is per environment and shared.
    let mut throttled = 0;
    for _ in 0..6 {
        if let SearchOutcome::Throttled {
            retry_after_seconds,
        } = t.a.search(&ctx(), &s, false, &search("US")).await.unwrap()
        {
            assert!((1..=60).contains(&retry_after_seconds));
            throttled += 1;
        }
    }
    assert!(throttled >= 1);
    // Another environment has its own quota.
    assert!(matches!(
        t.a.search(&ctx(), &sandbox(), false, &search("US"))
            .await
            .unwrap(),
        SearchOutcome::Page(_)
    ));
    // A new window resets the quota.
    t.h.clock.advance(Duration::minutes(1));
    assert!(matches!(
        t.a.search(&ctx(), &s, false, &search("US")).await.unwrap(),
        SearchOutcome::Page(_)
    ));
}

// ---------------------------------------------------------------------------------------
// Step 3: reservation creation
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_reservation_is_a_durable_idempotent_provider_hold() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let id = t.ingested(&s, "syn-voice-only").await;
    let created = t
        .reserve(&s, id, &[C::VoiceInbound, C::VoiceOutbound], "order-0001")
        .await
        .unwrap();
    assert!(!created.replayed);
    let r = &created.reservation;
    assert_eq!(
        (r.status.as_str(), r.hold_basis.as_str()),
        ("PENDING", "PRE_ELIGIBILITY_HOLD")
    );
    assert_eq!(
        r.requested_capabilities,
        vec!["VOICE_INBOUND", "VOICE_OUTBOUND"]
    );
    assert_eq!(t.state(id).await, S::Reserving);
    // The intent and the RESERVE command committed together; the provider is called by
    // the dispatch worker, with the command id as its idempotency key.
    assert_eq!(
        t.commands(r.id).await,
        vec![("RESERVE".into(), "PENDING".into())]
    );
    // Replays (also while PENDING and once HELD) return the same reservation.
    let replay = t
        .reserve(&s, id, &[C::VoiceOutbound, C::VoiceInbound], "order-0001")
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.reservation.id, r.id);
    t.drain().await;
    let held = t.h.numbers.reservation(&s, r.id).await.unwrap();
    assert_eq!(held.status, "HELD");
    assert!(held.provider_hold_ref.is_some());
    assert_eq!(t.state(id).await, S::Reserved);
    let replay = t
        .reserve(&s, id, &[C::VoiceInbound, C::VoiceOutbound], "order-0001")
        .await
        .unwrap();
    assert_eq!(
        (replay.replayed, replay.reservation.status.as_str()),
        (true, "HELD")
    );
    assert_eq!(
        t.p.sim.executions(Op::Reserve, t.commands_id(r.id).await),
        1
    );
    // The key with a different request, a new key for the held number, the key in
    // another scope (independent: there the number is simply not available).
    assert_eq!(
        t.reserve(&s, id, &[C::VoiceInbound], "order-0001")
            .await
            .unwrap_err()
            .code(),
        "IDEMPOTENCY_KEY_REUSED"
    );
    assert_eq!(
        t.reserve(&s, id, &[C::VoiceInbound], "order-0002")
            .await
            .unwrap_err()
            .code(),
        "NOT_ALLOCATABLE"
    );
    assert_eq!(
        t.reserve(
            &sandbox(),
            id,
            &[C::VoiceInbound, C::VoiceOutbound],
            "order-0001"
        )
        .await
        .unwrap_err()
        .code(),
        "NOT_ALLOCATABLE"
    );
    // Malformed keys are refused before anything is written.
    assert_eq!(
        t.reserve(&s, id, &[], "short").await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

impl H {
    async fn commands_id(&self, reservation: Uuid) -> Uuid {
        self.p
            .commands
            .reservation_commands(reservation)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.operation == "RESERVE")
            .unwrap()
            .id
    }
}

#[tokio::test]
async fn concurrent_reservations_of_one_number_have_exactly_one_winner() {
    let Some(t) = setup().await else {
        return;
    };
    let id = t.ingested(&sandbox(), "syn-voice-sms-mobile").await;
    let scopes: Vec<OwnerScope> = (0..6).map(|_| sandbox()).collect();
    let results = futures_join(&t, &scopes, id).await;
    let won: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
    assert_eq!(won.len(), 1, "{results:?}");
    for r in results.iter().filter_map(|r| r.as_ref().err()) {
        assert_eq!(r, "NOT_ALLOCATABLE");
    }
    assert_eq!(
        t.h.scalar(&format!(
            "SELECT count(*) FROM number_reservations WHERE number_resource_id = '{id}'"
        ))
        .await,
        1
    );
    t.drain().await;
    assert_eq!(
        t.p.sim
            .calls()
            .iter()
            .filter(|(op, _, o)| *op == Op::Reserve && *o == "EXECUTED")
            .count(),
        1
    );
}

async fn futures_join(t: &H, scopes: &[OwnerScope], id: Uuid) -> Vec<Result<Uuid, String>> {
    let tasks: Vec<_> = scopes
        .iter()
        .map(|s| {
            let a = t.a.clone();
            let s = *s;
            tokio::spawn(async move {
                a.create_reservation(
                    &RequestContext::generate(),
                    &s,
                    ReservationRequest {
                        number_resource_id: id,
                        capabilities: vec![],
                        hold_seconds: None,
                        eligibility_decision_id: None,
                    },
                    "race-key-0001",
                )
                .await
                .map(|r| r.reservation.id)
                .map_err(|e| e.code().to_string())
            })
        })
        .collect();
    let mut out = Vec::new();
    for task in tasks {
        out.push(task.await.unwrap());
    }
    out
}

#[tokio::test]
async fn eligibility_bounds_every_reservation_and_production_never_holds_before_it() {
    let Some(t) = setup_with(AcquisitionSettings {
        pre_eligibility_hold_environments: Default::default(),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let s = sandbox();
    let id = t.ingested(&s, "syn-voice-only").await;
    // No pre-eligibility holds by policy: a decision is required.
    assert_eq!(
        t.reserve(&s, id, &[C::VoiceInbound], "elig-key-001")
            .await
            .unwrap_err()
            .code(),
        "ELIGIBILITY_REQUIRED"
    );
    let decide = |subject_kind, ownership, caps: Vec<C>, number: Option<Uuid>, s: OwnerScope| {
        let n = t.h.numbers.clone();
        async move {
            n.evaluate_eligibility(
                &ctx(),
                &s,
                Standing {
                    active: true,
                    first_party: false,
                    live_telecom_allowed: false,
                },
                &EligibilityPolicy::default(),
                EligibilityRequest {
                    subject_kind,
                    subject_ref: Uuid::new_v4(),
                    country: "US".into(),
                    number_type: NumberType::Geographic,
                    ownership_class: ownership,
                    capabilities: caps,
                    number_resource_id: number,
                    provider_id: None,
                },
            )
            .await
            .unwrap()
        }
    };
    let ok = decide(
        SubjectKind::Organization,
        OwnershipClass::Organization,
        vec![C::VoiceInbound],
        Some(id),
        s,
    )
    .await;
    assert_eq!(ok.outcome, "ELIGIBLE");
    let with = |d: Uuid, caps: Vec<C>| ReservationRequest {
        number_resource_id: id,
        capabilities: caps,
        hold_seconds: Some(600),
        eligibility_decision_id: Some(d),
    };
    // Capabilities the decision did not cover, another scope's decision, a decision for
    // another number: refused.
    assert_eq!(
        t.a.create_reservation(
            &ctx(),
            &s,
            with(ok.id, vec![C::VoiceOutbound]),
            "elig-key-002"
        )
        .await
        .unwrap_err()
        .code(),
        "ELIGIBILITY_REQUIRED"
    );
    let foreign = decide(
        SubjectKind::Organization,
        OwnershipClass::Organization,
        vec![C::VoiceInbound],
        Some(id),
        sandbox(),
    )
    .await;
    assert_eq!(
        t.a.create_reservation(
            &ctx(),
            &s,
            with(foreign.id, vec![C::VoiceInbound]),
            "elig-key-003"
        )
        .await
        .unwrap_err()
        .code(),
        "ELIGIBILITY_REQUIRED"
    );
    let other_number = t.id_of("syn-inbound-only").await;
    let other = decide(
        SubjectKind::Organization,
        OwnershipClass::Organization,
        vec![C::VoiceInbound],
        Some(other_number),
        s,
    )
    .await;
    assert_eq!(
        t.a.create_reservation(
            &ctx(),
            &s,
            with(other.id, vec![C::VoiceInbound]),
            "elig-key-004"
        )
        .await
        .unwrap_err()
        .code(),
        "ELIGIBILITY_REQUIRED"
    );
    // An expired decision no longer admits.
    t.h.clock.advance(Duration::minutes(31));
    t.page(&s, &search("US")).await;
    assert_eq!(
        t.a.create_reservation(
            &ctx(),
            &s,
            with(ok.id, vec![C::VoiceInbound]),
            "elig-key-005"
        )
        .await
        .unwrap_err()
        .code(),
        "ELIGIBILITY_REQUIRED"
    );
    let fresh = decide(
        SubjectKind::Organization,
        OwnershipClass::Organization,
        vec![C::VoiceInbound],
        Some(id),
        s,
    )
    .await;
    let r =
        t.a.create_reservation(
            &ctx(),
            &s,
            with(fresh.id, vec![C::VoiceInbound]),
            "elig-key-006",
        )
        .await
        .unwrap()
        .reservation;
    assert_eq!(
        (r.hold_basis.as_str(), r.eligibility_decision_ref),
        ("ELIGIBILITY_DECISION", Some(fresh.id))
    );
    // PRODUCTION: never a synthetic number, never a pre-eligibility hold (policy AND
    // database), even when an operator misconfigures the policy.
    let prod = scope(EnvironmentKind::Production);
    let loose = Acquisition::new(
        t.h.numbers.clone(),
        Some(t.p.commands.clone()),
        t.h.state.eligibility.clone(),
        AcquisitionSettings {
            pre_eligibility_hold_environments: [EnvironmentKind::Production].into_iter().collect(),
            ..Default::default()
        },
    );
    let synthetic = t.id_of("syn-sms-only").await;
    assert_eq!(
        loose
            .create_reservation(
                &ctx(),
                &prod,
                ReservationRequest {
                    number_resource_id: synthetic,
                    capabilities: vec![],
                    hold_seconds: None,
                    eligibility_decision_id: None
                },
                "prod-key-0001",
            )
            .await
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
    let live = t.h.available_number(InventorySource::LiveProvider).await;
    exec(
        t.h.conn(),
        &format!(
            "INSERT INTO number_provider_inventory_refs (id, number_resource_id, provider_id, external_ref, first_seen_at) \
             VALUES (gen_random_uuid(), '{}', 'acme-telecom', 'acme-1', now())",
            live.id
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        loose
            .create_reservation(
                &ctx(),
                &prod,
                ReservationRequest {
                    number_resource_id: live.id,
                    capabilities: vec![],
                    hold_seconds: None,
                    eligibility_decision_id: None
                },
                "prod-key-0002",
            )
            .await
            .unwrap_err()
            .code(),
        "ELIGIBILITY_REQUIRED"
    );
    let err = exec(
        t.h.conn(),
        &format!(
            "INSERT INTO number_reservations (id, number_resource_id, tenant_id, application_id, environment_id, environment_kind, \
               service_account_id, status, expires_at, correlation_id, created_at, updated_at, hold_basis) \
             VALUES (gen_random_uuid(), '{}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'PRODUCTION', \
               gen_random_uuid(), 'PENDING', now() + interval '1 hour', 'corr_test0001', now(), now(), 'PRE_ELIGIBILITY_HOLD')",
            live.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("ck_number_reservations_production_basis"),
        "{err}"
    );
}

#[tokio::test]
async fn unsupported_quarantined_and_uncertain_numbers_are_never_reserved() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let voice_only = t.ingested(&s, "syn-voice-only").await;
    let err = t
        .reserve(&s, voice_only, &[C::SmsOutbound], "caps-key-001")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CAPABILITY_NOT_SUPPORTED");
    assert_eq!(
        t.reserve(&s, voice_only, &[C::GeographicLocal], "caps-key-002")
            .await
            .unwrap_err()
            .code(),
        "VALIDATION_ERROR"
    );
    // Quarantined (domain fixture) and uncertain numbers.
    let q = t.ingested(&s, "syn-sms-only").await;
    let r = t.h.numbers.resource(q).await.unwrap();
    t.h.numbers
        .transition(
            &ctx(),
            q,
            TransitionRequest::new(
                S::Quarantined,
                r.revision,
                Actor::operator("ops"),
                "HELD_BACK",
            )
            .with_quarantine_until(t.h.now() + Duration::days(30)),
        )
        .await
        .unwrap();
    assert_eq!(
        t.reserve(&s, q, &[], "quar-key-001")
            .await
            .unwrap_err()
            .code(),
        "NOT_ALLOCATABLE"
    );
    let u = t.id_of("syn-inbound-only").await;
    let r = t.h.numbers.resource(u).await.unwrap();
    t.h.numbers
        .transition(
            &ctx(),
            u,
            TransitionRequest::new(
                S::UnknownProviderState,
                r.revision,
                Actor::system("x"),
                "UNCERTAIN",
            )
            .uncertain(numbering::numbers::UncertainOperation::Discover),
        )
        .await
        .unwrap();
    assert_eq!(
        t.reserve(&s, u, &[], "unk-key-0001")
            .await
            .unwrap_err()
            .code(),
        "RECONCILIATION_REQUIRED"
    );
    // Neither appears in search.
    let page = t.page(&s, &search("US")).await;
    assert!(page
        .results
        .iter()
        .all(|r| r.resource.id != q && r.resource.id != u));
    // No provider configured: fail closed, nothing written.
    let bare = Acquisition::new(
        t.h.numbers.clone(),
        None,
        t.h.state.eligibility.clone(),
        AcquisitionSettings::default(),
    );
    assert_eq!(
        bare.create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: voice_only,
                capabilities: vec![],
                hold_seconds: None,
                eligibility_decision_id: None
            },
            "bare-key-001"
        )
        .await
        .unwrap_err()
        .code(),
        "PROVIDER_UNAVAILABLE"
    );
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_reservations").await,
        0
    );
}

// ---------------------------------------------------------------------------------------
// Step 4: cancellation, expiry, unknown outcomes
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn cancellation_gives_the_hold_back_and_frees_the_number_only_on_evidence() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid) = t.held(&s, "syn-voice-only").await;
    // Another scope cannot see or cancel it.
    assert_eq!(
        t.a.cancel_reservation(&ctx(), &sandbox(), rid)
            .await
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
    let cancelled = t.a.cancel_reservation(&ctx(), &s, rid).await.unwrap();
    assert_eq!(
        (
            cancelled.status.as_str(),
            cancelled.provider_release_state.as_str()
        ),
        ("CANCELLED", "PENDING")
    );
    // Not yet released: the number stays RESERVED and unreservable.
    assert_eq!(t.state(id).await, S::Reserved);
    assert_eq!(
        t.reserve(&sandbox(), id, &[], "after-cancel-1")
            .await
            .unwrap_err()
            .code(),
        "NOT_ALLOCATABLE"
    );
    assert_eq!(
        t.commands(rid).await,
        vec![
            ("RESERVE".into(), "SUCCEEDED".into()),
            ("RELEASE_HOLD".into(), "PENDING".into())
        ]
    );
    t.drain().await;
    let released = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(released.provider_release_state, "CONFIRMED");
    assert!(released.release_evidence_ref.is_some());
    assert_eq!(t.state(id).await, S::Available);
    // A second cancellation is an invalid transition, never a second release.
    assert_eq!(
        t.a.cancel_reservation(&ctx(), &s, rid)
            .await
            .unwrap_err()
            .code(),
        "INVALID_TRANSITION"
    );
    assert_eq!(t.commands(rid).await.len(), 2);
    // Free again: another tenant reserves it after a fresh observation.
    let other = sandbox();
    t.h.clock.advance(Duration::minutes(2));
    t.page(&other, &search("US")).await;
    let r = t.reserve(&other, id, &[], "other-key-001").await.unwrap();
    t.drain().await;
    assert_eq!(
        t.h.numbers
            .reservation(&other, r.reservation.id)
            .await
            .unwrap()
            .status,
        "HELD"
    );
}

#[tokio::test]
async fn the_expiry_worker_expires_due_reservations_and_releases_their_holds() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid) = t.held(&s, "syn-voice-sms-mobile").await;
    // Not due: nothing happens.
    assert_eq!(t.a.sweep_reservations(&ctx(), 100).await.unwrap(), (0, 0));
    assert_eq!(
        t.a.expire_reservation(&ctx(), rid).await.unwrap(),
        Expiry::NotDue
    );
    // Due exactly at expires_at (exclusive bound).
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    t.h.clock.set(r.expires_at);
    let (scheduled, _) = t.a.sweep_reservations(&ctx(), 100).await.unwrap();
    assert_eq!(scheduled, 1);
    // A repeated sweep never schedules twice.
    assert_eq!(t.a.sweep_reservations(&ctx(), 100).await.unwrap().0, 0);
    t.drain().await;
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(r.status, "EXPIRED");
    assert_eq!(r.provider_release_state, "CONFIRMED");
    assert_eq!(t.state(id).await, S::Available);
    // Repeating the expiry (retry safety) is a no-op.
    assert_eq!(
        t.a.expire_reservation(&ctx(), rid).await.unwrap(),
        Expiry::NotCurrent
    );
    // Auditable: lifecycle and reservation events in the outbox.
    assert!(
        t.h.scalar(&format!(
            "SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.reservation.changed' \
               AND aggregate_id = '{rid}'"
        ))
        .await
            >= 4
    );
}

#[tokio::test]
async fn unknown_release_outcomes_block_reallocation_until_read_back_proves_them() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid) = t.held(&s, "syn-voice-only").await;
    t.p.sim.inject(Op::Release, Fault::TimeoutAfterExecution);
    t.a.cancel_reservation(&ctx(), &s, rid).await.unwrap();
    t.drain().await;
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(r.provider_release_state, "UNKNOWN");
    assert_eq!(t.state(id).await, S::Reserved);
    // Never reallocated, never re-released blindly by the sweep.
    assert_eq!(t.a.sweep_reservations(&ctx(), 100).await.unwrap().1, 0);
    assert_eq!(
        t.reserve(&sandbox(), id, &[], "unknown-rel-1")
            .await
            .unwrap_err()
            .code(),
        "NOT_ALLOCATABLE"
    );
    assert_eq!(t.p.sim.executions(Op::Release, t.release_id(rid).await), 1);
    // Read-back (the provider released it) resolves it.
    t.reconcile().await;
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(r.provider_release_state, "CONFIRMED");
    assert_eq!(t.state(id).await, S::Available);
    assert_eq!(t.p.sim.executions(Op::Release, t.release_id(rid).await), 1);
}

impl H {
    async fn release_id(&self, reservation: Uuid) -> Uuid {
        self.p
            .commands
            .reservation_commands(reservation)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.operation == "RELEASE_HOLD")
            .unwrap()
            .id
    }
}

#[tokio::test]
async fn pending_and_uncertain_holds_are_never_ended_under_the_provider() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let id = t.ingested(&s, "syn-voice-only").await;
    let r = t
        .reserve(&s, id, &[], "pending-key-01")
        .await
        .unwrap()
        .reservation;
    assert_eq!(
        t.a.cancel_reservation(&ctx(), &s, r.id)
            .await
            .unwrap_err()
            .code(),
        "RESERVATION_BUSY"
    );
    // The hold is answered after the reservation expired: it is recorded and given back.
    t.h.clock.advance(Duration::minutes(16));
    assert_eq!(
        t.a.expire_reservation(&ctx(), r.id).await.unwrap(),
        Expiry::Deferred
    );
    t.drain().await;
    let late = t.h.numbers.reservation(&s, r.id).await.unwrap();
    assert_eq!(
        (late.status.as_str(), late.provider_release_state.as_str()),
        ("EXPIRED", "PENDING")
    );
    assert!(late.provider_hold_ref.is_some());
    assert_eq!(t.state(id).await, S::Reserving);
    assert_eq!(t.a.sweep_reservations(&ctx(), 100).await.unwrap().1, 1);
    t.drain().await;
    assert_eq!(
        t.h.numbers
            .reservation(&s, r.id)
            .await
            .unwrap()
            .provider_release_state,
        "CONFIRMED"
    );
    assert_eq!(t.state(id).await, S::Available);
    // A hold whose outcome is unknown is reconciled, never cancelled or re-sent.
    let id2 = t.id_of("syn-inbound-only").await;
    t.h.clock.advance(Duration::minutes(2));
    t.page(&s, &search("US")).await;
    t.p.sim.inject(Op::Reserve, Fault::TimeoutAfterExecution);
    let r2 = t
        .reserve(&s, id2, &[], "unknown-key-01")
        .await
        .unwrap()
        .reservation;
    t.drain().await;
    assert_eq!(
        t.h.numbers.reservation(&s, r2.id).await.unwrap().status,
        "UNKNOWN"
    );
    assert_eq!(t.state(id2).await, S::UnknownProviderState);
    assert_eq!(
        t.a.cancel_reservation(&ctx(), &s, r2.id)
            .await
            .unwrap_err()
            .code(),
        "RECONCILIATION_REQUIRED"
    );
    assert_eq!(
        t.reserve(&sandbox(), id2, &[], "unknown-key-02")
            .await
            .unwrap_err()
            .code(),
        "RECONCILIATION_REQUIRED"
    );
    t.reconcile().await;
    assert_eq!(
        t.h.numbers.reservation(&s, r2.id).await.unwrap().status,
        "HELD"
    );
    assert_eq!(t.state(id2).await, S::Reserved);
    assert_eq!(
        t.p.sim.executions(Op::Reserve, t.commands_id(r2.id).await),
        1
    );
    let cmd =
        t.p.commands
            .command(t.commands_id(r2.id).await)
            .await
            .unwrap();
    assert_eq!(cmd.status(), CommandStatus::ReconciledSucceeded);
}

// ---------------------------------------------------------------------------------------
// Database guards and migration rollback
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_database_guards_reservation_basis_conversion_and_hold_release_commands() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (_, rid) = t.held(&s, "syn-voice-only").await;
    for (sql, expected) in [
        (
            format!("UPDATE number_reservations SET hold_basis = 'DIRECT', revision = revision + 1 WHERE id = '{rid}'"),
            "immutable",
        ),
        (
            format!("UPDATE number_reservations SET requested_capabilities = ARRAY['SMS_OUTBOUND'], revision = revision + 1 WHERE id = '{rid}'"),
            "immutable",
        ),
        (
            format!("UPDATE number_reservations SET converted_to = 'ASSIGNMENT', revision = revision + 1 WHERE id = '{rid}'"),
            "ck_number_reservations_converted",
        ),
        (
            format!("INSERT INTO number_provider_commands (id, provider_id, operation, number_resource_id, idempotency_key, request, status, \
               max_attempts, correlation_id, created_at, updated_at) SELECT gen_random_uuid(), 'synthetic', 'RELEASE_HOLD', number_resource_id, \
               'no-reservation-1', '{{}}'::jsonb, 'PENDING', 5, 'corr_test0001', now(), now() FROM number_reservations WHERE id = '{rid}'"),
            "ck_number_provider_commands_reservation",
        ),
    ] {
        let err = exec(t.h.conn(), &sql).await.unwrap_err().to_string();
        assert!(err.contains(expected), "{sql}: {err}");
    }
}

#[tokio::test]
async fn the_workstream_h_migration_rolls_back_only_without_h_records() {
    let Some(t) = setup().await else {
        return;
    };
    use numbering_migration::{Migrator, MigratorTrait};
    let s = sandbox();
    let (_, rid) = t.held(&s, "syn-voice-only").await;
    t.a.cancel_reservation(&ctx(), &s, rid).await.unwrap();
    t.drain().await;
    let steps = common::steps_down_to("m20261020_000006_inventory_reservations");
    let err = Migrator::down(t.h.conn(), Some(steps)).await.unwrap_err();
    assert!(err.to_string().contains("Workstream H"), "{err}");
    let Some(clean) = common::TestDb::new().await else {
        return;
    };
    Migrator::down(&clean.conn, Some(steps)).await.unwrap();
    assert_eq!(
        scalar(
            &clean.conn,
            "SELECT count(*) FROM information_schema.columns WHERE table_name = 'number_reservations' AND column_name = 'hold_basis'"
        )
        .await,
        0
    );
    Migrator::up(&clean.conn, None).await.unwrap();
}

// ---------------------------------------------------------------------------------------
// The customer API
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_customer_api_is_scoped_isolated_contract_conformant_and_served_as_documented() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let a = ax::of(&h, &p, AcquisitionSettings::default());
    let runner = ax::runner(&h, &p, &a);
    let pa = h.pa.clone().unwrap();
    let all = ["numbers.search", "numbers.read", "numbers.manage"];
    let alice = pa.customer("SANDBOX", &all, &all).await;
    let bob = pa.customer("SANDBOX", &all, &all).await;
    let reader = pa
        .customer("SANDBOX", &["numbers.read"], &["numbers.read"])
        .await;

    // Countries and number types.
    let countries = get(&h, &alice.secret, "/v1/numbers/countries").await;
    assert_eq!(countries.status, StatusCode::OK, "{:?}", countries.body);
    conforms(&format!("{COMMON}CountryList"), &countries.body);
    assert_eq!(countries.body["data"][0]["countryCode"], "US");
    assert_eq!(countries.body["data"][0]["inventoryClass"], "SYNTHETIC");
    let types = get(&h, &alice.secret, "/v1/numbers/countries/US/number-types").await;
    conforms(&format!("{COMMON}NumberTypeList"), &types.body);
    assert_eq!(types.body["data"].as_array().unwrap().len(), 3);
    assert_eq!(
        get(&h, &alice.secret, "/v1/numbers/countries/NG/number-types")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Search (numbers.search), with the scope and parameter checks.
    let page = get(
        &h,
        &alice.secret,
        "/v1/numbers/search?countryCode=US&capabilities=VOICE_OUTBOUND&limit=2",
    )
    .await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    conforms(&format!("{COMMON}NumberSearchPage"), &page.body);
    assert_eq!(page.body["data"].as_array().unwrap().len(), 2);
    assert!(page.body["nextCursor"].is_string());
    assert_eq!(page.body["data"][0]["observation"]["guaranteed"], false);
    let text = page.body.to_string();
    assert!(
        !text.contains("syn-") && !text.contains("synthetic\":\"") && !text.contains("providerId")
    );
    for (path, code) in [
        ("/v1/numbers/search?countryCode=US", "INSUFFICIENT_SCOPE"),
        ("/v1/numbers/countries", "INSUFFICIENT_SCOPE"),
    ] {
        let r = get(&h, &reader.secret, path).await;
        assert_eq!((r.status, r.code()), (StatusCode::FORBIDDEN, code));
    }
    for bad in [
        "/v1/numbers/search",
        "/v1/numbers/search?countryCode=US&capabilities=TELEPATHY",
        "/v1/numbers/search?countryCode=US&limit=0",
        "/v1/numbers/search?countryCode=US&tenantId=x",
    ] {
        let r = get(&h, &alice.secret, bad).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{bad}"
        );
        conforms(&format!("{COMMON}Error"), &r.body);
    }
    // Reserve (numbers.manage, Idempotency-Key required).
    let number = page.body["data"][0]["numberResourceId"]
        .as_str()
        .unwrap()
        .to_string();
    let body = json!({ "numberResourceId": number, "capabilities": ["VOICE_INBOUND"] });
    let r = post(
        &h,
        &alice.secret,
        "/v1/numbers/reservations",
        Some(body.clone()),
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
    let r = post(
        &h,
        &reader.secret,
        "/v1/numbers/reservations",
        Some(body.clone()),
        Some("api-key-0001"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = post(
        &h,
        &alice.secret,
        "/v1/numbers/reservations",
        Some(json!({ "numberResourceId": number, "tenantId": Uuid::new_v4() })),
        Some("api-key-0001"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let created = post(
        &h,
        &alice.secret,
        "/v1/numbers/reservations",
        Some(body.clone()),
        Some("api-key-0001"),
    )
    .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    conforms(&format!("{COMMON}Reservation"), &created.body);
    assert_eq!(created.body["status"], "PENDING");
    let replay = post(
        &h,
        &alice.secret,
        "/v1/numbers/reservations",
        Some(body.clone()),
        Some("api-key-0001"),
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body["reservationId"], created.body["reservationId"]);
    let reused = post(
        &h,
        &alice.secret,
        "/v1/numbers/reservations",
        Some(json!({ "numberResourceId": number })),
        Some("api-key-0001"),
    )
    .await;
    assert_eq!(reused.code(), "IDEMPOTENCY_KEY_REUSED");
    let taken = post(
        &h,
        &bob.secret,
        "/v1/numbers/reservations",
        Some(body.clone()),
        Some("api-key-0002"),
    )
    .await;
    assert_eq!(
        (taken.status, taken.body["error"]["reason"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("NOT_ALLOCATABLE"))
    );
    ax::drain(&runner).await;
    let rid = created.body["reservationId"].as_str().unwrap();
    let held = get(
        &h,
        &alice.secret,
        &format!("/v1/numbers/reservations/{rid}"),
    )
    .await;
    assert_eq!(held.body["status"], "HELD");
    conforms(&format!("{COMMON}Reservation"), &held.body);
    // Isolation: Bob cannot read, list or cancel Alice's reservation.
    assert_eq!(
        get(&h, &bob.secret, &format!("/v1/numbers/reservations/{rid}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let list = get(&h, &bob.secret, "/v1/numbers/reservations").await;
    assert_eq!(list.body["data"].as_array().unwrap().len(), 0);
    let r = post(
        &h,
        &bob.secret,
        &format!("/v1/numbers/reservations/{rid}/cancel"),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let list = get(
        &h,
        &alice.secret,
        "/v1/numbers/reservations?status=HELD&limit=1",
    )
    .await;
    conforms(&format!("{COMMON}ReservationList"), &list.body);
    assert_eq!(list.body["data"][0]["reservationId"], rid);
    let cancelled = post(
        &h,
        &alice.secret,
        &format!("/v1/numbers/reservations/{rid}/cancel"),
        None,
        None,
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{:?}", cancelled.body);
    assert_eq!(
        (
            cancelled.body["status"].as_str(),
            cancelled.body["providerReleaseState"].as_str()
        ),
        (Some("CANCELLED"), Some("PENDING"))
    );
    // The served API description is exactly the contract and lists these routes.
    let served = h
        .raw(
            axum::http::Request::get("/openapi.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(
        served.body,
        common::contracts::read("numbering/internal/v1.openapi.json")
    );
    for path in [
        "/v1/numbers/countries",
        "/v1/numbers/countries/{countryCode}/number-types",
        "/v1/numbers/search",
        "/v1/numbers/reservations",
        "/v1/numbers/reservations/{reservationId}/cancel",
    ] {
        assert!(
            served.body["paths"].get(path).is_some(),
            "{path} not served"
        );
    }
    let _ = Arc::new(());
}

async fn get(h: &Harness, token: &str, path: &str) -> common::Reply {
    h.customer_with(token, Method::GET, path, None, &[]).await
}

async fn post(
    h: &Harness,
    token: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> common::Reply {
    let headers: Vec<(&str, &str)> = key
        .map(|k| vec![("idempotency-key", k)])
        .unwrap_or_default();
    h.customer_with(token, Method::POST, path, body, &headers)
        .await
}

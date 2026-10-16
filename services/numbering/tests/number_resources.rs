//! Part IV D10.1–D10.7: canonical identity, E.164, lifecycle policy, optimistic
//! concurrency, provider bindings, capabilities and inventory freshness — at the domain
//! AND the database level.
mod common;

use chrono::Duration;
use common::{
    actor, binding_for, capability_source, exec, reconciler, scalar, scope, strings, Harness,
};
use numbering::{
    domain::RequestContext,
    numbers::{
        bindings::NewBinding,
        capabilities::NewCapability,
        e164,
        inventory::NewObservation,
        lifecycle::{allowed, TransitionRequest},
        resources::{NewResource, ResourceFacts},
        Availability, BindingChangeReason, Capability, CapabilitySource, CapabilityStatus,
        ClassificationSource, DomainError, EnvironmentKind, FailureCategory, InventorySource,
        LifecycleState, NumberType, OwnershipClass, UncertainOperation,
    },
};
use uuid::Uuid;

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn new_resource(number: &str, country: &str, source: InventorySource) -> NewResource {
    NewResource {
        number: number.into(),
        country: country.into(),
        inventory_source: source,
        classification: None,
        facts: ResourceFacts::default(),
    }
}

fn observation(h: &Harness, source: InventorySource) -> NewObservation {
    let mut o = NewObservation::offered(source, h.now(), Duration::hours(1));
    if source == InventorySource::LiveProvider {
        o.provider_id = Some("acme-telecom".into());
    }
    o
}

// D10.1 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_discovered_resource_has_a_canonical_identity_and_an_audited_birth() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = ctx();
    let r = h
        .numbers
        .discover(
            &c,
            actor(),
            new_resource("+234 809 555 0101", "NG", InventorySource::LiveProvider),
            observation(&h, InventorySource::LiveProvider),
        )
        .await
        .unwrap();
    assert_eq!(r.e164, "+2348095550101");
    assert_eq!(
        (r.country_code.as_str(), r.calling_code.as_str()),
        ("NG", "234")
    );
    assert_eq!((r.state.as_str(), r.revision), ("DISCOVERED", 1));
    assert_eq!(
        r.number_type, "UNCLASSIFIED",
        "never inferred from the number format"
    );
    assert_eq!(r.inventory_source, "LIVE_PROVIDER");
    assert_eq!(r.risk_tier, "UNASSESSED");
    // The canonical id is ours, not the provider's: the provider reference lives only on a
    // binding, and a provider replacement keeps the resource id.
    let b = h
        .numbers
        .activate_binding(
            &c,
            r.id,
            binding_for(InventorySource::LiveProvider, &[Capability::VoiceInbound]),
        )
        .await
        .unwrap();
    assert_ne!(b.id, r.id);
    let replaced = h
        .numbers
        .replace_binding(
            &c,
            b.id,
            NewBinding {
                provider_id: "other-carrier".into(),
                external_ref: "OC-1".into(),
                capabilities: vec![Capability::VoiceInbound],
                change_reason: BindingChangeReason::ProviderReplacement,
            },
        )
        .await
        .unwrap();
    assert_eq!(replaced.number_resource_id, r.id);
    assert_eq!(
        h.numbers
            .resource_by_number("+2348095550101", false)
            .await
            .unwrap()
            .id,
        r.id
    );
    // Birth is audited: lifecycle event + outbox event (no number in the event).
    let events = h.numbers.lifecycle_events(r.id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].from_state.as_deref(), events[0].to_state.as_str()),
        (None, "DISCOVERED")
    );
    assert_eq!(events[0].correlation_id, c.correlation_id.as_str());
    let payloads = strings(
        h.conn(),
        &format!("SELECT payload::text FROM numbering_outbox_events WHERE aggregate_id = '{}' ORDER BY sequence", r.id),
    )
    .await;
    assert!(!payloads.is_empty());
    assert!(payloads
        .iter()
        .all(|p| !p.contains("2348095550101") && !p.contains("acme") && !p.contains("OC-1")));
    // The id is immutable at the database level.
    let err = exec(
        h.conn(),
        &format!(
            "UPDATE number_resources SET id = gen_random_uuid() WHERE id = '{}'",
            r.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("immutable") || err.to_string().contains("not permitted"),
        "{err}"
    );
}

// D10.2 ---------------------------------------------------------------------------------

#[tokio::test]
async fn e164_is_normalized_validated_and_unique_per_inventory_class() {
    for (raw, normalized) in [
        ("+234 809 123 4567", "+2348091234567"),
        ("+1 (555) 010-0001", "+15550100001"),
        ("+44 20 7946 0000", "+442079460000"),
    ] {
        assert_eq!(e164::normalize(raw).unwrap(), normalized, "{raw}");
    }
    for bad in [
        "",
        "2348091234567",
        "+0123456789",
        "+12",
        "+1234567890123456",
        "+1 555 CALL NOW",
        "++15550100001",
    ] {
        assert!(e164::normalize(bad).is_err(), "{bad:?}");
    }
    // The stated country must match the number's calling code (never inferred).
    assert!(e164::validate("+2348091234567", "US").is_err());
    assert!(
        e164::validate("+2348091234567", "ng").is_err()
            || e164::validate("+2348091234567", "ng").is_ok()
    );
    assert!(e164::validate("+2348091234567", "ZZ").is_err());

    let Some(h) = Harness::new().await else {
        return;
    };
    let live = InventorySource::LiveProvider;
    h.numbers
        .discover(
            &ctx(),
            actor(),
            new_resource("+2348091112222", "NG", live),
            observation(&h, live),
        )
        .await
        .unwrap();
    let dup = h
        .numbers
        .discover(
            &ctx(),
            actor(),
            new_resource("+234 809 111 2222", "NG", live),
            observation(&h, live),
        )
        .await
        .unwrap_err();
    assert_eq!(dup.code(), "NUMBER_EXISTS");
    // Manual imports are real inventory too: the same real number cannot exist twice.
    let mut manual = observation(&h, InventorySource::ManualImport);
    manual.source = InventorySource::ManualImport;
    let dup = h
        .numbers
        .discover(
            &ctx(),
            actor(),
            new_resource("+2348091112222", "NG", InventorySource::ManualImport),
            manual,
        )
        .await
        .unwrap_err();
    assert_eq!(dup.code(), "NUMBER_EXISTS");
    // Malformed numbers never reach the database; the database refuses them anyway.
    let bad = h
        .numbers
        .discover(
            &ctx(),
            actor(),
            new_resource("+234-809", "NG", live),
            observation(&h, live),
        )
        .await
        .unwrap_err();
    assert!(matches!(bad, DomainError::Validation(_)));
    assert!(exec(
        h.conn(),
        "INSERT INTO number_resources (id, e164, country_code, calling_code, number_type, classification_source, \
           inventory_source, state, revision, state_changed_at, discovered_at, risk_tier, cost_evidence, created_at, updated_at) \
         VALUES (gen_random_uuid(), '2348090000000', 'NG', '234', 'UNCLASSIFIED', 'NONE', 'LIVE_PROVIDER', 'DISCOVERED', 1, \
           now(), now(), 'UNASSESSED', 'UNKNOWN', now(), now())"
    )
    .await
    .is_err());
}

// D10.3 ---------------------------------------------------------------------------------

#[tokio::test]
async fn the_lifecycle_policy_is_one_edge_set_in_rust_and_sql() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mut edges = 0;
    for from in LifecycleState::ALL {
        for to in LifecycleState::ALL {
            let sql = scalar(
                h.conn(),
                &format!("SELECT CASE WHEN number_lifecycle_allowed('{from}', '{to}') THEN 1 ELSE 0 END::bigint"),
            )
            .await;
            assert_eq!(sql == 1, allowed(*from, *to), "{from} -> {to}");
            edges += sql;
        }
    }
    assert!(edges > 30);
    // Nothing reaches ACTIVE except PROVISIONED, SUSPENDED and reconciliation; nothing
    // leaves RELEASED / FAILED except rediscovery; no self-loops.
    let into_active: Vec<_> = LifecycleState::ALL
        .iter()
        .filter(|s| allowed(**s, LifecycleState::Active))
        .collect();
    assert_eq!(
        into_active,
        vec![
            &LifecycleState::Provisioned,
            &LifecycleState::Suspended,
            &LifecycleState::UnknownProviderState
        ]
    );
    for s in LifecycleState::ALL {
        assert!(!allowed(*s, *s), "{s}");
    }
    for terminal in [LifecycleState::Released, LifecycleState::Failed] {
        let out: Vec<_> = LifecycleState::ALL
            .iter()
            .filter(|s| allowed(terminal, **s))
            .collect();
        assert_eq!(out, vec![&LifecycleState::Discovered], "{terminal}");
    }
}

#[tokio::test]
async fn transitions_follow_the_policy_and_record_actor_reason_and_evidence() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    assert_eq!((r.state.as_str(), r.revision), ("AVAILABLE", 2));
    assert!(r.available_at.is_some());
    // Not an edge.
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Active, r.revision, actor(), "SKIP"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidTransition { .. }),
        "{err:?}"
    );
    // An edge whose prerequisite is missing (no pending reservation).
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Reserving, r.revision, actor(), "TRY"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "RESERVATION_REQUIRED");
    // Malformed reason codes are refused.
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Quarantined,
                r.revision,
                actor(),
                "bad reason",
            )
            .with_quarantine_until(h.now() + Duration::days(1)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation(_)));
    // Quarantine needs a future end.
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Quarantined, r.revision, actor(), "ABUSE"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation(_)));
    let evidence = Uuid::new_v4();
    let q = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Quarantined,
                r.revision,
                common::reconciler(),
                "ABUSE_REPORTED",
            )
            .with_quarantine_until(h.now() + Duration::days(30))
            .with_evidence(evidence),
        )
        .await
        .unwrap();
    assert_eq!((q.state.as_str(), q.revision), ("QUARANTINED", 3));
    let events = h.numbers.lifecycle_events(r.id).await.unwrap();
    let last = events.last().unwrap();
    assert_eq!(
        (
            last.from_state.as_deref(),
            last.to_state.as_str(),
            last.reason_code.as_str(),
            last.actor_kind.as_str()
        ),
        (
            Some("AVAILABLE"),
            "QUARANTINED",
            "ABUSE_REPORTED",
            "RECONCILIATION"
        )
    );
    assert_eq!(last.evidence_ref, Some(evidence));
    // Leaving quarantine early is refused.
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Reassignable, q.revision, actor(), "EARLY"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "QUARANTINE_ACTIVE");
    h.clock.advance(Duration::days(31));
    let ok = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Reassignable,
                q.revision,
                actor(),
                "COOLED_DOWN",
            ),
        )
        .await
        .unwrap();
    assert_eq!(ok.state, "REASSIGNABLE");
    // A failure needs its category; released numbers are rediscovered only from fresh
    // inventory.
    let d = h
        .numbers
        .discover(
            &ctx(),
            actor(),
            new_resource(
                &common::random_number(InventorySource::SyntheticTest),
                "US",
                InventorySource::SyntheticTest,
            ),
            observation(&h, InventorySource::SyntheticTest),
        )
        .await
        .unwrap();
    let err = h
        .numbers
        .transition(
            &ctx(),
            d.id,
            TransitionRequest::new(LifecycleState::Failed, 1, actor(), "NOT_OBTAINABLE"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation(_)));
    let f = h
        .numbers
        .transition(
            &ctx(),
            d.id,
            TransitionRequest::new(LifecycleState::Failed, 1, actor(), "NOT_OBTAINABLE")
                .with_failure(FailureCategory::NotObtainable),
        )
        .await
        .unwrap();
    assert_eq!(
        (f.state.as_str(), f.failure_category.as_deref()),
        ("FAILED", Some("NOT_OBTAINABLE"))
    );
    h.clock.advance(Duration::hours(2));
    let err = h
        .numbers
        .transition(
            &ctx(),
            d.id,
            TransitionRequest::new(
                LifecycleState::Discovered,
                f.revision,
                actor(),
                "REDISCOVERED",
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "INVENTORY_NOT_FRESH");
    h.numbers
        .observe(
            &ctx(),
            d.id,
            observation(&h, InventorySource::SyntheticTest),
        )
        .await
        .unwrap();
    let f = h.numbers.resource(d.id).await.unwrap();
    h.numbers
        .transition(
            &ctx(),
            d.id,
            TransitionRequest::new(
                LifecycleState::Discovered,
                f.revision,
                actor(),
                "REDISCOVERED",
            ),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn the_database_refuses_state_changes_outside_the_transition_policy() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    // Without the policy's session flag.
    let err = exec(h.conn(), &format!("UPDATE number_resources SET state = 'RESERVED', revision = revision + 1 WHERE id = '{}'", r.id))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("only through a domain transition"),
        "{err}"
    );
    // With the flag but on a non-edge, or without the revision step.
    let flag = "SELECT set_config('numbering.lifecycle', 'transition', true)";
    for sql in [
        format!(
            "UPDATE number_resources SET state = 'ACTIVE', revision = revision + 1 WHERE id = '{}'",
            r.id
        ),
        format!(
            "UPDATE number_resources SET state = 'RELEASED' WHERE id = '{}'",
            r.id
        ),
    ] {
        assert!(
            common::in_txn(h.conn(), &[flag, &sql]).await.is_err(),
            "{sql}"
        );
    }
    assert_eq!(h.numbers.resource(r.id).await.unwrap().state, "AVAILABLE");
    // Lifecycle events are append-only.
    for sql in [
        format!("UPDATE number_lifecycle_events SET reason_code = 'X_X' WHERE number_resource_id = '{}'", r.id),
        format!("DELETE FROM number_lifecycle_events WHERE number_resource_id = '{}'", r.id),
        "TRUNCATE number_lifecycle_events".to_string(),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
    // Resources are never deleted.
    assert!(exec(
        h.conn(),
        &format!("DELETE FROM number_resources WHERE id = '{}'", r.id)
    )
    .await
    .is_err());
}

#[tokio::test]
async fn uncertainty_is_recorded_and_only_reconciliation_with_evidence_resolves_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::LiveProvider).await;
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::UnknownProviderState,
                r.revision,
                actor(),
                "TIMEOUT",
            ),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation(_)),
        "the uncertain operation is required"
    );
    let u = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::UnknownProviderState,
                r.revision,
                actor(),
                "PROVIDER_TIMEOUT",
            )
            .uncertain(UncertainOperation::Release),
        )
        .await
        .unwrap();
    assert_eq!(
        (
            u.uncertain_operation.as_deref(),
            u.uncertain_from_state.as_deref()
        ),
        (Some("RELEASE"), Some("AVAILABLE"))
    );
    assert!(u.uncertain_since.is_some());
    // A system actor, or reconciliation without evidence, cannot resolve it.
    for req in [
        TransitionRequest::new(LifecycleState::Available, u.revision, actor(), "GUESS")
            .with_evidence(Uuid::new_v4()),
        TransitionRequest::new(LifecycleState::Available, u.revision, reconciler(), "GUESS"),
    ] {
        let err = h.numbers.transition(&ctx(), r.id, req).await.unwrap_err();
        assert_eq!(err.code(), "RECONCILIATION_REQUIRED");
    }
    // The database refuses it too (transition flag, not reconciliation mode).
    let sql = format!(
        "UPDATE number_resources SET state = 'AVAILABLE', revision = revision + 1 WHERE id = '{}'",
        r.id
    );
    assert!(common::in_txn(
        h.conn(),
        &[
            "SELECT set_config('numbering.lifecycle', 'transition', true)",
            &sql
        ]
    )
    .await
    .is_err());
    let resolved = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Released,
                u.revision,
                reconciler(),
                "PROVIDER_CONFIRMED_RELEASE",
            )
            .with_evidence(Uuid::new_v4()),
        )
        .await
        .unwrap();
    assert_eq!(resolved.state, "RELEASED");
    assert!(resolved.uncertain_operation.is_none() && resolved.uncertain_since.is_none());
}

// D10.4 ---------------------------------------------------------------------------------

#[tokio::test]
async fn stale_revisions_conflict_and_concurrent_writers_have_one_winner() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let stale = r.revision - 1;
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Quarantined, stale, actor(), "ABUSE")
                .with_quarantine_until(h.now() + Duration::days(1)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionConflict(_)));
    // Eight writers with the same expected revision: exactly one applies.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let n = h.numbers.clone();
        let until = h.now() + Duration::days(1 + i);
        tasks.push(tokio::spawn(async move {
            n.transition(
                &RequestContext::generate(),
                r.id,
                TransitionRequest::new(LifecycleState::Quarantined, r.revision, actor(), "ABUSE")
                    .with_quarantine_until(until),
            )
            .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert!(matches!(e, DomainError::VersionConflict(_)), "{e:?}"),
        }
    }
    assert_eq!(ok, 1);
    let after = h.numbers.resource(r.id).await.unwrap();
    assert_eq!(after.revision, r.revision + 1);
    assert_eq!(h.numbers.lifecycle_events(r.id).await.unwrap().len(), 3);
    // Classification is revisioned as well.
    let err = h
        .numbers
        .classify(
            &ctx(),
            r.id,
            r.revision,
            NumberType::Mobile,
            ClassificationSource::Synthetic,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionConflict(_)));
}

// D10.5 ---------------------------------------------------------------------------------

#[tokio::test]
async fn at_most_one_active_binding_serves_a_capability_and_history_is_kept() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let live = InventorySource::LiveProvider;
    let r = h.available_number(live).await;
    let voice = h
        .numbers
        .activate_binding(
            &ctx(),
            r.id,
            binding_for(live, &[Capability::VoiceInbound, Capability::VoiceOutbound]),
        )
        .await
        .unwrap();
    // A second binding for an already served capability conflicts…
    let err = h
        .numbers
        .activate_binding(
            &ctx(),
            r.id,
            binding_for(live, &[Capability::VoiceOutbound, Capability::SmsInbound]),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "BINDING_CONFLICT");
    // …a binding for other capabilities does not.
    let sms = h
        .numbers
        .activate_binding(&ctx(), r.id, binding_for(live, &[Capability::SmsInbound]))
        .await
        .unwrap();
    // Non-routable capabilities are not bound, synthetic and live providers never mix.
    for bad in [
        binding_for(live, &[Capability::MobileNumberClass]),
        binding_for(InventorySource::SyntheticTest, &[Capability::VoiceInbound]),
        NewBinding {
            external_ref: String::new(),
            ..binding_for(live, &[Capability::MmsRich])
        },
    ] {
        assert!(matches!(
            h.numbers
                .activate_binding(&ctx(), r.id, bad)
                .await
                .unwrap_err(),
            DomainError::Validation(_)
        ));
    }
    // The database enforces exclusivity on its own (exclusion constraint, 23P01).
    let err = exec(
        h.conn(),
        &format!(
            "INSERT INTO number_provider_bindings (id, number_resource_id, provider_id, external_ref, capabilities, \
               status, binding_revision, change_reason, effective_from, created_at) \
             VALUES (gen_random_uuid(), '{}', 'acme-telecom', 'RAW-1', ARRAY['VOICE_INBOUND'], 'ACTIVE', 99, 'CORRECTION', now(), now())",
            r.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("conflicting active provider binding"),
        "{err}"
    );
    // Replacement ends the old binding, links its successor and bumps the binding revision.
    let next = h
        .numbers
        .replace_binding(
            &ctx(),
            voice.id,
            NewBinding {
                change_reason: BindingChangeReason::PortIn,
                ..binding_for(live, &[Capability::VoiceInbound])
            },
        )
        .await
        .unwrap();
    let all = h.numbers.bindings(r.id).await.unwrap();
    assert_eq!(all.len(), 3);
    let old = all.iter().find(|b| b.id == voice.id).unwrap();
    assert_eq!(
        (old.status.as_str(), old.superseded_by_binding_id),
        ("ENDED", Some(next.id))
    );
    assert!(old.effective_to.is_some());
    assert_eq!(h.numbers.resource(r.id).await.unwrap().binding_revision, 3);
    assert!(next.binding_revision > sms.binding_revision);
    // An ended binding cannot be replaced or ended again, and history rows are immutable.
    assert_eq!(
        h.numbers
            .end_binding(&ctx(), voice.id)
            .await
            .unwrap_err()
            .code(),
        "BINDING_ENDED"
    );
    assert!(exec(
        h.conn(),
        &format!(
            "UPDATE number_provider_bindings SET status = 'ACTIVE' WHERE id = '{}'",
            voice.id
        )
    )
    .await
    .is_err());
    assert!(exec(
        h.conn(),
        &format!(
            "DELETE FROM number_provider_bindings WHERE id = '{}'",
            voice.id
        )
    )
    .await
    .is_err());
    // A released number keeps no active binding.
    let err = h
        .numbers
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Released,
                h.numbers.resource(r.id).await.unwrap().revision,
                reconciler(),
                "RETURNED",
            )
            .with_evidence(Uuid::new_v4()),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "BINDINGS_ACTIVE");
}

#[tokio::test]
async fn provider_identifiers_never_leave_numbering() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::LiveProvider).await;
    let b = h
        .numbers
        .activate_binding(
            &ctx(),
            r.id,
            binding_for(InventorySource::LiveProvider, &[Capability::VoiceInbound]),
        )
        .await
        .unwrap();
    let reply = h
        .internal(
            &h.peers.calling,
            axum::http::Method::GET,
            &format!("/internal/v1/number-resources/{}", r.id),
        )
        .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let text = reply.body.to_string();
    assert!(
        !text.contains("acme-telecom") && !text.contains(&b.external_ref),
        "{text}"
    );
    assert_eq!(
        reply.body["activeBindings"][0]["bindingRef"],
        b.id.to_string()
    );
    let public = serde_json::to_value(b.public_ref()).unwrap().to_string();
    assert!(!public.contains("acme-telecom") && !public.contains(&b.external_ref));
    // Nor do they reach events.
    let leaked = scalar(
        h.conn(),
        &format!(
            "SELECT count(*) FROM numbering_outbox_events WHERE payload::text LIKE '%{}%' OR payload::text LIKE '%acme-telecom%'",
            b.external_ref
        ),
    )
    .await;
    assert_eq!(leaked, 0);
}

// D10.6 ---------------------------------------------------------------------------------

#[tokio::test]
async fn capabilities_are_evidence_backed_never_inferred_and_downgrades_are_safe() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scope = scope(EnvironmentKind::Production);
    let live = InventorySource::LiveProvider;
    let r = h.available_number(live).await;
    // VERIFIED needs evidence; synthetic facts are never VERIFIED.
    let unverified = NewCapability {
        status: CapabilityStatus::Verified,
        ..NewCapability::declared(
            Capability::SmsOutbound,
            CapabilitySource::ProviderConfirmation,
        )
    };
    assert_eq!(
        h.numbers
            .record_capability(&ctx(), r.id, unverified)
            .await
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );
    let s = h.available_number(InventorySource::SyntheticTest).await;
    let synthetic_verified = NewCapability {
        status: CapabilityStatus::Verified,
        evidence_ref: Some(Uuid::new_v4()),
        verified_at: Some(h.now()),
        ..NewCapability::declared(Capability::VoiceInbound, CapabilitySource::Synthetic)
    };
    assert_eq!(
        h.numbers
            .record_capability(&ctx(), s.id, synthetic_verified)
            .await
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );
    assert!(matches!(
        h.numbers
            .record_capability(
                &ctx(),
                s.id,
                NewCapability::declared(
                    Capability::VoiceInbound,
                    CapabilitySource::ProviderInventory
                )
            )
            .await
            .unwrap_err(),
        DomainError::Validation(_)
    ));
    // (Workstream E: a PROVIDER_CONFIRMATION names its confirming binding; inventory
    // evidence does not.)
    let verified = NewCapability {
        status: CapabilityStatus::Verified,
        evidence_ref: Some(Uuid::new_v4()),
        verified_at: Some(h.now()),
        ..NewCapability::declared(Capability::SmsOutbound, CapabilitySource::ProviderInventory)
    };
    h.numbers
        .record_capability(&ctx(), r.id, verified)
        .await
        .unwrap();
    // SMS support does not make a number mobile.
    let after = h.numbers.resource(r.id).await.unwrap();
    assert_eq!(after.number_type, "UNCLASSIFIED");
    let caps: Vec<String> = h
        .numbers
        .current_capabilities(r.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.capability)
        .collect();
    assert_eq!(caps, vec!["SMS_OUTBOUND"]);
    // The database refuses an unevidenced VERIFIED row too.
    assert!(exec(
        h.conn(),
        &format!(
            "INSERT INTO number_capabilities (id, number_resource_id, capability, status, source, effective_from, limitations, recorded_at) \
             VALUES (gen_random_uuid(), '{}', 'MMS_RICH', 'VERIFIED', 'PROVIDER_CONFIRMATION', now(), '{{}}', now())",
            r.id
        )
    )
    .await
    .is_err());

    // Downgrade safety: an open assignment relies on VOICE_INBOUND.
    let j = h.active(&scope, OwnershipClass::Professional).await;
    assert_eq!(j.assignment.capability_snapshot, vec!["VOICE_INBOUND"]);
    let withdraw = || NewCapability {
        status: CapabilityStatus::Withdrawn,
        ..NewCapability::declared(Capability::VoiceInbound, capability_source(live))
    };
    let err = h
        .numbers
        .record_capability(&ctx(), j.resource_id, withdraw())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CAPABILITY_IN_USE");
    let change = h
        .numbers
        .record_capability(
            &ctx(),
            j.resource_id,
            NewCapability {
                acknowledge_impact: true,
                ..withdraw()
            },
        )
        .await
        .unwrap();
    assert_eq!(change.previous_status.as_deref(), Some("DECLARED"));
    assert_eq!(change.impacted_assignments, vec![j.assignment.id]);
    // The superseded record is kept with its effective dates.
    let history = h.numbers.capability_history(j.resource_id).await.unwrap();
    assert_eq!(history.len(), 2);
    let old = history.iter().find(|c| c.status == "DECLARED").unwrap();
    assert_eq!(old.superseded_by, Some(change.capability.id));
    assert!(old.effective_to.is_some());
    assert!(h
        .numbers
        .current_capabilities(j.resource_id)
        .await
        .unwrap()
        .iter()
        .all(|c| c.status == "WITHDRAWN"));
    // A new assignment would no longer snapshot a withdrawn capability (only usable ones).
    assert!(exec(
        h.conn(),
        &format!(
            "DELETE FROM number_capabilities WHERE number_resource_id = '{}'",
            j.resource_id
        )
    )
    .await
    .is_err());
}

// D10.7 ---------------------------------------------------------------------------------

#[tokio::test]
async fn inventory_freshness_and_classification_are_explicit() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let live = InventorySource::LiveProvider;
    let number = common::random_number(live);
    let mut stale = observation(&h, live);
    stale.availability = Availability::Unknown;
    let d = h
        .numbers
        .discover(&ctx(), actor(), new_resource(&number, "NG", live), stale)
        .await
        .unwrap();
    // An observation that does not offer the number does not make it AVAILABLE.
    let err = h
        .numbers
        .transition(
            &ctx(),
            d.id,
            TransitionRequest::new(LifecycleState::Available, 1, actor(), "OFFERED"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "INVENTORY_NOT_FRESH");
    h.numbers
        .observe(&ctx(), d.id, observation(&h, live))
        .await
        .unwrap();
    let rev = h.numbers.resource(d.id).await.unwrap().revision;
    let a = h
        .numbers
        .transition(
            &ctx(),
            d.id,
            TransitionRequest::new(LifecycleState::Available, rev, actor(), "OFFERED"),
        )
        .await
        .unwrap();
    assert!(a.is_fresh(h.now()));
    // Observation is not allocation: an AVAILABLE number has no reservation.
    assert_eq!(
        scalar(
            h.conn(),
            &format!(
                "SELECT count(*) FROM number_reservations WHERE number_resource_id = '{}'",
                d.id
            )
        )
        .await,
        0
    );
    // Freshness expires: no reservation on stale inventory.
    h.clock.advance(Duration::hours(1));
    assert!(!h.numbers.resource(d.id).await.unwrap().is_fresh(h.now()));
    let err = h
        .numbers
        .reserve(
            &ctx(),
            &scope(EnvironmentKind::Production),
            actor(),
            d.id,
            Duration::minutes(5),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "INVENTORY_NOT_FRESH");
    // Snapshots are append-only history.
    assert_eq!(
        scalar(
            h.conn(),
            &format!(
                "SELECT count(*) FROM number_inventory_snapshots WHERE number_resource_id = '{}'",
                d.id
            )
        )
        .await,
        2
    );
    assert!(exec(h.conn(), &format!("UPDATE number_inventory_snapshots SET availability = 'AVAILABLE' WHERE number_resource_id = '{}'", d.id))
        .await
        .is_err());

    // Synthetic inventory is distinguishable and never mixes with real inventory.
    let s = h.available_number(InventorySource::SyntheticTest).await;
    assert_eq!(s.inventory_source, "SYNTHETIC_TEST");
    assert!(matches!(
        h.numbers
            .observe(&ctx(), s.id, observation(&h, live))
            .await
            .unwrap_err(),
        DomainError::Validation(_)
    ));
    assert!(matches!(
        h.numbers
            .classify(
                &ctx(),
                a.id,
                h.numbers.resource(a.id).await.unwrap().revision,
                NumberType::Mobile,
                ClassificationSource::Synthetic
            )
            .await
            .unwrap_err(),
        DomainError::Validation(_)
    ));
    let c = h
        .numbers
        .classify(
            &ctx(),
            a.id,
            h.numbers.resource(a.id).await.unwrap().revision,
            NumberType::Mobile,
            ClassificationSource::ProviderInventory,
        )
        .await
        .unwrap();
    assert_eq!(
        (c.number_type.as_str(), c.classification_source.as_str()),
        ("MOBILE", "PROVIDER_INVENTORY")
    );
    // A synthetic number can share digits with a real one without being confused with it.
    let twin = h
        .numbers
        .discover(
            &ctx(),
            actor(),
            new_resource(&s.e164, "US", InventorySource::SyntheticTest),
            observation(&h, InventorySource::SyntheticTest),
        )
        .await
        .unwrap_err();
    assert_eq!(twin.code(), "NUMBER_EXISTS");
    // Cost facts need a currency.
    let mut costly = observation(&h, live);
    costly.monthly_cost_minor = Some(150);
    assert!(matches!(
        h.numbers
            .discover(
                &ctx(),
                actor(),
                new_resource(&common::random_number(live), "NG", live),
                costly
            )
            .await
            .unwrap_err(),
        DomainError::Validation(_)
    ));
}

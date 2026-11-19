//! HPKE sealing (round trip only with the `open` feature, which the service enables) and
//! the routing fence helper (Document 4 §9.3, §9.4, §9.8).

use cf_notification_client::{
    routing::{
        canary, critical_reroute, decide, legacy_insert_allowed, CriticalDecisionState,
        CriticalFallback, DecisionReason, ExistingDecision, RerouteVerdict, Route, RoutingMode,
        RoutingState,
    },
    seal::{generate_keypair, seal, valid_kid, SealError, SealedPlaintext, SealingKey},
};
use uuid::Uuid;

fn plaintext() -> SealedPlaintext {
    SealedPlaintext::default().with_code("482913")
}

#[test]
fn key_ids_and_plaintexts_are_checked() {
    for ok in ["notif-hpke-2026-11", "notif-hpke-2027-01-b2"] {
        assert!(valid_kid(ok), "{ok}");
    }
    for bad in [
        "notif-hpke-26-11",
        "hpke-2026-11",
        "notif-hpke-2026-11-",
        "notif-hpke-2026-11-UPPER",
        "notif-hpke-2026-11-toolongsuffix",
    ] {
        assert!(!valid_kid(bad), "{bad}");
        assert_eq!(generate_keypair(bad).unwrap_err(), SealError::InvalidKid);
    }
    let (public, _) = generate_keypair("notif-hpke-2026-11").unwrap();
    let key = SealingKey::parse(&public).unwrap();
    assert_eq!(key.kid(), "notif-hpke-2026-11");
    assert_eq!(key.public_bytes().len(), 32);
    assert!(SealingKey::parse("notif-hpke-2026-11:not base64").is_err());
    for bad in [
        SealedPlaintext::default(),
        SealedPlaintext::default().with_code("12"),
        SealedPlaintext::default().with_token("short"),
        SealedPlaintext::default().with_address("no-at-sign"),
    ] {
        assert_eq!(
            seal(&key, "p", "e", "t", &bad).unwrap_err(),
            SealError::InvalidPlaintext
        );
    }
}

#[test]
fn sealed_plaintext_never_prints_its_secrets() {
    let p = SealedPlaintext {
        code: Some("482913".into()),
        token: Some("tok_0123456789abcdefghij".into()),
        address: Some("invitee@example.com".into()),
        masked_address: Some("i***@example.com".into()),
        request_client: Some("TestBrowser/1.0".into()),
        request_network: Some("102.89.x.x".into()),
    };
    let shown = format!("{p:?}");
    for secret in [
        "482913",
        "tok_0123456789abcdefghij",
        "invitee@example.com",
        "i***@example.com",
        "TestBrowser",
        "102.89.x.x",
    ] {
        assert!(!shown.contains(secret), "{shown}");
    }
    assert!(shown.contains("[REDACTED]"));
}

#[test]
fn sealing_is_randomized_and_bound_to_the_key_id() {
    let (public, _) = generate_keypair("notif-hpke-2026-11").unwrap();
    let key = SealingKey::parse(&public).unwrap();
    let a = seal(
        &key,
        "cfmeet.meeting-service",
        "evt_1",
        "account.verification-code.requested",
        &plaintext(),
    )
    .unwrap();
    let b = seal(
        &key,
        "cfmeet.meeting-service",
        "evt_1",
        "account.verification-code.requested",
        &plaintext(),
    )
    .unwrap();
    assert_ne!(a.enc, b.enc);
    assert_ne!(a.ct, b.ct);
    assert_eq!(a.kid, "notif-hpke-2026-11");
    assert_eq!(a.enc.len(), 43);
    assert!(!a.ct.contains("482913"));
}

#[cfg(feature = "open")]
mod open {
    use super::*;
    use cf_notification_client::seal::{open, OpeningKey};

    fn pair(kid: &str) -> (SealingKey, OpeningKey) {
        let (public, private) = generate_keypair(kid).unwrap();
        (
            SealingKey::parse(&public).unwrap(),
            OpeningKey::from_bytes(kid, &private).unwrap(),
        )
    }

    const P: &str = "cfmeet.meeting-service";
    const T: &str = "account.verification-code.requested";

    #[test]
    fn the_service_opens_what_a_producer_sealed() {
        let (pk, sk) = pair("notif-hpke-2026-11");
        let full = SealedPlaintext {
            code: None,
            token: Some("tok_0123456789abcdefghij".into()),
            address: Some("invitee@example.com".into()),
            masked_address: Some("n***@example.com".into()),
            request_client: Some("TestBrowser/1.0".into()),
            request_network: Some("2001:db8:x:x".into()),
        };
        let sealed = seal(&pk, P, "evt_abc", T, &full).unwrap();
        assert_eq!(open(&sk, P, "evt_abc", T, &sealed).unwrap(), full);
    }

    #[test]
    fn a_sealed_part_moved_to_another_envelope_or_tampered_does_not_open() {
        let (pk, sk) = pair("notif-hpke-2026-11");
        let sealed = seal(&pk, P, "evt_abc", T, &plaintext()).unwrap();
        // Bound to producer, event id and event type (AAD).
        assert_eq!(
            open(&sk, "cfmeet.organizations", "evt_abc", T, &sealed).unwrap_err(),
            SealError::OpenFailed
        );
        assert_eq!(
            open(&sk, P, "evt_other", T, &sealed).unwrap_err(),
            SealError::OpenFailed
        );
        assert_eq!(
            open(
                &sk,
                P,
                "evt_abc",
                "account.password-reset-code.requested",
                &sealed
            )
            .unwrap_err(),
            SealError::OpenFailed
        );
        // Tampered ciphertext / encapsulated key.
        let mut t = sealed.clone();
        let flip = if t.ct.starts_with('A') { "B" } else { "A" };
        t.ct.replace_range(0..1, flip);
        assert_eq!(
            open(&sk, P, "evt_abc", T, &t).unwrap_err(),
            SealError::OpenFailed
        );
        let mut t = sealed.clone();
        t.enc = "A".repeat(43);
        assert!(open(&sk, P, "evt_abc", T, &t).is_err());
        // Another service key (same kid) cannot open; a different kid is refused early.
        let (_, other) = pair("notif-hpke-2026-11");
        assert_eq!(
            open(&other, P, "evt_abc", T, &sealed).unwrap_err(),
            SealError::OpenFailed
        );
        let (_, rotated) = pair("notif-hpke-2026-12");
        assert_eq!(
            open(&rotated, P, "evt_abc", T, &sealed).unwrap_err(),
            SealError::WrongKid
        );
    }
    /// ADR 0063 N-P1: a partner push credential opens only for the scope it was sealed for.
    #[test]
    fn push_credentials_open_only_for_their_scope() {
        use cf_notification_client::seal::{open_push_credential, seal_push_credential};
        let (pk, sk) = pair("notif-hpke-2026-11");
        let (t, a, e) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let secret = "{\"type\":\"service_account\",\"project_id\":\"demo\"}";
        let sealed = seal_push_credential(&pk, t, a, e, secret).unwrap();
        assert_eq!(
            open_push_credential(&sk, t, a, e, &sealed)
                .unwrap()
                .as_str(),
            secret
        );
        for (t2, a2, e2) in [
            (Uuid::new_v4(), a, e),
            (t, Uuid::new_v4(), e),
            (t, a, Uuid::new_v4()),
        ] {
            assert_eq!(
                open_push_credential(&sk, t2, a2, e2, &sealed).unwrap_err(),
                SealError::OpenFailed
            );
        }
        // An event part never opens as a credential (domain-separated info / AAD).
        let part = seal(&pk, P, "evt_abc", T, &plaintext()).unwrap();
        assert!(open_push_credential(&sk, t, a, e, &part).is_err());
    }
}

fn state(mode: RoutingMode, canary_percent: u8, fallback: CriticalFallback) -> RoutingState {
    RoutingState {
        mode,
        canary_percent,
        critical_fallback: fallback,
        routing_version: 9,
        ..RoutingState::legacy_only("SECURITY")
    }
}

#[test]
fn decisions_follow_the_mode_and_are_sticky() {
    let id = Uuid::new_v4();
    for (mode, expected, shadow) in [
        (RoutingMode::LegacyOnly, Route::Old, false),
        (RoutingMode::Shadow, Route::Old, true),
        (RoutingMode::FencedOld, Route::Old, false),
        (RoutingMode::FencedNew, Route::New, false),
    ] {
        let d = decide(
            "otp:verify:1",
            &state(mode, 100, CriticalFallback::Auto),
            true,
            None,
            id,
        );
        assert_eq!(
            (d.route, d.shadow, d.sticky, d.routing_version, d.reason),
            (expected, shadow, false, 9, DecisionReason::Routed),
            "{mode:?}"
        );
        assert_eq!(d.decision_id, id);
    }
    // FENCED_NEW with 0 % canary routes OLD.
    assert_eq!(
        decide(
            "k",
            &state(RoutingMode::FencedNew, 0, CriticalFallback::Auto),
            false,
            None,
            id
        )
        .route,
        Route::Old
    );
    // A retry follows the earlier decision whatever the mode now says (sticky, writes nothing).
    let earlier = ExistingDecision {
        decision_id: Uuid::new_v4(),
        route: Route::New,
        routing_version: 3,
    };
    let d = decide(
        "otp:verify:1",
        &state(RoutingMode::FencedOld, 0, CriticalFallback::Forced),
        true,
        Some(&earlier),
        id,
    );
    assert_eq!(
        (d.route, d.sticky, d.decision_id, d.routing_version),
        (Route::New, true, earlier.decision_id, 3)
    );
    // The route reference carries protocol 1.
    assert_eq!(d.route_ref().protocol, 1);
}

#[test]
fn forced_fallback_routes_new_critical_decisions_old_only() {
    let s = state(RoutingMode::FencedNew, 100, CriticalFallback::Forced);
    assert_eq!(
        decide("otp:verify:1", &s, true, None, Uuid::new_v4()).route,
        Route::Old
    );
    assert_eq!(
        decide("meeting-notification:1", &s, false, None, Uuid::new_v4()).route,
        Route::New
    );
}

#[test]
fn the_canary_is_a_deterministic_function_of_the_key() {
    for k in ["otp:verify:a", "meeting-notification:b", "evt_c"] {
        assert_eq!(canary(k, 37), canary(k, 37));
        assert!(!canary(k, 0));
        assert!(canary(k, 100));
    }
    // Monotonic in the percentage, and roughly proportional.
    let keys: Vec<String> = (0..10_000).map(|i| format!("otp:verify:{i}")).collect();
    for p in [10u8, 50] {
        let n = keys.iter().filter(|k| canary(k, p)).count();
        let expected = usize::from(p) * 100;
        assert!(n.abs_diff(expected) < 300, "{p}% -> {n}");
        assert!(keys.iter().all(|k| !canary(k, p) || canary(k, p + 1)));
    }
}

#[test]
fn the_guard_trigger_rule_is_inert_only_in_legacy_only() {
    assert!(legacy_insert_allowed(RoutingMode::LegacyOnly, None));
    for mode in [
        RoutingMode::Shadow,
        RoutingMode::FencedOld,
        RoutingMode::FencedNew,
    ] {
        assert!(
            !legacy_insert_allowed(mode, None),
            "{mode:?}: pre-fence binary write must raise"
        );
        assert!(!legacy_insert_allowed(mode, Some(Route::New)), "{mode:?}");
        assert!(legacy_insert_allowed(mode, Some(Route::Old)), "{mode:?}");
    }
}

#[test]
fn critical_reroutes_follow_the_lease_rules() {
    let now = 1_000;
    let base = CriticalDecisionState {
        route: Route::New,
        lease_count: 0,
        send_lease_until: None,
        completed: false,
        code_expired: false,
    };
    // Never leased: AUTO failover or manual reroute, zero duplicate risk.
    assert_eq!(
        critical_reroute(&base, now, true, false),
        RerouteVerdict::Allowed(DecisionReason::AutoFailover)
    );
    assert_eq!(
        critical_reroute(&base, now, false, false),
        RerouteVerdict::Allowed(DecisionReason::ReroutedCriticalLease)
    );
    // Live lease: wait.
    let live = CriticalDecisionState {
        lease_count: 1,
        send_lease_until: Some(now + 10),
        ..base
    };
    assert_eq!(
        critical_reroute(&live, now, true, true),
        RerouteVerdict::WaitForLease
    );
    // Completed, already OLD or expired code: never.
    for s in [
        CriticalDecisionState {
            completed: true,
            lease_count: 1,
            ..base
        },
        CriticalDecisionState {
            route: Route::Old,
            ..base
        },
        CriticalDecisionState {
            code_expired: true,
            ..base
        },
    ] {
        assert_eq!(
            critical_reroute(&s, now, false, true),
            RerouteVerdict::Refused
        );
    }
    // Leased, expired, not completed: never automatic; FORCED manual only, recorded.
    let lapsed = CriticalDecisionState {
        lease_count: 2,
        send_lease_until: Some(now - 1),
        ..base
    };
    assert_eq!(
        critical_reroute(&lapsed, now, true, true),
        RerouteVerdict::Refused
    );
    assert_eq!(
        critical_reroute(&lapsed, now, false, false),
        RerouteVerdict::Refused
    );
    assert_eq!(
        critical_reroute(&lapsed, now, false, true),
        RerouteVerdict::Allowed(DecisionReason::ReroutedPossibleDuplicate)
    );
}

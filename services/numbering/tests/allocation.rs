//! Part IV D10.8–D10.17: reservations, DCN assignments, ownership, isolation, history,
//! routing revisions, activation and allocation safety — at the domain AND the database
//! level.
mod common;

use std::collections::HashSet;

use chrono::Duration;
use common::{
    actor, binding_for, eligible_decision, exec, in_txn, reconciler, scope, voice_to_principal,
    Harness,
};
use numbering::{
    domain::RequestContext,
    numbers::{
        assignments::{allowed as assignment_allowed, NewAssignment},
        lifecycle::TransitionRequest,
        routing::RoutingTargets,
        Actor, AssignmentStatus, Capability, DomainError, EnvironmentKind, InventorySource,
        LifecycleState, OwnershipClass, SmsTargetKind, UncertainOperation, VoiceTargetKind,
    },
};
use uuid::Uuid;

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn assignment_for(resource: Uuid, class: OwnershipClass) -> NewAssignment {
    NewAssignment {
        number_resource_id: resource,
        ownership_class: class,
        owner_ref: Uuid::new_v4(),
        policy_snapshot_ref: None,
        kyc_profile_ref: None,
    }
}

// D10.8 ---------------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_reservations_of_one_number_have_exactly_one_winner() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let n = h.numbers.clone();
        // Different customers race for the same number.
        let s = scope(EnvironmentKind::Sandbox);
        tasks.push(tokio::spawn(async move {
            n.reserve(
                &RequestContext::generate(),
                &s,
                actor(),
                r.id,
                Duration::minutes(5),
                None,
            )
            .await
        }));
    }
    let mut winners = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(res) => {
                winners += 1;
                assert_eq!((res.status.as_str(), res.revision), ("PENDING", 1));
            }
            Err(e) => assert_eq!(e.code(), "RESERVATION_EXISTS", "{e:?}"),
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(h.numbers.resource(r.id).await.unwrap().state, "RESERVING");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM number_reservations WHERE number_resource_id = '{}'",
            r.id
        ))
        .await,
        1
    );
    // The database's unique index refuses a second current reservation on its own.
    let err = exec(
        h.conn(),
        &format!(
            "INSERT INTO number_reservations (id, number_resource_id, tenant_id, application_id, environment_id, \
               environment_kind, service_account_id, status, revision, expires_at, correlation_id, created_at, updated_at) \
             VALUES (gen_random_uuid(), '{}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'SANDBOX', \
               gen_random_uuid(), 'PENDING', 1, now() + interval '1 hour', 'corr_abcdefgh', now(), now())",
            r.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("cannot be reserved")
            || err.to_string().contains("uq_number_reservations_current"),
        "{err}"
    );
    // Reservation durations are bounded.
    let other = h.available_number(InventorySource::SyntheticTest).await;
    for ttl in [Duration::seconds(59), Duration::hours(25)] {
        assert!(matches!(
            h.numbers
                .reserve(
                    &ctx(),
                    &scope(EnvironmentKind::Sandbox),
                    actor(),
                    other.id,
                    ttl,
                    None
                )
                .await
                .unwrap_err(),
            DomainError::Validation(_)
        ));
    }
}

// D10.9 ---------------------------------------------------------------------------------

#[tokio::test]
async fn reservation_expiry_is_exclusive_at_expires_at_and_never_implies_a_provider_release() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let sandbox = scope(EnvironmentKind::Sandbox);
    let production = scope(EnvironmentKind::Production);
    let ttl = Duration::minutes(10);

    // Boundary: one microsecond before expiry it is current; at expires_at it is expired.
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let res = h
        .numbers
        .reserve(&ctx(), &sandbox, actor(), r.id, ttl, None)
        .await
        .unwrap();
    assert_eq!(res.expires_at, h.now() + ttl);
    h.clock.set(res.expires_at - Duration::microseconds(1));
    assert!(!res.is_expired_at(h.now()));
    assert_eq!(
        h.numbers
            .expire_reservation(&ctx(), actor(), res.id)
            .await
            .unwrap_err()
            .code(),
        "NOT_EXPIRED"
    );
    h.clock.set(res.expires_at);
    assert!(res.is_expired_at(h.now()));
    assert_eq!(
        h.numbers
            .confirm_hold(&ctx(), actor(), res.id, Uuid::new_v4())
            .await
            .unwrap_err()
            .code(),
        "RESERVATION_EXPIRED"
    );
    // Synthetic inventory needs no provider release: back to AVAILABLE.
    let expired = h
        .numbers
        .expire_reservation(&ctx(), actor(), res.id)
        .await
        .unwrap();
    assert_eq!(
        (
            expired.status.as_str(),
            expired.provider_release_state.as_str()
        ),
        ("EXPIRED", "NOT_REQUIRED")
    );
    assert!(expired.ended_at.is_some());
    assert_eq!(h.numbers.resource(r.id).await.unwrap().state, "AVAILABLE");
    // Final: an expired reservation cannot be revived (domain and database).
    assert!(h
        .numbers
        .expire_reservation(&ctx(), actor(), res.id)
        .await
        .is_err());
    assert!(exec(h.conn(), &format!("UPDATE number_reservations SET status = 'HELD', revision = revision + 1 WHERE id = '{}'", res.id))
        .await
        .is_err());

    // A held LIVE number stays RESERVED after expiry until release evidence arrives.
    let (held_resource, held) = h.held(&production).await;
    h.clock.advance(Duration::minutes(11));
    let expired = h
        .numbers
        .expire_reservation(&ctx(), actor(), held.id)
        .await
        .unwrap();
    assert_eq!(
        (
            expired.status.as_str(),
            expired.provider_release_state.as_str()
        ),
        ("EXPIRED", "PENDING")
    );
    assert_eq!(
        h.numbers.resource(held_resource.id).await.unwrap().state,
        "RESERVED"
    );
    // …and is therefore not allocatable to anyone else meanwhile.
    let err = h
        .numbers
        .reserve(
            &ctx(),
            &scope(EnvironmentKind::Production),
            actor(),
            held_resource.id,
            ttl,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "RESERVATION_EXISTS");
    let released = h
        .numbers
        .confirm_reservation_release(&ctx(), actor(), held.id, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(released.provider_release_state, "CONFIRMED");
    assert!(released.release_evidence_ref.is_some());
    assert_eq!(
        h.numbers.resource(held_resource.id).await.unwrap().state,
        "AVAILABLE"
    );

    // A PENDING live reservation may have reached the provider: expiry makes it uncertain.
    let live = h.available_number(InventorySource::LiveProvider).await;
    let pending = h
        .numbers
        .reserve(&ctx(), &production, actor(), live.id, ttl, None)
        .await
        .unwrap();
    h.clock.advance(ttl);
    let expired = h
        .numbers
        .expire_reservation(&ctx(), actor(), pending.id)
        .await
        .unwrap();
    assert_eq!(expired.provider_release_state, "UNKNOWN");
    let uncertain = h.numbers.resource(live.id).await.unwrap();
    assert_eq!(
        (
            uncertain.state.as_str(),
            uncertain.uncertain_operation.as_deref()
        ),
        ("UNKNOWN_PROVIDER_STATE", Some("RESERVE"))
    );
}

#[tokio::test]
async fn cancellation_and_provider_refusals_follow_the_same_release_rules() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let production = scope(EnvironmentKind::Production);
    let live = h.available_number(InventorySource::LiveProvider).await;
    let pending = h
        .numbers
        .reserve(
            &ctx(),
            &production,
            actor(),
            live.id,
            Duration::minutes(5),
            None,
        )
        .await
        .unwrap();
    // Only the owning scope can cancel; others see nothing.
    let err = h
        .numbers
        .cancel_reservation(
            &ctx(),
            &scope(EnvironmentKind::Production),
            actor(),
            pending.id,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound(_)));
    // A refused hold returns the number without uncertainty.
    let failed = h
        .numbers
        .fail_reservation(&ctx(), actor(), pending.id)
        .await
        .unwrap();
    assert_eq!(
        (
            failed.status.as_str(),
            failed.provider_release_state.as_str()
        ),
        ("FAILED", "NOT_REQUIRED")
    );
    assert_eq!(
        h.numbers.resource(live.id).await.unwrap().state,
        "AVAILABLE"
    );
    // A hold whose outcome is unknown parks the reservation AND the number.
    let again = h
        .numbers
        .reserve(
            &ctx(),
            &production,
            actor(),
            live.id,
            Duration::minutes(5),
            None,
        )
        .await
        .unwrap();
    let unknown = h
        .numbers
        .mark_reservation_unknown(&ctx(), actor(), again.id)
        .await
        .unwrap();
    assert_eq!(unknown.status, "UNKNOWN");
    assert_eq!(
        h.numbers.resource(live.id).await.unwrap().state,
        "UNKNOWN_PROVIDER_STATE"
    );
    // Cancelling a held live reservation keeps the number RESERVED (release pending).
    let (r, held) = h.held(&production).await;
    let cancelled = h
        .numbers
        .cancel_reservation(&ctx(), &production, actor(), held.id)
        .await
        .unwrap();
    assert_eq!(
        (
            cancelled.status.as_str(),
            cancelled.provider_release_state.as_str()
        ),
        ("CANCELLED", "PENDING")
    );
    assert_eq!(h.numbers.resource(r.id).await.unwrap().state, "RESERVED");
}

// D10.10 --------------------------------------------------------------------------------

#[tokio::test]
async fn ownership_follows_the_class_and_never_changes_on_a_record() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    for class in OwnershipClass::ALL {
        let r = h.available_number(InventorySource::SyntheticTest).await;
        let a = h
            .numbers
            .request_assignment(&ctx(), &s, actor(), assignment_for(r.id, *class))
            .await
            .unwrap();
        let expected = if matches!(
            class,
            OwnershipClass::Personal | OwnershipClass::Professional
        ) {
            "PRINCIPAL"
        } else {
            "ORGANIZATION"
        };
        assert_eq!(a.owner_kind, expected, "{class}");
        assert_eq!((a.status.as_str(), a.revision), ("REQUESTED", 1));
        assert!(a.in_scope(&s));
    }
    // An employee-routed number stays organizational, also at the database level.
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let a = h
        .numbers
        .request_assignment(
            &ctx(),
            &s,
            actor(),
            assignment_for(r.id, OwnershipClass::EmployeeDirect),
        )
        .await
        .unwrap();
    assert_eq!(a.owner_kind, "ORGANIZATION");
    for sql in [
        format!("UPDATE dcn_assignments SET owner_kind = 'PRINCIPAL', revision = revision + 1 WHERE id = '{}'", a.id),
        format!("UPDATE dcn_assignments SET owner_ref = gen_random_uuid(), revision = revision + 1 WHERE id = '{}'", a.id),
        format!("UPDATE dcn_assignments SET tenant_id = gen_random_uuid(), revision = revision + 1 WHERE id = '{}'", a.id),
        format!("UPDATE dcn_assignments SET ownership_class = 'PERSONAL', revision = revision + 1 WHERE id = '{}'", a.id),
        format!("DELETE FROM dcn_assignments WHERE id = '{}'", a.id),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
    let other = h.available_number(InventorySource::SyntheticTest).await;
    let err = exec(
        h.conn(),
        &format!(
            "INSERT INTO dcn_assignments (id, number_resource_id, tenant_id, application_id, environment_id, environment_kind, \
               owner_kind, owner_ref, ownership_class, status, revision, requested_at, created_by, updated_at) \
             VALUES (gen_random_uuid(), '{}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'SANDBOX', \
               'PRINCIPAL', gen_random_uuid(), 'EMPLOYEE_DIRECT', 'REQUESTED', 1, now(), 'raw', now())",
            other.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("ck_dcn_assignments_owner_class"),
        "{err}"
    );
}

// D10.11 --------------------------------------------------------------------------------

#[tokio::test]
async fn one_open_assignment_per_number_even_under_contention() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let n = h.numbers.clone();
        tasks.push(tokio::spawn(async move {
            n.request_assignment(
                &RequestContext::generate(),
                &s,
                actor(),
                assignment_for(r.id, OwnershipClass::Organization),
            )
            .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(e.code(), "ASSIGNMENT_EXISTS", "{e:?}"),
        }
    }
    assert_eq!(ok, 1);
    // Closing the open one allows a NEW assignment (a new record, not a changed owner).
    let open = h.numbers.assignments(&s, 10).await.unwrap().remove(0);
    h.numbers
        .close_assignment(&ctx(), actor(), open.id, open.revision, "WITHDRAWN")
        .await
        .unwrap();
    let next = h
        .numbers
        .request_assignment(
            &ctx(),
            &s,
            actor(),
            assignment_for(r.id, OwnershipClass::Department),
        )
        .await
        .unwrap();
    assert_ne!(next.id, open.id);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM dcn_assignments WHERE number_resource_id = '{}'",
            r.id
        ))
        .await,
        2
    );
}

// D10.12 --------------------------------------------------------------------------------

#[tokio::test]
async fn tenants_and_environments_are_isolated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mine = scope(EnvironmentKind::Sandbox);
    let j = h.active(&mine, OwnershipClass::Personal).await;
    // Another tenant, and another environment of the SAME application.
    let stranger = scope(EnvironmentKind::Sandbox);
    let sibling = numbering::numbers::OwnerScope {
        environment_id: Uuid::new_v4(),
        ..mine
    };
    for other in [&stranger, &sibling] {
        assert!(matches!(
            h.numbers
                .assignment(other, j.assignment.id)
                .await
                .unwrap_err(),
            DomainError::NotFound(_)
        ));
        assert!(matches!(
            h.numbers
                .assignment_history(other, j.assignment.id)
                .await
                .unwrap_err(),
            DomainError::NotFound(_)
        ));
        assert!(matches!(
            h.numbers
                .reservation(other, j.reservation.id)
                .await
                .unwrap_err(),
            DomainError::NotFound(_)
        ));
        assert!(h.numbers.assignments(other, 50).await.unwrap().is_empty());
        assert!(matches!(
            h.numbers
                .current_routing_profile(other, j.routing_profile_id)
                .await
                .unwrap_err(),
            DomainError::NotFound(_)
        ));
    }
    assert_eq!(h.numbers.assignments(&mine, 50).await.unwrap().len(), 1);

    // Sandbox / test never touch real inventory; production never touches synthetic.
    let live = h.available_number(InventorySource::LiveProvider).await;
    let synthetic = h.available_number(InventorySource::SyntheticTest).await;
    for (s, resource) in [
        (scope(EnvironmentKind::Sandbox), live.id),
        (scope(EnvironmentKind::Test), live.id),
        (scope(EnvironmentKind::Production), synthetic.id),
    ] {
        let err = h
            .numbers
            .reserve(&ctx(), &s, actor(), resource, Duration::minutes(5), None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ENVIRONMENT_INVENTORY_MISMATCH");
        let err = h
            .numbers
            .request_assignment(
                &ctx(),
                &s,
                actor(),
                assignment_for(resource, OwnershipClass::Organization),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ENVIRONMENT_INVENTORY_MISMATCH");
    }
    let err = exec(
        h.conn(),
        &format!(
            "INSERT INTO number_reservations (id, number_resource_id, tenant_id, application_id, environment_id, \
               environment_kind, service_account_id, status, revision, expires_at, correlation_id, created_at, updated_at) \
             VALUES (gen_random_uuid(), '{}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'PRODUCTION', \
               gen_random_uuid(), 'PENDING', 1, now() + interval '1 hour', 'corr_abcdefgh', now(), now())",
            synthetic.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("never used by a PRODUCTION environment"),
        "{err}"
    );

    // Another scope's held reservation cannot be attached (domain) nor referenced (FK).
    let (_, theirs) = h.held(&stranger).await;
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let a = h
        .numbers
        .request_assignment(
            &ctx(),
            &mine,
            actor(),
            assignment_for(r.id, OwnershipClass::Campaign),
        )
        .await
        .unwrap();
    let a = h
        .numbers
        .submit_for_eligibility(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    let a = h
        .numbers
        .record_eligibility(
            &ctx(),
            actor(),
            a.id,
            a.revision,
            eligible_decision(h.conn(), &mine, h.now()).await,
        )
        .await
        .unwrap();
    assert!(matches!(
        h.numbers
            .attach_reservation(&ctx(), actor(), a.id, a.revision, theirs.id)
            .await
            .unwrap_err(),
        DomainError::NotFound(_)
    ));
    let err = exec(
        h.conn(),
        &format!("UPDATE dcn_assignments SET reservation_id = '{}', revision = revision + 1 WHERE id = '{}'", theirs.id, a.id),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("fk_dcn_assignments_reservation"),
        "{err}"
    );
    // Nor can another scope's routing profile be pinned.
    let foreign_profile = h
        .numbers
        .create_routing_profile(&ctx(), &stranger, actor(), voice_to_principal())
        .await
        .unwrap();
    assert!(matches!(
        h.numbers
            .set_assignment_routing(
                &ctx(),
                actor(),
                a.id,
                a.revision,
                foreign_profile.profile_id
            )
            .await
            .unwrap_err(),
        DomainError::NotFound(_)
    ));
}

// D10.13 --------------------------------------------------------------------------------

#[tokio::test]
async fn reassignment_creates_a_new_record_and_history_is_preserved() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let first_owner = scope(EnvironmentKind::Production);
    let j = h.active(&first_owner, OwnershipClass::Professional).await;
    let n = &h.numbers;
    // Suspend and resume (assignment and number together).
    let a = n
        .set_assignment_suspended(
            &ctx(),
            actor(),
            j.assignment.id,
            j.assignment.revision,
            true,
            "PAYMENT_OVERDUE",
        )
        .await
        .unwrap();
    assert_eq!(n.resource(j.resource_id).await.unwrap().state, "SUSPENDED");
    let a = n
        .set_assignment_suspended(&ctx(), actor(), a.id, a.revision, false, "PAYMENT_RECEIVED")
        .await
        .unwrap();
    assert_eq!(n.resource(j.resource_id).await.unwrap().state, "ACTIVE");
    // Release: the number cannot start RELEASING while the assignment is live.
    let r = n.resource(j.resource_id).await.unwrap();
    let err = n
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Releasing, r.revision, actor(), "EARLY"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "ASSIGNMENT_ACTIVE");
    let a = n
        .request_release(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    let a = n
        .remove_routing(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    assert_eq!(n.resource(j.resource_id).await.unwrap().state, "RELEASING");
    let until = h.now() + Duration::days(90);
    let a = n
        .quarantine_assignment(&ctx(), actor(), a.id, a.revision, until)
        .await
        .unwrap();
    let r = n.resource(j.resource_id).await.unwrap();
    assert_eq!(
        (r.state.as_str(), r.quarantine_until),
        ("QUARANTINED", Some(until))
    );
    // Quarantined numbers are not allocatable, even after the old record closes.
    let closed = n
        .close_assignment(&ctx(), actor(), a.id, a.revision, "RELEASED_TO_POOL")
        .await
        .unwrap();
    assert!(closed.closed_at.is_some());
    let next_owner = scope(EnvironmentKind::Production);
    let err = n
        .reserve(
            &ctx(),
            &next_owner,
            actor(),
            j.resource_id,
            Duration::minutes(5),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "NOT_ALLOCATABLE");
    // After the cool-down the number is reassignable to a NEW owner, in a NEW record.
    h.clock.advance(Duration::days(91));
    let r = n.resource(j.resource_id).await.unwrap();
    n.transition(
        &ctx(),
        r.id,
        TransitionRequest::new(
            LifecycleState::Reassignable,
            r.revision,
            actor(),
            "COOLED_DOWN",
        ),
    )
    .await
    .unwrap();
    n.observe(&ctx(), r.id, {
        let mut o = numbering::numbers::inventory::NewObservation::offered(
            InventorySource::LiveProvider,
            h.now(),
            Duration::hours(1),
        );
        o.provider_id = Some("acme-telecom".into());
        o
    })
    .await
    .unwrap();
    let res = n
        .reserve(
            &ctx(),
            &next_owner,
            actor(),
            j.resource_id,
            Duration::minutes(5),
            None,
        )
        .await
        .unwrap();
    let a2 = n
        .request_assignment(
            &ctx(),
            &next_owner,
            actor(),
            assignment_for(j.resource_id, OwnershipClass::Organization),
        )
        .await
        .unwrap();
    assert_ne!(a2.id, j.assignment.id);
    // The previous owner's record and full history are intact and still theirs.
    let old = n.assignment(&first_owner, j.assignment.id).await.unwrap();
    assert_eq!(
        (old.status.as_str(), old.owner_ref),
        ("CLOSED", j.assignment.owner_ref)
    );
    let history = n
        .assignment_history(&first_owner, j.assignment.id)
        .await
        .unwrap();
    let path: Vec<&str> = history.iter().map(|e| e.to_status.as_str()).collect();
    assert_eq!(path.first(), Some(&"REQUESTED"));
    assert_eq!(path.last(), Some(&"CLOSED"));
    for status in [
        "ELIGIBLE",
        "RESERVED",
        "ACTIVATING",
        "ACTIVE",
        "SUSPENDED",
        "RELEASE_REQUESTED",
        "ROUTING_REMOVED",
        "QUARANTINED",
    ] {
        assert!(path.contains(&status), "{status} missing from {path:?}");
    }
    let revisions: Vec<i64> = history.iter().map(|e| e.assignment_revision).collect();
    assert_eq!(
        revisions,
        (1..=revisions.len() as i64).collect::<Vec<_>>(),
        "one history row per revision"
    );
    assert!(history
        .iter()
        .all(|e| e.owner_ref == j.assignment.owner_ref));
    // The new owner cannot see the old owner's history; operators can see both.
    assert!(n
        .assignment_history(&next_owner, j.assignment.id)
        .await
        .is_err());
    assert_eq!(
        n.resource_assignment_history(&Actor::system("worker"), j.resource_id)
            .await
            .unwrap_err()
            .code(),
        "PRIVILEGED_ACCESS_REQUIRED"
    );
    let all = n
        .resource_assignment_history(&Actor::operator("ops-1"), j.resource_id)
        .await
        .unwrap();
    let owners: HashSet<Uuid> = all.iter().map(|e| e.assignment_id).collect();
    assert_eq!(owners, HashSet::from([j.assignment.id, a2.id]));
    // History is append-only; closed assignments are history.
    for sql in [
        format!("UPDATE number_assignment_history SET reason_code = 'X_X' WHERE assignment_id = '{}'", j.assignment.id),
        format!("DELETE FROM number_assignment_history WHERE assignment_id = '{}'", j.assignment.id),
        "TRUNCATE number_assignment_history".to_string(),
        format!("UPDATE dcn_assignments SET close_reason = 'CHANGED', revision = revision + 1 WHERE id = '{}'", j.assignment.id),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
    let _ = res;
}

// D10.14 --------------------------------------------------------------------------------

#[tokio::test]
async fn routing_profiles_are_versioned_and_assignments_pin_a_revision() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let n = &h.numbers;
    let mut j = h.reserved_assignment(&s, OwnershipClass::Department).await;
    assert_eq!(j.assignment.routing_profile_revision, Some(1));
    let ring_group = Uuid::new_v4();
    let v2 = n
        .revise_routing_profile(
            &ctx(),
            &s,
            actor(),
            j.routing_profile_id,
            1,
            RoutingTargets {
                voice: (VoiceTargetKind::EnterpriseRingGroup, Some(ring_group)),
                sms: (SmsTargetKind::None, None),
            },
        )
        .await
        .unwrap();
    assert_eq!(v2.revision, 2);
    // A stale revision conflicts; targets need a reference exactly when not NONE.
    assert!(matches!(
        n.revise_routing_profile(
            &ctx(),
            &s,
            actor(),
            j.routing_profile_id,
            1,
            voice_to_principal()
        )
        .await
        .unwrap_err(),
        DomainError::VersionConflict(_)
    ));
    assert!(matches!(
        n.create_routing_profile(
            &ctx(),
            &s,
            actor(),
            RoutingTargets {
                voice: (VoiceTargetKind::EnterpriseQueue, None),
                sms: (SmsTargetKind::None, None)
            }
        )
        .await
        .unwrap_err(),
        DomainError::Validation(_)
    ));
    // The assignment still pins revision 1 until it is re-pinned.
    assert_eq!(
        n.assignment(&s, j.assignment.id)
            .await
            .unwrap()
            .routing_profile_revision,
        Some(1)
    );
    let before = n.resource(j.resource_id).await.unwrap().routing_revision;
    j.assignment = n
        .set_assignment_routing(
            &ctx(),
            actor(),
            j.assignment.id,
            j.assignment.revision,
            j.routing_profile_id,
        )
        .await
        .unwrap();
    assert_eq!(j.assignment.routing_profile_revision, Some(2));
    assert_eq!(
        n.resource(j.resource_id).await.unwrap().routing_revision,
        before + 1
    );
    // Every revision is kept; exactly one is CURRENT.
    let revisions = n
        .routing_profile_revisions(&s, j.routing_profile_id)
        .await
        .unwrap();
    let states: Vec<(i64, &str)> = revisions
        .iter()
        .map(|r| (r.revision, r.status.as_str()))
        .collect();
    assert_eq!(states, vec![(1, "SUPERSEDED"), (2, "CURRENT")]);
    assert_eq!(
        n.current_routing_profile(&s, j.routing_profile_id)
            .await
            .unwrap()
            .voice_target_ref,
        Some(ring_group)
    );
    // The database refuses revision gaps, scope moves, edits and deletion.
    let p = j.routing_profile_id;
    for sql in [
        format!(
            "INSERT INTO number_routing_profiles (id, profile_id, revision, tenant_id, application_id, environment_id, \
               voice_target_kind, sms_target_kind, status, created_by, created_at) \
             VALUES (gen_random_uuid(), '{p}', 5, '{}', '{}', '{}', 'NONE', 'NONE', 'CURRENT', 'raw', now())",
            s.tenant_id, s.application_id, s.environment_id
        ),
        format!(
            "INSERT INTO number_routing_profiles (id, profile_id, revision, tenant_id, application_id, environment_id, \
               voice_target_kind, sms_target_kind, status, created_by, created_at) \
             VALUES (gen_random_uuid(), '{p}', 3, gen_random_uuid(), '{}', '{}', 'NONE', 'NONE', 'SUPERSEDED', 'raw', now())",
            s.application_id, s.environment_id
        ),
        format!("UPDATE number_routing_profiles SET voice_target_ref = gen_random_uuid() WHERE profile_id = '{p}' AND revision = 1"),
        format!("DELETE FROM number_routing_profiles WHERE profile_id = '{p}'"),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
    // An assignment cannot pin a revision that does not exist (composite FK).
    let err = exec(
        h.conn(),
        &format!(
            "UPDATE dcn_assignments SET routing_profile_revision = 7, revision = revision + 1 WHERE id = '{}'",
            j.assignment.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("fk_dcn_assignments_routing"),
        "{err}"
    );
}

// D10.15 --------------------------------------------------------------------------------

#[tokio::test]
async fn an_assignment_never_becomes_active_prematurely() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let n = &h.numbers;
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let a = n
        .request_assignment(
            &ctx(),
            &s,
            actor(),
            assignment_for(r.id, OwnershipClass::Personal),
        )
        .await
        .unwrap();
    // Not from REQUESTED, ELIGIBLE or RESERVED; activation is a single entry.
    assert!(matches!(
        n.activate_assignment(&ctx(), actor(), a.id, a.revision)
            .await
            .unwrap_err(),
        DomainError::InvalidTransition { .. }
    ));
    for from in AssignmentStatus::ALL {
        if assignment_allowed(*from, AssignmentStatus::Active) {
            assert!(
                matches!(
                    from,
                    AssignmentStatus::Activating | AssignmentStatus::Suspended
                ),
                "{from}"
            );
        }
    }
    // Eligibility cannot be skipped.
    assert!(matches!(
        n.attach_reservation(&ctx(), actor(), a.id, a.revision, Uuid::new_v4())
            .await
            .unwrap_err(),
        DomainError::NotFound(_) | DomainError::InvalidTransition { .. }
    ));
    let err = in_txn(
        h.conn(),
        &[
            "SELECT set_config('numbering.assignment', 'transition', true)",
            &format!("UPDATE dcn_assignments SET status = 'ELIGIBLE', revision = revision + 1 WHERE id = '{}'", a.id),
        ],
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("not allowed")
            || err.to_string().contains("ck_dcn_assignments_eligibility"),
        "{err}"
    );
    // Not without the policy's session flag at all.
    let err = exec(
        h.conn(),
        &format!(
            "UPDATE dcn_assignments SET status = 'CLOSED', revision = revision + 1 WHERE id = '{}'",
            a.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("only through a domain transition"),
        "{err}"
    );

    // An unheld reservation is not enough for RESERVED.
    let a = n
        .submit_for_eligibility(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    let a = n
        .record_eligibility(
            &ctx(),
            actor(),
            a.id,
            a.revision,
            eligible_decision(h.conn(), &s, h.now()).await,
        )
        .await
        .unwrap();
    let pending = n
        .reserve(&ctx(), &s, actor(), r.id, Duration::minutes(10), None)
        .await
        .unwrap();
    assert_eq!(
        n.attach_reservation(&ctx(), actor(), a.id, a.revision, pending.id)
            .await
            .unwrap_err()
            .code(),
        "HOLD_REQUIRED"
    );
    n.confirm_hold(&ctx(), actor(), pending.id, Uuid::new_v4())
        .await
        .unwrap();
    let a = n
        .attach_reservation(&ctx(), actor(), a.id, a.revision, pending.id)
        .await
        .unwrap();
    // No activation without routing…
    assert_eq!(
        n.begin_activation(&ctx(), actor(), a.id, a.revision)
            .await
            .unwrap_err()
            .code(),
        "ROUTING_REQUIRED"
    );
    let p = n
        .create_routing_profile(&ctx(), &s, actor(), voice_to_principal())
        .await
        .unwrap();
    let a = n
        .set_assignment_routing(&ctx(), actor(), a.id, a.revision, p.profile_id)
        .await
        .unwrap();
    let a = n
        .begin_activation(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    // …nor before the number is provisioned…
    assert_eq!(
        n.activate_assignment(&ctx(), actor(), a.id, a.revision)
            .await
            .unwrap_err()
            .code(),
        "PROVISIONING_REQUIRED"
    );
    let res = n.resource(r.id).await.unwrap();
    let err = n
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Provisioned, res.revision, actor(), "SKIP"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition { .. }));
    n.transition(
        &ctx(),
        r.id,
        TransitionRequest::new(
            LifecycleState::Provisioning,
            res.revision,
            actor(),
            "PROVISION_REQUESTED",
        ),
    )
    .await
    .unwrap();
    // …nor without a provider binding (PROVISIONED needs one)…
    let res = n.resource(r.id).await.unwrap();
    assert_eq!(
        n.transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Provisioned,
                res.revision,
                actor(),
                "PROVISIONED"
            )
        )
        .await
        .unwrap_err()
        .code(),
        "BINDING_REQUIRED"
    );
    // …and the binding must cover every routable capability the assignment relies on.
    n.record_capability(
        &ctx(),
        r.id,
        numbering::numbers::capabilities::NewCapability::declared(
            Capability::SmsInbound,
            numbering::numbers::CapabilitySource::Synthetic,
        ),
    )
    .await
    .unwrap();
    // (The snapshot was taken at request time: it has no capability yet; a fresh request
    // would snapshot SMS_INBOUND. Use the database to prove the activation rule instead.)
    n.activate_binding(
        &ctx(),
        r.id,
        binding_for(InventorySource::SyntheticTest, &[Capability::VoiceInbound]),
    )
    .await
    .unwrap();
    let res = n.resource(r.id).await.unwrap();
    n.transition(
        &ctx(),
        r.id,
        TransitionRequest::new(
            LifecycleState::Provisioned,
            res.revision,
            actor(),
            "PROVISIONED",
        ),
    )
    .await
    .unwrap();
    // The number becomes ACTIVE only for an ACTIVATING assignment, the assignment only on
    // an ACTIVE number — database-enforced in both directions.
    let err = in_txn(
        h.conn(),
        &[
            "SELECT set_config('numbering.assignment', 'transition', true)",
            &format!(
                "UPDATE dcn_assignments SET status = 'ACTIVE', activated_at = now(), revision = revision + 1 WHERE id = '{}'",
                a.id
            ),
        ],
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("only on an ACTIVE number resource"),
        "{err}"
    );
    let done = n
        .activate_assignment(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    assert_eq!(done.status, "ACTIVE");
    assert!(done.activated_at.is_some());
    assert_eq!(n.resource(r.id).await.unwrap().state, "ACTIVE");
}

#[tokio::test]
async fn a_number_never_becomes_active_without_its_activating_assignment_and_bindings() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Production);
    let n = &h.numbers;
    let mut j = h
        .reserved_assignment(&s, OwnershipClass::Organization)
        .await;
    let r = n.resource(j.resource_id).await.unwrap();
    n.transition(
        &ctx(),
        r.id,
        TransitionRequest::new(
            LifecycleState::Provisioning,
            r.revision,
            actor(),
            "PROVISION_REQUESTED",
        ),
    )
    .await
    .unwrap();
    // A binding that does NOT cover the snapshotted VOICE_INBOUND.
    n.activate_binding(
        &ctx(),
        r.id,
        binding_for(InventorySource::LiveProvider, &[Capability::SmsOutbound]),
    )
    .await
    .unwrap();
    let r = n.resource(j.resource_id).await.unwrap();
    let r = n
        .transition(
            &ctx(),
            r.id,
            TransitionRequest::new(
                LifecycleState::Provisioned,
                r.revision,
                actor(),
                "PROVISIONED",
            ),
        )
        .await
        .unwrap();
    // PROVISIONED → ACTIVE directly: no ACTIVATING assignment yet.
    assert_eq!(
        n.transition(
            &ctx(),
            r.id,
            TransitionRequest::new(LifecycleState::Active, r.revision, actor(), "ACTIVATE")
        )
        .await
        .unwrap_err()
        .code(),
        "ASSIGNMENT_REQUIRED"
    );
    let err = in_txn(
        h.conn(),
        &[
            "SELECT set_config('numbering.lifecycle', 'transition', true)",
            &format!("UPDATE number_resources SET state = 'ACTIVE', revision = revision + 1 WHERE id = '{}'", r.id),
        ],
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("ACTIVATING"), "{err}");
    j.assignment = n
        .begin_activation(&ctx(), actor(), j.assignment.id, j.assignment.revision)
        .await
        .unwrap();
    assert_eq!(
        n.activate_assignment(&ctx(), actor(), j.assignment.id, j.assignment.revision)
            .await
            .unwrap_err()
            .code(),
        "BINDING_REQUIRED"
    );
    n.activate_binding(
        &ctx(),
        r.id,
        binding_for(InventorySource::LiveProvider, &[Capability::VoiceInbound]),
    )
    .await
    .unwrap();
    let active = n
        .activate_assignment(&ctx(), actor(), j.assignment.id, j.assignment.revision)
        .await
        .unwrap();
    assert_eq!(active.status, "ACTIVE");
}

// D10.16 --------------------------------------------------------------------------------

#[tokio::test]
async fn quarantined_and_uncertain_numbers_are_never_allocated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Production);
    let n = &h.numbers;
    let q = h.available_number(InventorySource::LiveProvider).await;
    n.transition(
        &ctx(),
        q.id,
        TransitionRequest::new(
            LifecycleState::Quarantined,
            q.revision,
            actor(),
            "FRAUD_SIGNAL",
        )
        .with_quarantine_until(h.now() + Duration::days(30)),
    )
    .await
    .unwrap();
    let u = h.available_number(InventorySource::LiveProvider).await;
    n.transition(
        &ctx(),
        u.id,
        TransitionRequest::new(
            LifecycleState::UnknownProviderState,
            u.revision,
            actor(),
            "PROVIDER_TIMEOUT",
        )
        .uncertain(UncertainOperation::Reserve),
    )
    .await
    .unwrap();
    assert_eq!(
        n.reserve(&ctx(), &s, actor(), q.id, Duration::minutes(5), None)
            .await
            .unwrap_err()
            .code(),
        "NOT_ALLOCATABLE"
    );
    assert_eq!(
        n.reserve(&ctx(), &s, actor(), u.id, Duration::minutes(5), None)
            .await
            .unwrap_err()
            .code(),
        "RECONCILIATION_REQUIRED"
    );
    for id in [q.id, u.id] {
        assert_eq!(
            n.request_assignment(
                &ctx(),
                &s,
                actor(),
                assignment_for(id, OwnershipClass::Organization)
            )
            .await
            .unwrap_err()
            .code(),
            "NOT_ALLOCATABLE"
        );
        // The database refuses a raw reservation as well.
        let err = exec(
            h.conn(),
            &format!(
                "INSERT INTO number_reservations (id, number_resource_id, tenant_id, application_id, environment_id, \
                   environment_kind, service_account_id, status, revision, expires_at, correlation_id, created_at, updated_at) \
                 VALUES (gen_random_uuid(), '{id}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'PRODUCTION', \
                   gen_random_uuid(), 'PENDING', 1, now() + interval '1 hour', 'corr_abcdefgh', now(), now())"
            ),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cannot be reserved"), "{err}");
    }
    // A provider-affecting step is not repeated while uncertain: only reconciliation with
    // evidence moves it, after which allocation works again.
    let u = n.resource(u.id).await.unwrap();
    n.transition(
        &ctx(),
        u.id,
        TransitionRequest::new(
            LifecycleState::Available,
            u.revision,
            reconciler(),
            "PROVIDER_CONFIRMED_NO_HOLD",
        )
        .with_evidence(Uuid::new_v4()),
    )
    .await
    .unwrap();
    n.reserve(&ctx(), &s, actor(), u.id, Duration::minutes(5), None)
        .await
        .unwrap();
}

// D10.17 --------------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_operation_rolls_back_every_row_and_event_it_wrote() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let n = &h.numbers;
    let mut j = h.reserved_assignment(&s, OwnershipClass::SharedTeam).await;
    h.provisioned(&s, &mut j).await;
    let snapshot = |h: &Harness| {
        let conn = h.conn().clone();
        async move {
            (
                common::scalar(&conn, "SELECT count(*) FROM numbering_outbox_events").await,
                common::scalar(&conn, "SELECT count(*) FROM number_lifecycle_events").await,
                common::scalar(&conn, "SELECT count(*) FROM number_assignment_history").await,
            )
        }
    };
    let before = snapshot(&h).await;
    let resource_before = n.resource(j.resource_id).await.unwrap();
    // activate_assignment moves the NUMBER first (PROVISIONED → ACTIVE, lifecycle event,
    // outbox event) and then fails on the stale assignment revision: all of it rolls back.
    let err = n
        .activate_assignment(&ctx(), actor(), j.assignment.id, j.assignment.revision - 1)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionConflict(_)), "{err:?}");
    assert_eq!(snapshot(&h).await, before);
    assert_eq!(n.resource(j.resource_id).await.unwrap(), resource_before);
    assert_eq!(
        n.assignment(&s, j.assignment.id).await.unwrap().status,
        "ACTIVATING"
    );
    // Successful operations write state, history and events together.
    n.activate_assignment(&ctx(), actor(), j.assignment.id, j.assignment.revision)
        .await
        .unwrap();
    let after = snapshot(&h).await;
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (2, 1, 1)
    );
    // Every lifecycle change has its outbox event and every assignment history row its
    // event (no orphan on either side).
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.assignment.changed'").await,
        h.scalar("SELECT count(*) FROM number_assignment_history").await
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM numbering_outbox_events WHERE event_type IN \
               ('number.resource.discovered', 'number.resource.lifecycle-changed')"
        )
        .await,
        h.scalar("SELECT count(*) FROM number_lifecycle_events")
            .await
    );
}

//! Part C §19: logical participants vs sessions, server-generated media identities,
//! multi-device sessions, explicit reconnect (renewal / replacement), connection intervals
//! and participation time computed from the union of intervals.

mod common;

use calling::{
    calls::{
        ids::MediaIdentity,
        kinds::{DisconnectReason, EndpointCategory, ParticipantRole, SessionState},
        participants::DisplayName,
        service::NewCall,
        CallError, CallState,
    },
    domain::{Clock, EnvironmentKind, PrincipalKind},
};
use chrono::Duration;
use common::{
    calls::{sql, CallWorld},
    Harness,
};

#[tokio::test]
async fn trusted_principals_become_participants_and_foreign_principals_never_do() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await;
    assert_eq!(call.initiator.role, ParticipantRole::Initiator);
    assert_eq!(call.initiator.principal_id, w.initiator.id());
    assert_eq!(
        call.initiator.principal_kind,
        PrincipalKind::ExternalApplicationPrincipal
    );
    let (recipient, created) = w
        .calls()
        .add_participant(&w.cmd, call.call.id, &w.recipient, None)
        .await
        .unwrap();
    assert!(created);
    assert_eq!(recipient.role, ParticipantRole::Recipient);

    // Principals of another tenant, another application and another environment.
    let h2 = Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone());
    let other_tenant = CallWorld::in_harness(h2, "other", EnvironmentKind::Production).await;
    let other_app = w.h.application_in(&w.consumer.tenant, "second").await;
    let other_app = CallWorld::for_consumer(
        Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone()),
        other_app,
        EnvironmentKind::Production,
    )
    .await;
    let sandbox = CallWorld::for_consumer(
        Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone()),
        w.h.application_in(&w.consumer.tenant, "third").await,
        EnvironmentKind::Sandbox,
    )
    .await;
    let same_app_sandbox_principal = {
        let token =
            w.h.issue(w.consumer.sa(EnvironmentKind::Sandbox), &[])
                .await;
        let ctx = w.h.ctx(token.secret.expose_once()).await;
        common::calls::resolve(
            &w.h,
            &w.consumer,
            EnvironmentKind::Sandbox,
            &ctx,
            "sbx-user",
        )
        .await
    };
    for foreign in [
        &other_tenant.recipient,
        &other_app.recipient,
        &sandbox.recipient,
        &same_app_sandbox_principal,
    ] {
        assert!(matches!(
            w.calls()
                .add_participant(&w.cmd, call.call.id, foreign, None)
                .await
                .unwrap_err(),
            CallError::NotFound
        ));
        // Nor can it initiate a call here.
        assert!(matches!(
            w.calls()
                .create_requested_call(
                    &w.ctx,
                    foreign,
                    NewCall::of_kind(calling::calls::kinds::CallKind::Voice)
                )
                .await
                .unwrap_err(),
            CallError::NotFound
        ));
    }
    // The database refuses a participant whose principal is in another scope, or whose
    // kind snapshot is wrong.
    for (principal, kind) in [
        (
            other_tenant.recipient.id().to_string(),
            "EXTERNAL_APPLICATION_PRINCIPAL",
        ),
        (
            w.principal("kind-check").await.id().to_string(),
            "NATIVE_CF_USER",
        ),
    ] {
        let err = sql(
            &w.h,
            &format!(
                "INSERT INTO call_participants SELECT gen_random_uuid(), id, tenant_id, application_id, environment_id, \
                 '{principal}', '{kind}', 'RECIPIENT', NULL, now(), NULL, now() FROM calls WHERE id = '{}'",
                call.call.id
            ),
        )
        .await
        .unwrap_err();
        assert!(err.contains("fk_call_participants_principal"), "{err}");
    }
}

#[tokio::test]
async fn one_logical_participant_per_principal_per_call() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await;
    let (first, created) = w
        .calls()
        .add_participant(&w.cmd, call.call.id, &w.recipient, None)
        .await
        .unwrap();
    assert!(created);
    let (again, created) = w
        .calls()
        .add_participant(
            &w.cmd,
            call.call.id,
            &w.recipient,
            DisplayName::parse("Other").ok(),
        )
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(again.id, first.id);
    // The initiator is already the INITIATOR participant.
    let (initiator, created) = w
        .calls()
        .add_participant(&w.cmd, call.call.id, &w.initiator, None)
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(initiator.id, call.initiator.id);
    assert_eq!(initiator.role, ParticipantRole::Initiator);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM call_participants WHERE call_id = '{}'",
            call.call.id
        ))
        .await,
        2
    );
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_participants SELECT gen_random_uuid(), call_id, tenant_id, application_id, environment_id, \
             principal_id, principal_kind, 'RECIPIENT', NULL, now(), NULL, now() FROM call_participants WHERE id = '{}'",
            first.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("uq_call_participants_call_principal"), "{err}");
    // Only one INITIATOR, ever.
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_participants SELECT gen_random_uuid(), id, tenant_id, application_id, environment_id, \
             '{}', 'EXTERNAL_APPLICATION_PRINCIPAL', 'INITIATOR', NULL, now(), NULL, now() FROM calls WHERE id = '{}'",
            w.principal("second-initiator").await.id(),
            call.call.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("uq_call_participants_one_initiator"), "{err}");
}

#[tokio::test]
async fn display_names_are_presentation_metadata_never_identity() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await.call;
    let ada = DisplayName::parse("Ada").unwrap();
    let other = w.principal("another-ada").await;
    let (a, _) = w
        .calls()
        .add_participant(&w.cmd, call.id, &w.recipient, Some(ada.clone()))
        .await
        .unwrap();
    let (b, _) = w
        .calls()
        .add_participant(&w.cmd, call.id, &other, Some(ada.clone()))
        .await
        .unwrap();
    // Same display name, two identities.
    assert_eq!(a.display_name, b.display_name);
    assert_ne!(a.id, b.id);
    assert_ne!(a.principal_id, b.principal_id);
    // Changing a display name changes nothing else.
    let renamed = w
        .calls()
        .set_participant_display_name(&w.cmd, call.id, a.id, DisplayName::parse("Grace").ok())
        .await
        .unwrap();
    assert_eq!(
        (renamed.id, renamed.principal_id, renamed.role),
        (a.id, a.principal_id, a.role)
    );
    assert_eq!(renamed.display_name.unwrap().as_str(), "Grace");
    // The event never carries the name itself.
    let events = w.calls().events(w.scope(), call.id).await.unwrap();
    let updated = events.last().unwrap();
    assert_eq!(updated.data["display_name_set"], true);
    assert!(!updated.data.to_string().contains("Grace"));
    // Telephone-number-like names are refused by Rust and by the database.
    assert!(DisplayName::parse("+2348012345678").is_err());
    let err = sql(
        &w.h,
        &format!(
            "UPDATE call_participants SET display_name = '0801 234 5678' WHERE id = '{}'",
            a.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("ck_call_participants_display_name"), "{err}");
}

#[tokio::test]
async fn session_media_identities_are_server_generated_and_unique() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await;
    let mut identities = Vec::new();
    for category in EndpointCategory::ALL {
        let s = w
            .calls()
            .create_session(&w.cmd, call.call.id, call.initiator.id, *category)
            .await
            .unwrap();
        assert!(MediaIdentity::is_well_formed(s.media_identity.as_str()));
        assert_eq!(s.state, SessionState::Created);
        assert_eq!(s.generation, 0);
        identities.push(s.media_identity.as_str().to_string());
    }
    let mut unique = identities.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), identities.len());
    // A client-style identity (provider participant id, user id, phone number) or a
    // duplicate cannot be stored.
    for bad in ["participant-123", "+2348012345678", "PA_livekit_xyz"] {
        let err = sql(
            &w.h,
            &format!(
                "INSERT INTO call_sessions SELECT gen_random_uuid(), call_id, participant_id, tenant_id, application_id, \
                 environment_id, '{bad}', 'WEB', 'CREATED', 0, NULL, now(), NULL, NULL, NULL, now() \
                 FROM call_sessions WHERE call_id = '{}' LIMIT 1",
                call.call.id
            ),
        )
        .await
        .unwrap_err();
        assert!(err.contains("ck_call_sessions_media_identity"), "{err}");
    }
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_sessions SELECT gen_random_uuid(), call_id, participant_id, tenant_id, application_id, \
             environment_id, media_identity, 'WEB', 'CREATED', 0, NULL, now(), NULL, NULL, NULL, now() \
             FROM call_sessions WHERE call_id = '{}' LIMIT 1",
            call.call.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("uq_call_sessions_media_identity"), "{err}");
    // The identity is immutable.
    let err = sql(
        &w.h,
        &format!(
            "UPDATE call_sessions SET media_identity = 'cfmid_{}' WHERE call_id = '{}'",
            "0".repeat(64),
            call.call.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("immutable"), "{err}");
}

#[tokio::test]
async fn one_participant_may_hold_several_simultaneous_sessions() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await;
    let mut ids = Vec::new();
    for category in [
        EndpointCategory::Mobile,
        EndpointCategory::Web,
        EndpointCategory::Desktop,
    ] {
        let s = w
            .calls()
            .create_session(&w.cmd, call.call.id, call.initiator.id, category)
            .await
            .unwrap();
        w.calls()
            .connect_session(&w.cmd, call.call.id, s.id)
            .await
            .unwrap();
        ids.push(s.id);
    }
    let sessions = w.calls().sessions(w.scope(), call.call.id).await.unwrap();
    assert_eq!(sessions.len(), 3);
    assert!(sessions
        .iter()
        .all(|s| s.participant_id == call.initiator.id && s.state == SessionState::Connected));
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM call_participants WHERE call_id = '{}'",
            call.call.id
        ))
        .await,
        1
    );
    // No uniqueness on participant_id exists (multi-device is structural).
    let unique_on_participant = w
        .count(
            "SELECT count(*) FROM pg_indexes WHERE tablename = 'call_sessions' \
             AND indexdef ILIKE 'CREATE UNIQUE INDEX%(participant_id)'",
        )
        .await;
    assert_eq!(unique_on_participant, 0);
}

#[tokio::test]
async fn sessions_never_cross_calls_or_scopes() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let a = w.create().await;
    let b = w.create().await;
    // A participant of call A cannot get a session on call B.
    assert!(matches!(
        w.calls()
            .create_session(&w.cmd, b.call.id, a.initiator.id, EndpointCategory::Web)
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
    let session = w
        .calls()
        .create_session(&w.cmd, a.call.id, a.initiator.id, EndpointCategory::Web)
        .await
        .unwrap();
    assert!(matches!(
        w.calls()
            .connect_session(&w.cmd, b.call.id, session.id)
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
    // Another tenant cannot touch it at all.
    let other = CallWorld::in_harness(
        Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone()),
        "intruder",
        EnvironmentKind::Production,
    )
    .await;
    assert!(matches!(
        other
            .calls()
            .connect_session(&other.cmd, a.call.id, session.id)
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
    assert!(matches!(
        other
            .calls()
            .create_session(&other.cmd, a.call.id, a.initiator.id, EndpointCategory::Web)
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
    // The database refuses cross-call and cross-tenant session rows.
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_sessions SELECT gen_random_uuid(), '{}', participant_id, tenant_id, application_id, \
             environment_id, 'cfmid_{}', 'WEB', 'CREATED', 0, NULL, now(), NULL, NULL, NULL, now() \
             FROM call_sessions WHERE id = '{}'",
            b.call.id,
            "a".repeat(64),
            session.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("fk_call_sessions_participant"), "{err}");
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_sessions SELECT gen_random_uuid(), call_id, participant_id, '{}', application_id, \
             environment_id, 'cfmid_{}', 'WEB', 'CREATED', 0, NULL, now(), NULL, NULL, NULL, now() \
             FROM call_sessions WHERE id = '{}'",
            other.consumer.tenant.id,
            "b".repeat(64),
            session.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("fk_call_sessions_participant"), "{err}");
    // Ownership columns are immutable.
    let err = sql(
        &w.h,
        &format!(
            "UPDATE call_sessions SET participant_id = '{}' WHERE id = '{}'",
            b.initiator.id, session.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("immutable") || err.contains("fk_"), "{err}");
}

#[tokio::test]
async fn reconnect_renews_the_session_with_a_new_interval_and_keeps_call_and_participant() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let before = w.call(call.id).await;
    let participants_before = w.calls().participants(w.scope(), call.id).await.unwrap();
    let session = w.calls().sessions(w.scope(), call.id).await.unwrap()[0].clone();
    let open = w
        .calls()
        .intervals(w.scope(), call.id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.session_id == session.id)
        .unwrap();
    assert_eq!(open.interval_number, 1);
    assert!(open.disconnected_at.is_none());

    // Only a CREATED session "connects"; a connected one cannot reconnect.
    assert!(matches!(
        w.calls()
            .connect_session(&w.cmd, call.id, session.id)
            .await
            .unwrap_err(),
        CallError::InvalidState(_)
    ));
    assert!(matches!(
        w.calls()
            .reconnect_session(&w.cmd, call.id, session.id)
            .await
            .unwrap_err(),
        CallError::InvalidState(_)
    ));
    w.h.clock.advance(Duration::seconds(10));
    let (s, closed) = w
        .calls()
        .disconnect_session(
            &w.cmd,
            call.id,
            session.id,
            DisconnectReason::ConnectionLost,
        )
        .await
        .unwrap();
    assert_eq!(s.state, SessionState::Disconnected);
    assert_eq!(closed.interval_number, 1);
    assert_eq!(
        closed.disconnect_reason,
        Some(DisconnectReason::ConnectionLost)
    );
    assert!(matches!(
        w.calls()
            .connect_session(&w.cmd, call.id, session.id)
            .await
            .unwrap_err(),
        CallError::InvalidState(_)
    ));
    w.h.clock.advance(Duration::seconds(3));
    let (renewed, second) = w
        .calls()
        .reconnect_session(&w.cmd, call.id, session.id)
        .await
        .unwrap();
    assert_eq!(renewed.id, session.id);
    assert_eq!(renewed.media_identity, session.media_identity);
    assert_eq!(renewed.participant_id, session.participant_id);
    assert_eq!(renewed.generation, 2);
    assert_eq!(second.interval_number, 2);
    assert!(second.connected_at >= closed.disconnected_at.unwrap());
    // Call and participants are unchanged by reconnect.
    let after = w.call(call.id).await;
    assert_eq!((after.state, after.version), (before.state, before.version));
    assert_eq!(
        w.calls().participants(w.scope(), call.id).await.unwrap(),
        participants_before
    );
    let events = w.calls().events(w.scope(), call.id).await.unwrap();
    let kinds: Vec<_> = events
        .iter()
        .rev()
        .take(2)
        .map(|e| e.event_type.as_str())
        .collect();
    assert_eq!(
        kinds,
        ["CALL_SESSION_RECONNECTED", "CALL_SESSION_DISCONNECTED"]
    );

    // Database: one open interval per session, no overlap, no gaps, no reopening.
    let insert = |number: i32, connected: &str| {
        format!(
            "INSERT INTO call_session_intervals SELECT gen_random_uuid(), call_id, id, tenant_id, application_id, \
             environment_id, {number}, {connected}, NULL, NULL, now() FROM call_sessions WHERE id = '{}'",
            session.id
        )
    };
    let err = sql(&w.h, &insert(3, "now()")).await.unwrap_err();
    assert!(
        err.contains("already has an open connection interval"),
        "{err}"
    );
    let err = sql(
        &w.h,
        &format!(
            "UPDATE call_session_intervals SET disconnected_at = NULL, disconnect_reason = NULL \
             WHERE session_id = '{}' AND interval_number = 1",
            session.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("cannot be reassigned"), "{err}");
    w.calls()
        .disconnect_session(&w.cmd, call.id, session.id, DisconnectReason::Left)
        .await
        .unwrap();
    let err = sql(&w.h, &insert(5, "now()")).await.unwrap_err();
    assert!(err.contains("increase by one"), "{err}");
    let err = sql(&w.h, &insert(3, "'2000-01-01T00:00:00Z'"))
        .await
        .unwrap_err();
    assert!(err.contains("must not overlap"), "{err}");
    assert!(sql(&w.h, "DELETE FROM call_session_intervals")
        .await
        .is_err());
}

#[tokio::test]
async fn replacement_reconnect_creates_a_new_session_for_the_same_participant() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let old = w.calls().sessions(w.scope(), call.id).await.unwrap()[0].clone();
    let replacement = w
        .calls()
        .replace_session(&w.cmd, call.id, old.id, EndpointCategory::Desktop)
        .await
        .unwrap();
    assert_ne!(replacement.id, old.id);
    assert_ne!(replacement.media_identity, old.media_identity);
    assert_eq!(replacement.participant_id, old.participant_id);
    assert_eq!(replacement.replaces_session_id, Some(old.id));
    assert_eq!(replacement.state, SessionState::Created);
    let sessions = w.calls().sessions(w.scope(), call.id).await.unwrap();
    let old_now = sessions.iter().find(|s| s.id == old.id).unwrap();
    assert_eq!(old_now.state, SessionState::Ended);
    let intervals = w.calls().intervals(w.scope(), call.id).await.unwrap();
    let old_interval = intervals.iter().find(|i| i.session_id == old.id).unwrap();
    assert_eq!(
        old_interval.disconnect_reason,
        Some(DisconnectReason::SessionReplaced)
    );
    // Still two logical participants; the call did not move.
    assert_eq!(
        w.calls()
            .participants(w.scope(), call.id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(w.call(call.id).await.version, call.version);
    // A session is replaced at most once.
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_sessions SELECT gen_random_uuid(), call_id, participant_id, tenant_id, application_id, \
             environment_id, 'cfmid_{}', 'WEB', 'CREATED', 0, '{}', now(), NULL, NULL, NULL, now() \
             FROM call_sessions WHERE id = '{}'",
            "c".repeat(64),
            old.id,
            replacement.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("uq_call_sessions_replaces"), "{err}");
}

#[tokio::test]
async fn participation_time_is_the_union_of_session_intervals() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await;
    let id = call.call.id;
    let p = call.initiator.id;
    let sec = Duration::seconds;
    let duration = |as_of| {
        w.calls()
            .participant_connected_duration_ms(w.scope(), id, p, as_of)
    };

    // The participant exists for a long time before connecting: that is not participation.
    w.h.clock.advance(sec(600));
    let phone = w
        .calls()
        .create_session(&w.cmd, id, p, EndpointCategory::Mobile)
        .await
        .unwrap();
    // Single interval: 60 s.
    w.calls()
        .connect_session(&w.cmd, id, phone.id)
        .await
        .unwrap();
    w.h.clock.advance(sec(60));
    w.calls()
        .disconnect_session(&w.cmd, id, phone.id, DisconnectReason::ConnectionLost)
        .await
        .unwrap();
    assert_eq!(duration(w.h.clock.now()).await.unwrap(), 60_000);
    // Reconnect after a 20 s gap, 30 s more: 90 s (the gap is not counted).
    w.h.clock.advance(sec(20));
    w.calls()
        .reconnect_session(&w.cmd, id, phone.id)
        .await
        .unwrap();
    w.h.clock.advance(sec(30));
    w.calls()
        .disconnect_session(&w.cmd, id, phone.id, DisconnectReason::Left)
        .await
        .unwrap();
    assert_eq!(duration(w.h.clock.now()).await.unwrap(), 90_000);
    // A sequential second session (replacement device): 40 s more → 130 s.
    let laptop = w
        .calls()
        .replace_session(&w.cmd, id, phone.id, EndpointCategory::Desktop)
        .await
        .unwrap();
    w.calls()
        .connect_session(&w.cmd, id, laptop.id)
        .await
        .unwrap();
    w.h.clock.advance(sec(40));
    // Overlapping device: a tablet connects for 25 s while the laptop is still connected.
    let tablet = w
        .calls()
        .create_session(&w.cmd, id, p, EndpointCategory::Web)
        .await
        .unwrap();
    w.calls()
        .connect_session(&w.cmd, id, tablet.id)
        .await
        .unwrap();
    w.h.clock.advance(sec(25));
    w.calls()
        .disconnect_session(&w.cmd, id, tablet.id, DisconnectReason::Left)
        .await
        .unwrap();
    // Laptop: 65 s connected so far, still open. Union: 90 + 65 = 155 s (the tablet's 25 s
    // overlap is not double counted), as of now.
    let now = w.h.clock.now();
    assert_eq!(duration(now).await.unwrap(), 155_000);
    // Open interval evaluated at a fixed later instant.
    assert_eq!(duration(now + sec(10)).await.unwrap(), 165_000);
    // And at an earlier instant only what was connected by then counts.
    assert_eq!(duration(now - sec(25)).await.unwrap(), 130_000);

    // The read model agrees: per session sums, per participant union.
    let detail = w.calls().call_detail(w.scope(), id).await.unwrap();
    let ms = |s: calling::calls::ids::CallSessionId| {
        detail
            .sessions
            .iter()
            .find(|d| d.session.id == s)
            .unwrap()
            .connected_duration_ms
    };
    assert_eq!(ms(phone.id), 90_000);
    assert_eq!(ms(laptop.id), 65_000);
    assert_eq!(ms(tablet.id), 25_000);
    let participant = detail
        .participants
        .iter()
        .find(|d| d.participant.id == p)
        .unwrap();
    assert_eq!(participant.connected_duration_ms, 155_000);
    assert_ne!(
        participant.connected_duration_ms,
        (detail.as_of - participant.participant.created_at).num_milliseconds()
    );
}

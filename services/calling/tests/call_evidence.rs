//! Immutable call events and the transactional outbox (Part C §16, Workstream D):
//!
//! * creation writes exactly one Call, one initiator participant, one CALL_REQUESTED
//!   event and one outbox row — or nothing (failures injected into each table);
//! * events are append-only (UPDATE / DELETE / TRUNCATE refused), gap-free per Call and
//!   always describe the Call as persisted;
//! * a state change, its event and its outbox row commit together or not at all — the
//!   database refuses a state without its event and an event without its outbox row;
//! * outbox rows are pending publication records with immutable envelopes.

mod common;

use calling::calls::{kinds::EndpointCategory, CallAction, CallError, CallState};
use common::{
    calls::{sql, CallWorld},
    secret_part,
};
use sea_orm::{ConnectionTrait, TransactionTrait};

const FAIL_FUNCTION: &str =
    "CREATE FUNCTION test_injected_failure() RETURNS trigger LANGUAGE plpgsql AS $$ \
     BEGIN RAISE EXCEPTION 'injected failure' USING ERRCODE = TG_ARGV[0]; END $$";

async fn inject(w: &CallWorld, table: &str, sqlstate: &str) {
    sql(
        &w.h,
        &format!(
            "CREATE TRIGGER test_injected_failure BEFORE INSERT ON {table} FOR EACH ROW \
         EXECUTE FUNCTION test_injected_failure('{sqlstate}')"
        ),
    )
    .await
    .unwrap();
}

async fn heal(w: &CallWorld, table: &str) {
    sql(
        &w.h,
        &format!("DROP TRIGGER test_injected_failure ON {table}"),
    )
    .await
    .unwrap();
}

async fn totals(w: &CallWorld) -> [i64; 4] {
    [
        w.count("SELECT count(*) FROM calls").await,
        w.count("SELECT count(*) FROM call_participants").await,
        w.count("SELECT count(*) FROM call_events").await,
        w.count("SELECT count(*) FROM calling_outbox_events").await,
    ]
}

#[tokio::test]
async fn creation_writes_exactly_the_call_its_initiator_one_event_and_one_outbox_row() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let created = w.create().await;
    let call = &created.call;
    assert_eq!((call.state, call.version), (CallState::Requested, 1));
    assert_eq!(totals(&w).await, [1, 1, 1, 1]);
    let event = &created.event;
    assert_eq!(event.event_type.as_str(), "CALL_REQUESTED");
    assert_eq!((event.sequence, event.call_version), (1, 1));
    assert_eq!(event.to_state, Some(CallState::Requested));
    assert_eq!(event.correlation_id, w.ctx.correlation_id().as_str());
    assert_eq!(event.request_id, w.ctx.request_id().as_str());
    assert_eq!(
        event.data["initiator_participant_id"],
        created.initiator.id.to_string()
    );
    // Everything shares the authenticated scope and correlation context.
    let scope = format!(
        "tenant_id = '{}' AND application_id = '{}' AND environment_id = '{}'",
        w.ctx.tenant_id(),
        w.ctx.application_id(),
        w.ctx.environment_id()
    );
    for table in [
        "calls",
        "call_participants",
        "call_events",
        "calling_outbox_events",
    ] {
        assert_eq!(
            w.count(&format!("SELECT count(*) FROM {table} WHERE {scope}"))
                .await,
            1,
            "{table}"
        );
    }
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM calls WHERE id = '{}' AND created_by_credential_id = '{}' \
             AND created_by_service_account_id = '{}' AND initiator_principal_id = '{}'",
            call.id,
            w.ctx.credential_id(),
            w.ctx.service_account_id(),
            w.initiator.id()
        ))
        .await,
        1
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM call_events WHERE call_id = '{}' AND actor_kind = 'CUSTOMER_APPLICATION' \
             AND actor_credential_id = '{}' AND actor_service_account_id = '{}' AND actor_service IS NULL",
            call.id,
            w.ctx.credential_id(),
            w.ctx.service_account_id()
        ))
        .await,
        1
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM calling_outbox_events o JOIN call_events e ON e.event_id = o.source_event_id \
             WHERE o.aggregate_id = '{}' AND o.event_id = 'evt_' || replace(e.event_id::text, '-', '') \
             AND o.event_type = 'calling.call-requested' AND o.status = 'PENDING' \
             AND o.envelope ->> 'correlation_id' = e.correlation_id \
             AND o.envelope -> 'payload' ->> 'sequence' = '1'",
            call.id
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn a_failed_creation_persists_nothing() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    sql(&w.h, FAIL_FUNCTION).await.unwrap();
    for (table, sqlstate) in [
        ("call_participants", "P0001"),
        ("call_events", "P0001"),
        ("calling_outbox_events", "P0001"),
        ("calling_outbox_events", "23505"),
    ] {
        inject(&w, table, sqlstate).await;
        let err = w
            .calls()
            .create_requested_call(
                &w.ctx,
                &w.initiator,
                calling::calls::service::NewCall::of_kind(calling::calls::kinds::CallKind::Voice),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, CallError::Database { .. } | CallError::Conflict(_)),
            "{err:?}"
        );
        assert_eq!(totals(&w).await, [0, 0, 0, 0], "failure in {table}");
        heal(&w, table).await;
    }
    w.create().await;
    assert_eq!(totals(&w).await, [1, 1, 1, 1]);
}

#[tokio::test]
async fn state_changes_commit_with_their_event_and_outbox_row_or_not_at_all() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    sql(&w.h, FAIL_FUNCTION).await.unwrap();
    let call = w.create().await.call;
    for (table, sqlstate) in [
        ("calling_outbox_events", "P0001"),
        // A duplicate / constraint failure at the outbox must not let the state commit alone.
        ("calling_outbox_events", "23505"),
        ("call_events", "P0001"),
        ("call_events", "23505"),
    ] {
        let before = totals(&w).await;
        inject(&w, table, sqlstate).await;
        assert!(w.act(call.id, CallAction::Cancel).await.is_err());
        heal(&w, table).await;
        let after = w.call(call.id).await;
        assert_eq!(
            (after.state, after.version),
            (CallState::Requested, 1),
            "{table}"
        );
        assert_eq!(totals(&w).await, before, "{table}");
    }
    let outcome = w.act(call.id, CallAction::Cancel).await.unwrap();
    assert_eq!(outcome.call.version, 2);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM calling_outbox_events WHERE source_event_id = '{}'",
            outcome.event.event_id
        ))
        .await,
        1
    );

    // The database itself refuses an event without its outbox row.
    let open = w.create().await.call;
    let txn = w.h.conn().begin().await.unwrap();
    txn.execute_unprepared(&format!(
        "INSERT INTO call_events SELECT gen_random_uuid(), call_id, tenant_id, application_id, environment_id, \
         2, 'CALL_PARTICIPANT_UPDATED', 1, NULL, NULL, 1, actor_kind, actor_service_account_id, actor_credential_id, \
         actor_service, request_id, correlation_id, '{{}}'::jsonb, now(), now() FROM call_events WHERE call_id = '{}'",
        open.id
    ))
    .await
    .unwrap();
    let err = txn.commit().await.unwrap_err().to_string();
    assert!(err.contains("has no outbox event"), "{err}");
    assert_eq!(w.event_count(open.id).await, 1);
}

#[tokio::test]
async fn call_events_are_append_only_gap_free_and_faithful() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let events = w.calls().events(w.scope(), call.id).await.unwrap();
    assert!(events.len() > 7);
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.sequence, i as i64 + 1, "gap-free");
    }
    // Versions only grow and every state event carries the version it produced.
    let mut version = 0;
    for e in &events {
        assert!(e.call_version >= version);
        version = e.call_version;
    }
    assert_eq!(version, call.version);
    // Immutable.
    for statement in [
        format!(
            "UPDATE call_events SET payload = '{{}}'::jsonb WHERE call_id = '{}'",
            call.id
        ),
        format!(
            "UPDATE call_events SET to_state = 'ENDED' WHERE call_id = '{}'",
            call.id
        ),
        format!("DELETE FROM call_events WHERE call_id = '{}'", call.id),
        "TRUNCATE call_events CASCADE".to_string(),
    ] {
        let err = sql(&w.h, &statement).await.unwrap_err();
        assert!(err.contains("immutable evidence"), "{statement}: {err}");
    }
    assert_eq!(w.event_count(call.id).await, events.len() as i64);
    // Sequence collisions and gaps are refused; so is an event that misdescribes the Call.
    let insert = |sequence: i64, version: i64, to_state: &str| {
        format!(
            "INSERT INTO call_events SELECT gen_random_uuid(), call_id, tenant_id, application_id, environment_id, \
             {sequence}, CASE WHEN {to_state} IS NULL THEN 'CALL_PARTICIPANT_UPDATED' ELSE 'CALL_STATE_CHANGED' END, \
             1, CASE WHEN {to_state} IS NULL THEN NULL ELSE 'CONNECTED' END, {to_state}, {version}, actor_kind, \
             actor_service_account_id, actor_credential_id, actor_service, request_id, correlation_id, '{{}}'::jsonb, \
             now(), now() FROM call_events WHERE call_id = '{}' AND sequence = 1",
            call.id
        )
    };
    let next = events.len() as i64 + 1;
    for (statement, needle) in [
        (insert(1, call.version, "NULL"), "increase by one"),
        (insert(next + 3, call.version, "NULL"), "increase by one"),
        (
            insert(next, call.version + 1, "NULL"),
            "describe the call as persisted",
        ),
        (
            insert(next, call.version, "'HELD'"),
            "describe the call as persisted",
        ),
    ] {
        let err = sql(&w.h, &statement).await.unwrap_err();
        assert!(err.contains(needle), "{err}");
    }
    // Normal appends keep working.
    let participant = w.calls().participants(w.scope(), call.id).await.unwrap()[0].id;
    w.calls()
        .create_session(&w.cmd, call.id, participant, EndpointCategory::Server)
        .await
        .unwrap();
    assert_eq!(w.event_count(call.id).await, next);
}

#[tokio::test]
async fn outbox_rows_are_pending_publication_records_with_immutable_envelopes() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Ringing).await;
    let pending = w
        .count(&format!(
            "SELECT count(*) FROM calling_outbox_events WHERE aggregate_id = '{}' AND status = 'PENDING' \
             AND attempt_count = 0 AND published_at IS NULL AND next_attempt_at IS NOT NULL",
            call.id
        ))
        .await;
    assert_eq!(pending, w.event_count(call.id).await);
    for (statement, needle) in [
        (
            format!("UPDATE calling_outbox_events SET envelope = '{{}}'::jsonb WHERE aggregate_id = '{}'", call.id),
            "immutable",
        ),
        (
            format!("DELETE FROM calling_outbox_events WHERE aggregate_id = '{}'", call.id),
            "cannot be deleted",
        ),
    ] {
        let err = sql(&w.h, &statement).await.unwrap_err();
        assert!(err.contains(needle), "{err}");
    }
    // A future publisher may mark rows published, never back to pending.
    sql(
        &w.h,
        &format!(
            "UPDATE calling_outbox_events SET status = 'PUBLISHED', published_at = now(), attempt_count = 1 \
             WHERE aggregate_id = '{}'",
            call.id
        ),
    )
    .await
    .unwrap();
    let err = sql(
        &w.h,
        &format!(
            "UPDATE calling_outbox_events SET status = 'PENDING', published_at = NULL WHERE aggregate_id = '{}'",
            call.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("not allowed") || err.contains("reassigned"),
        "{err}"
    );
}

#[tokio::test]
async fn events_and_outbox_never_carry_secrets_contacts_or_media_identities() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let dump = format!(
        "{}\n{}",
        w.h.conn()
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT e::text FROM call_events e WHERE call_id = '{}'",
                    call.id
                ),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| r.try_get_by_index::<String>(0).unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
        w.h.conn()
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT o::text FROM calling_outbox_events o WHERE aggregate_id = '{}'",
                    call.id
                ),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| r.try_get_by_index::<String>(0).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(!dump.contains(secret_part(w.secret())));
    assert!(!dump.contains("cfmk_"));
    for session in w.calls().sessions(w.scope(), call.id).await.unwrap() {
        assert!(!dump.contains(session.media_identity.as_str()));
    }
    assert!(
        !dump.contains("initiator-1"),
        "principal references stay out of events"
    );
    for forbidden in [
        "phone", "msisdn", "+234", "tel:", "sip:", "provider", "carrier",
    ] {
        assert!(!dump.to_lowercase().contains(forbidden), "{forbidden}");
    }
}

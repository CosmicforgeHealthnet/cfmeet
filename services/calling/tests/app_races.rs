//! Single-winner arbitration and command idempotency under real concurrency, the
//! database-level proof that the single-winner constraint is what holds, and practical
//! load on the critical paths (Part C §14, §57, §100–§103, §126).

mod common;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;
use calling::{calls::CallState, domain::EnvironmentKind, signaling::RealtimeSignalBus};
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
};
use futures_util::future::join_all;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

const ROUNDS: usize = 20;
const DEVICES: usize = 8;

async fn scalar(a: &AppWorld, sql: &str) -> i64 {
    a.h()
        .conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

async fn exec(a: &AppWorld, sql: &str) -> Result<u64, sea_orm::DbErr> {
    a.h()
        .conn()
        .execute(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .map(|r| r.rows_affected())
}

fn call_id(r: &common::TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

/// Every device of `devices` answers at the same time, each with the version it read.
async fn accept_all(a: &AppWorld, call: Uuid, devices: &[Device]) -> Vec<common::TestResponse> {
    let mut offers = Vec::new();
    for d in devices {
        offers.push((d, a.invitation_for(call, d.endpoint).await.unwrap()));
    }
    let (_, version) = a.state(call).await;
    join_all(
        offers
            .iter()
            .map(|(d, offer)| a.accept(call, *offer, d, version)),
    )
    .await
}

#[tokio::test]
async fn concurrent_accepts_have_exactly_one_winner() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "race-caller").await;
    let mut devices = Vec::new();
    for i in 0..DEVICES {
        devices.push(a.device(&a.w.recipient, &format!("race-bob-{i}")).await);
    }
    for round in 0..ROUNDS {
        let r = a
            .create_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&a.w.recipient],
                &format!("race-{round}"),
            )
            .await;
        let call = call_id(&r);
        let results = accept_all(&a, call, &devices).await;
        let winners: Vec<_> = results
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "round {round}: {:?}",
            results.iter().map(|r| &r.text).collect::<Vec<_>>()
        );
        for r in results.iter().filter(|r| r.status != StatusCode::OK) {
            assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
        }
        let q = |sql: &str| sql.replace("$call", &call.to_string());
        assert_eq!(
            scalar(
                &a,
                &q("SELECT count(*) FROM call_answer_claims WHERE call_id = '$call'")
            )
            .await,
            1
        );
        assert_eq!(
            scalar(&a, &q("SELECT count(*) FROM call_invitations WHERE call_id = '$call' AND status = 'ACCEPTED'")).await,
            1
        );
        assert_eq!(
            scalar(&a, &q("SELECT count(*) FROM call_invitations WHERE call_id = '$call' AND status = 'ANSWERED_ELSEWHERE'")).await,
            (DEVICES - 1) as i64
        );
        assert_eq!(
            scalar(&a, &q("SELECT count(*) FROM call_sessions s JOIN call_participants p ON p.id = s.participant_id \
                            WHERE s.call_id = '$call' AND p.role = 'RECIPIENT' AND s.media_access = 'AUTHORIZED'")).await,
            1,
            "round {round}: exactly one recipient device holds media authority"
        );
        // Losing devices cannot get media: they have no session at all.
        let winner_offer = &winners[0].json["invitationId"];
        for d in &devices {
            let offer = a.call_view(call).await["invitations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["endpointId"] == d.endpoint.to_string())
                .cloned()
                .unwrap();
            if &offer["invitationId"] != winner_offer {
                assert!(a.session_on(call, d.endpoint).await.is_none());
            }
        }
        assert_eq!(a.state(call).await.0, CallState::Answered);
    }
}

#[tokio::test]
async fn group_arbitration_holds_per_participant_under_contention() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("race-carol").await;
    let caller = a.device(&a.w.initiator, "group-race-caller").await;
    let mut all: Vec<(Uuid, Device)> = Vec::new();
    for i in 0..3 {
        all.push((
            a.w.recipient.id().as_uuid(),
            a.device(&a.w.recipient, &format!("gr-bob-{i}")).await,
        ));
        all.push((
            carol.id().as_uuid(),
            a.device(&carol, &format!("gr-carol-{i}")).await,
        ));
    }
    for round in 0..5 {
        let r = a
            .create_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&a.w.recipient, &carol],
                &format!("group-race-{round}"),
            )
            .await;
        let call = call_id(&r);
        // Each device keeps answering with the version it last read until it gets a
        // final (non-stale) answer.
        let a = &a;
        let outcomes = join_all(all.iter().map(|(principal, d)| async move {
            let offer = a.invitation_for(call, d.endpoint).await;
            for _ in 0..50 {
                let Some(offer) = offer else {
                    return (*principal, StatusCode::CONFLICT);
                };
                let (_, version) = a.state(call).await;
                let r = a.accept(call, offer, d, version).await;
                if r.json["error"]["code"] != "STALE_CALL_VERSION" {
                    return (*principal, r.status);
                }
            }
            panic!("never settled");
        }))
        .await;
        let mut winners: BTreeMap<Uuid, usize> = BTreeMap::new();
        for (principal, status) in outcomes {
            if status == StatusCode::OK {
                *winners.entry(principal).or_default() += 1;
            }
        }
        assert_eq!(
            winners.values().copied().collect::<Vec<_>>(),
            vec![1, 1],
            "round {round}: {winners:?}"
        );
        assert_eq!(
            scalar(
                a,
                &format!("SELECT count(*) FROM call_answer_claims WHERE call_id = '{call}'")
            )
            .await,
            2
        );
    }
}

#[tokio::test]
async fn accept_racing_cancel_leaves_one_consistent_outcome() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "ac-caller").await;
    let callee = a.device(&a.w.recipient, "ac-callee").await;
    let mut outcomes = BTreeSet::new();
    for round in 0..15 {
        let r = a
            .create_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&a.w.recipient],
                &format!("ac-{round}"),
            )
            .await;
        let call = call_id(&r);
        let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
        let (_, version) = a.state(call).await;
        let (accepted, cancelled) = tokio::join!(
            a.accept(call, offer, &callee, version),
            a.command(call, "cancel", version)
        );
        assert!(
            (accepted.status == StatusCode::OK) != (cancelled.status == StatusCode::OK),
            "exactly one wins: {} / {}",
            accepted.text,
            cancelled.text
        );
        let (state, _) = a.state(call).await;
        let authorized = scalar(
            &a,
            &format!("SELECT count(*) FROM call_sessions s JOIN call_participants p ON p.id = s.participant_id \
                      WHERE s.call_id = '{call}' AND p.role = 'RECIPIENT' AND s.media_access = 'AUTHORIZED'"),
        )
        .await;
        match state {
            CallState::Cancelled => {
                assert_eq!(authorized, 0);
                assert!(!a.media().room_exists(&AppWorld::room(call)));
            }
            CallState::Answered => assert_eq!(authorized, 1),
            other => panic!("unexpected {other:?}"),
        }
        outcomes.insert(format!("{state:?}"));
    }
    eprintln!("accept/cancel race outcomes observed: {outcomes:?}");
}

#[tokio::test]
async fn concurrent_duplicate_commands_take_effect_once() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "dup-caller").await;
    a.device(&a.w.recipient, "dup-callee").await;
    // Ten identical creations at once: one Call, one answer for everyone.
    let results = join_all((0..10).map(|_| async {
        a.create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "dup-create",
        )
        .await
    }))
    .await;
    let ids: BTreeSet<String> = results
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .map(|r| r.json["call"]["callId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "{:?}",
        results
            .iter()
            .map(|r| (&r.status, &r.text))
            .collect::<Vec<_>>()
    );
    for r in &results {
        assert!(
            r.status == StatusCode::CREATED || r.status == StatusCode::CONFLICT,
            "{}",
            r.text
        );
    }
    assert_eq!(scalar(&a, "SELECT count(*) FROM calls").await, 1);
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM call_media_sessions").await,
        1
    );

    // Ten identical holds at once: one transition.
    let (call, _, _) = a.connected_call("VOICE").await;
    let (_, version) = a.state(call).await;
    let world = &a;
    let results = join_all((0..10).map(|_| async move {
        world
            .post_with(
                &format!("/v1/calls/{call}/hold"),
                json!({ "expectedVersion": version }),
                &[("idempotency-key", "dup-hold")],
            )
            .await
    }))
    .await;
    let ok: Vec<_> = results
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .collect();
    assert!(!ok.is_empty());
    for r in &ok {
        assert_eq!(r.json["call"]["state"], "HELD");
        assert_eq!(r.json["call"]["version"], version + 1);
    }
    assert_eq!(a.state(call).await, (CallState::Held, version + 1));
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_events WHERE call_id = '{call}' AND to_state = 'HELD'"
            )
        )
        .await,
        1
    );
}

/// The database itself refuses a second winner — and the constraint is what does it:
/// with `uq_call_answer_claims_participant` dropped (mutation), naive concurrent claims
/// produce two winners for one participant.
#[tokio::test]
async fn the_single_winner_constraint_is_the_database_guarantee() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "db-caller").await;
    let bob_1 = a.device(&a.w.recipient, "db-bob-1").await;
    let bob_2 = a.device(&a.w.recipient, "db-bob-2").await;

    // Two candidate sessions of the same participant and two offers, then two claims
    // inserted concurrently on separate connections.
    async fn contend(
        a: &AppWorld,
        key: &str,
        caller: &Device,
        bob_1: &Device,
        bob_2: &Device,
        guarded: bool,
    ) -> usize {
        let r = a
            .create_call("VOICE", &a.w.initiator, caller, &[&a.w.recipient], key)
            .await;
        let call = call_id(&r);
        let participant = a.participant_of(call, a.w.recipient.id().as_uuid()).await;
        let initiator_session = a.session_on(call, caller.endpoint).await.unwrap();
        let mut pairs = Vec::new();
        for d in [bob_1, bob_2] {
            let offer = a.invitation_for(call, d.endpoint).await.unwrap();
            let session = Uuid::new_v4();
            exec(
                a,
                &format!(
                    "INSERT INTO call_sessions SELECT (json_populate_record(NULL::call_sessions, \
                       (row_to_json(s)::jsonb || jsonb_build_object( \
                         'id', '{session}', 'participant_id', '{participant}', 'endpoint_id', '{}', \
                         'media_identity', 'cfmid_' || md5(random()::text) || md5(random()::text), \
                         'media_access', 'NONE', 'media_generation', 0, 'media_admitted_at', NULL, \
                         'replaces_session_id', NULL))::json)).* \
                     FROM call_sessions s WHERE s.id = '{initiator_session}'",
                    d.endpoint
                ),
            )
            .await
            .unwrap();
            pairs.push((offer, session));
        }
        let on_conflict = if guarded {
            "ON CONFLICT ON CONSTRAINT uq_call_answer_claims_participant DO NOTHING"
        } else {
            ""
        };
        let results = join_all(pairs.iter().map(|(offer, session)| {
            let sql = format!(
                    "INSERT INTO call_answer_claims (id, call_id, participant_id, tenant_id, application_id, environment_id, \
                       winner_invitation_id, winner_session_id, claimed_at, created_at) \
                     SELECT gen_random_uuid(), c.id, '{participant}', c.tenant_id, c.application_id, c.environment_id, \
                            '{offer}', '{session}', now(), now() FROM calls c WHERE c.id = '{call}' {on_conflict}"
            );
            async move { exec(a, &sql).await }
        }))
        .await;
        let _ = results;
        scalar(
            a,
            &format!("SELECT count(*) FROM call_answer_claims WHERE call_id = '{call}'"),
        )
        .await as usize
    }

    for round in 0..10 {
        assert_eq!(
            contend(&a, &format!("db-{round}"), &caller, &bob_1, &bob_2, true).await,
            1
        );
    }
    exec(
        &a,
        "ALTER TABLE call_answer_claims DROP CONSTRAINT uq_call_answer_claims_participant",
    )
    .await
    .unwrap();
    let mutated = contend(&a, "db-mutated", &caller, &bob_1, &bob_2, false).await;
    assert_eq!(
        mutated, 2,
        "without the constraint the database accepts two winners"
    );
}

#[tokio::test]
async fn load_many_calls_across_applications_fanout_credentials_and_reconciliation() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let others = [
        AppWorld {
            w: CallWorld::in_harness(a.h().sibling(), "load-b", EnvironmentKind::Production).await,
        },
        AppWorld {
            w: CallWorld::in_harness(a.h().sibling(), "load-c", EnvironmentKind::Production).await,
        },
    ];
    // Many concurrent Calls across three applications.
    let mut worlds: Vec<&AppWorld> = vec![&a];
    worlds.extend(others.iter());
    let mut setups = Vec::new();
    for (i, w) in worlds.iter().enumerate() {
        let caller = w.device(&w.w.initiator, &format!("load-caller-{i}")).await;
        let callee = w.device(&w.w.recipient, &format!("load-callee-{i}")).await;
        setups.push((*w, caller, callee));
    }
    let created = join_all(setups.iter().flat_map(|(w, caller, _)| {
        (0..15).map(move |n| async move {
            w.create_call(
                "VOICE",
                &w.w.initiator,
                caller,
                &[&w.w.recipient],
                &format!("load-{n}"),
            )
            .await
        })
    }))
    .await;
    assert!(created
        .iter()
        .all(|r| r.status == StatusCode::CREATED && r.json["call"]["state"] == "RINGING"));
    let rooms: BTreeSet<String> = created.iter().map(|r| AppWorld::room(call_id(r))).collect();
    assert_eq!(rooms.len(), 45);
    // Scope isolation under load: each application's calls ring only its own endpoints.
    for (w, _, callee) in &setups {
        let listed = w.get("/v1/calls?limit=100").await;
        assert_eq!(listed.json["data"].as_array().unwrap().len(), 15);
        for r in &created {
            let call = call_id(r);
            let mine = listed.json["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["callId"] == call.to_string());
            if mine {
                assert!(w.invitation_for(call, callee.endpoint).await.is_some());
            } else {
                assert_eq!(
                    w.get(&format!("/v1/calls/{call}")).await.status,
                    StatusCode::NOT_FOUND
                );
            }
        }
    }

    // Signaling fanout burst: a 15-way group call with two devices per target.
    let caller = a.device(&a.w.initiator, "burst-caller").await;
    let mut targets = Vec::new();
    let mut subscriptions = Vec::new();
    for i in 0..15 {
        let p = a.principal(&format!("burst-{i}")).await;
        for k in 0..2 {
            let d = a.device(&p, &format!("burst-{i}-{k}")).await;
            subscriptions.push(a.h().bus.subscribe(d.endpoint));
        }
        targets.push(p);
    }
    let refs: Vec<_> = targets.iter().collect();
    let r = a
        .create_call("VIDEO", &a.w.initiator, &caller, &refs, "burst")
        .await;
    assert_eq!(r.json["liveInvitationCount"], 30, "{}", r.text);
    for mut s in subscriptions {
        let mut incoming = 0;
        while let Some(signal) = s.try_recv() {
            if signal["type"] == "call.incoming" {
                incoming += 1;
            }
        }
        assert_eq!(
            incoming, 1,
            "every endpoint is offered the call exactly once"
        );
    }

    // Credential burst on one session: every issuance is recorded, none lost.
    let (call, _, callee) = a.connected_call("VOICE").await;
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    let before = scalar(
        &a,
        &format!("SELECT media_generation::bigint FROM call_sessions WHERE id = '{session}'"),
    )
    .await;
    let burst = join_all((0..25).map(|_| a.media_credential(call, session))).await;
    assert!(burst.iter().all(|r| r.status == StatusCode::OK));
    let tokens: BTreeSet<&str> = burst
        .iter()
        .map(|r| r.json["token"].as_str().unwrap())
        .collect();
    assert_eq!(tokens.len(), 25);
    let after = scalar(
        &a,
        &format!("SELECT media_generation::bigint FROM call_sessions WHERE id = '{session}'"),
    )
    .await;
    assert_eq!(after - before, 25, "no lost generation update");

    // Reconciliation batch: drift on many Calls is repaired in one sweep.
    let mut drifted = Vec::new();
    for _ in 0..10 {
        let (call, _, _) = a.connected_call("VOICE").await;
        let room = AppWorld::room(call);
        for p in a.media().participants(&room) {
            let mut drift = p.permissions;
            drift.can_publish_video = true;
            a.media().force_permissions(&room, &p.identity, drift);
        }
        drifted.push(room);
    }
    a.h().clock.advance(chrono::Duration::seconds(30));
    a.h().state.app.reconcile_due(500).await.unwrap();
    for room in drifted {
        assert!(a
            .media()
            .participants(&room)
            .iter()
            .all(|p| !p.permissions.can_publish_video));
    }
}

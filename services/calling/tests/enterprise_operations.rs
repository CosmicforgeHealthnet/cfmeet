//! Part F cross-cutting operations (§146–§148, §165–§168): enterprise routing decisions
//! are durable outbox events, operator incidents are raised (deduplicated, bounded,
//! restricted evidence) for repeated loop blocks, IVR routing corruption and stuck queue
//! entries, and the enterprise metrics stay bounded (no ids, numbers or digits as labels).

mod common;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use common::{
    app::Device,
    enterprise::{id_of, strings, EnterpriseWorld},
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

struct Desk {
    principal: Principal,
    phone: Device,
}

async fn desk(e: &EnterpriseWorld, name: &str) -> Desk {
    let principal = e.member(name).await;
    let phone = e.a.device(&principal, &format!("{name}-desk")).await;
    Desk { principal, phone }
}

async fn dial(e: &EnterpriseWorld, from: &Desk, number: &str) -> Uuid {
    let r = e
        .call_extension(
            "VOICE",
            &from.principal,
            &from.phone,
            number,
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

fn dest(kind: &str, id: impl std::fmt::Display) -> Value {
    json!({ "type": kind, "id": id.to_string() })
}

async fn incidents(e: &EnterpriseWorld, category: &str) -> Vec<Value> {
    strings(
        e.h().conn(),
        &format!(
            "SELECT json_build_object('status', status, 'severity', severity, 'occurrences', occurrences, \
               'resourceType', resource_type, 'resourceId', resource_id, 'callId', call_id, \
               'evidence', restricted_evidence, 'key', dedupe_key)::text \
               FROM calling_incidents WHERE category = '{category}' ORDER BY first_seen_at"
        ),
    )
    .await
    .iter()
    .map(|s| serde_json::from_str(s).unwrap())
    .collect()
}

/// §147: every routing step of an enterprise Call is a durable Call event in the outbox,
/// written in the same transaction as its evidence and valid against the shared contracts.
#[tokio::test]
async fn routing_steps_are_durable_outbox_events() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    e.extension("5100", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let call = dial(&e, &caller, "5100").await;
    let steps = e
        .count(&format!(
            "SELECT count(*) FROM enterprise_routing_steps WHERE call_id = '{call}'"
        ))
        .await;
    assert!(steps >= 1);
    let events = strings(
        e.h().conn(),
        &format!(
            "SELECT payload::text FROM call_events WHERE call_id = '{call}' \
               AND event_type = 'ENTERPRISE_ROUTING_STEP' ORDER BY sequence"
        ),
    )
    .await;
    assert_eq!(events.len() as i64, steps, "one event per routing step");
    let evidence = strings(
        e.h().conn(),
        &format!(
            "SELECT decision || ':' || reason FROM enterprise_routing_steps \
               WHERE call_id = '{call}' ORDER BY step_number"
        ),
    )
    .await;
    for (i, (event, step)) in events.iter().zip(&evidence).enumerate() {
        let data: Value = serde_json::from_str(event).unwrap();
        assert_eq!(data["step_number"], i as i64 + 1);
        assert_eq!(
            format!(
                "{}:{}",
                data["decision"].as_str().unwrap(),
                data["reason"].as_str().unwrap()
            ),
            *step
        );
    }
    let envelopes = strings(
        e.h().conn(),
        &format!(
            "SELECT envelope::text FROM calling_outbox_events \
               WHERE event_type = 'calling.enterprise-routing-step' AND aggregate_id = '{call}'"
        ),
    )
    .await;
    assert_eq!(envelopes.len() as i64, steps);
    for text in envelopes {
        let envelope: Value = serde_json::from_str(&text).unwrap();
        common::contracts::check("platform/events/envelope/v1.schema.json", &envelope);
        common::contracts::check(
            "calling/events/call-lifecycle/v1.schema.json",
            &envelope["payload"],
        );
        assert_eq!(envelope["payload"]["event_type"], "ENTERPRISE_ROUTING_STEP");
    }
}

/// §168: a forwarding loop that keeps being blocked is ONE open incident whose
/// occurrences count the repeats; its evidence is bounded and restricted.
#[tokio::test]
async fn repeated_loop_blocks_are_one_incident() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let b = desk(&e, "b").await;
    let ea = e
        .extension("6501", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    let eb = e
        .extension("6502", "ENTERPRISE_MEMBER", b.principal.id())
        .await;
    for (from, to) in [(&a, &eb), (&b, &ea)] {
        let r = e
            .admin(
                Method::POST,
                "/v1/enterprise/forwarding-rules",
                Some(
                    json!({ "principalId": from.principal.id(), "condition": "ALWAYS",
                             "destination": dest("EXTENSION", id_of(to, "extensionId")) }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let first = dial(&e, &caller, "6501").await;
    let second = dial(&e, &caller, "6501").await;
    for call in [first, second] {
        assert_eq!(e.a.state(call).await.0, CallState::Failed);
    }
    let open = incidents(&e, "FORWARDING_LOOP_REPEATED").await;
    assert_eq!(open.len(), 1, "{open:?}");
    let incident = &open[0];
    assert_eq!(incident["status"], "OPEN");
    assert_eq!(incident["occurrences"], 2);
    assert_eq!(incident["resourceType"], "DESTINATION");
    assert_eq!(incident["evidence"]["reason"], "LOOP_BLOCKED");
    // The first observation keeps its Call; repeats count.
    assert_eq!(incident["callId"], first.to_string());
    // Random identifiers (hyphenated or simple, e.g. inside `key`) can contain "650"; drop
    // the incident's own ids so the check only sees the remaining evidence.
    let mut text = incident.to_string();
    for field in ["callId", "resourceId"] {
        let id = incident[field].as_str().unwrap().to_owned();
        text = text.replace(&id, "").replace(&id.replace('-', ""), "");
    }
    assert!(
        !text.contains("650"),
        "no extension number in evidence: {text}"
    );
    assert!(
        calling::metrics::counter(
            "calling_enterprise_incidents_total",
            &[("category", "FORWARDING_LOOP_REPEATED")]
        ) >= 2
    );
}

/// §168: a pinned IVR version naming a node that does not exist is routing corruption —
/// the Call ends safely and an operator incident is raised.
#[tokio::test]
async fn ivr_routing_corruption_is_an_incident() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    let target = e
        .extension("7300", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let flow = e
        .create("/v1/enterprise/ivr-flows", json!({ "name": "Front menu" }))
        .await;
    let flow_id = id_of(&flow, "ivrFlowId");
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{flow_id}/versions"),
            Some(json!({ "graph": { "startNode": "main", "nodes": [
                { "id": "main", "kind": "MENU", "prompt": { "builtin": "MENU" },
                  "inputTimeoutSeconds": 5, "maxRetries": 1,
                  "options": [{ "digit": "1", "next": "sales" }],
                  "onInvalid": "bye", "onTimeout": "bye" },
                { "id": "sales", "kind": "ROUTE_EXTENSION", "extensionId": id_of(&target, "extensionId") },
                { "id": "bye", "kind": "HANGUP", "prompt": { "builtin": "GOODBYE" } },
            ]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{flow_id}/activation"),
            Some(json!({ "expectedVersion": 2, "versionNumber": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("7301", "IVR", flow_id).await;
    let call = dial(&e, &caller, "7301").await;
    // Corrupt the immutable version underneath the running session (what the
    // validation and the immutability trigger make impossible in production).
    let conn = e.h().conn();
    let txn = conn.begin().await.unwrap();
    txn.execute_unprepared(
        "ALTER TABLE enterprise_ivr_flow_versions DISABLE TRIGGER USER; \
         UPDATE enterprise_ivr_flow_versions \
            SET graph = jsonb_set(graph, '{nodes,0,options,0,next}', '\"ghost\"'); \
         ALTER TABLE enterprise_ivr_flow_versions ENABLE TRIGGER USER",
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let r = e
        .send(
            e.caller_token(),
            Method::POST,
            &format!("/v1/calls/{call}/ivr/input"),
            Some(json!({ "endpointId": caller.phone.endpoint, "digit": "1" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let found = incidents(&e, "IVR_ROUTING_CORRUPTION").await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["severity"], "HIGH");
    assert_eq!(found[0]["resourceType"], "IVR_SESSION");
    assert_eq!(found[0]["callId"], call.to_string());
    // The caller's key never appears in the incident.
    assert!(!found[0]["evidence"].to_string().contains("\"1\""));
    // The member was never rung through the corrupted branch.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}'"
        ))
        .await,
        0
    );
}

/// §168 / §156: an entry the worker cannot process past its deadline (its Call stays
/// locked) is a stuck entry; once released, the worker drains it normally.
#[tokio::test]
async fn a_stuck_queue_entry_is_an_incident_and_drains_later() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    // An agent with no device: never offered, the entry just waits.
    let agent = e.member("agent-without-device").await;
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": "Support line", "maxWaitSeconds": 30, "offerTimeoutSeconds": 10,
                    "capacity": 5 }),
        )
        .await;
    let queue_id = id_of(&q, "queueId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{queue_id}/members"),
            Some(json!({ "expectedVersion": q["version"],
                         "members": [{ "principalId": agent.id().to_string() }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("8400", "QUEUE", queue_id).await;
    let call = dial(&e, &caller, "8400").await;
    e.h().state.app.expire_due(100).await.unwrap();
    assert!(calling::metrics::gauge("calling_enterprise_queue_depth", &[]).unwrap_or(0) >= 1);
    e.h().clock.advance(chrono::Duration::seconds(30 + 121));
    // Another instance holds the Call row (crashed mid-transaction, long lock).
    let conn = e.h().conn();
    let holder = conn.begin().await.unwrap();
    holder
        .execute_unprepared(&format!(
            "SELECT 1 FROM calls WHERE id = '{call}' FOR UPDATE"
        ))
        .await
        .unwrap();
    e.h().state.app.expire_due(100).await.unwrap();
    let stuck = incidents(&e, "STUCK_QUEUE_ENTRY").await;
    assert_eq!(stuck.len(), 1, "{stuck:?}");
    assert_eq!(stuck[0]["resourceType"], "QUEUE_ENTRY");
    assert_eq!(stuck[0]["evidence"]["entryState"], "WAITING");
    holder.rollback().await.unwrap();
    // The lock is gone: the worker drains the entry (no duplicate effects).
    e.h().state.app.expire_due(100).await.unwrap();
    let state = strings(
        e.h().conn(),
        &format!("SELECT state FROM enterprise_queue_entries WHERE call_id = '{call}'"),
    )
    .await;
    assert_eq!(state.len(), 1);
    assert_ne!(state[0], "WAITING");
    assert!(e.a.state(call).await.0.is_terminal());
}

/// §165 / §166: the missed-call counter counts canonical misses, and no metric label
/// carries an id, a number, a key sequence or an object key.
#[tokio::test]
async fn metrics_are_bounded() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    e.extension("5200", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let before = calling::metrics::counter("calling_missed_calls_total", &[("call_kind", "VOICE")]);
    let call = dial(&e, &caller, "5200").await;
    e.h().clock.advance(chrono::Duration::seconds(46));
    e.h().state.app.expire_due(100).await.unwrap();
    assert!(e.a.state(call).await.0.is_terminal());
    assert!(
        calling::metrics::counter("calling_missed_calls_total", &[("call_kind", "VOICE")]) > before
    );
    let rendered = calling::metrics::render();
    for line in rendered.lines().filter(|l| !l.starts_with('#')) {
        let Some(start) = line.find('{') else {
            continue;
        };
        let labels = &line[start + 1..line.rfind('}').unwrap()];
        for pair in labels.split(',') {
            let (key, value) = pair.split_once('=').unwrap();
            let value = value.trim_matches('"');
            assert!(
                [
                    "artifact",
                    "call_kind",
                    "category",
                    "channel",
                    "condition",
                    "decision",
                    "destination",
                    "kind",
                    "operation",
                    "outcome",
                    "reason",
                    "resource",
                    "result",
                    "route_class",
                    "route_type",
                    "source",
                    "status",
                    "strategy",
                    "target_type",
                    "le",
                ]
                .contains(&key),
                "unexpected label {key} in {line}"
            );
            assert!(
                Uuid::parse_str(value).is_err()
                    && !value.contains('/')
                    && !value.chars().filter(|c| c.is_ascii_digit()).count().gt(&4)
                    || key == "le",
                "unbounded label value {key}={value}"
            );
        }
    }
}

/// §191: a database failure inside the routing transaction leaves NO authoritative effect
/// (no Call, offer, routing step or outbox event); the next attempt routes normally.
#[tokio::test]
async fn a_database_failure_mid_routing_leaves_no_effect() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    e.extension("5300", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let counts = |e: &EnterpriseWorld| {
        let conn = e.h().conn().clone();
        async move {
            let mut out = Vec::new();
            for table in [
                "calls",
                "call_invitations",
                "enterprise_routing_steps",
                "enterprise_call_contexts",
                "calling_outbox_events",
            ] {
                out.push(common::scalar_i64(&conn, &format!("SELECT count(*) FROM {table}")).await);
            }
            out
        }
    };
    let before = counts(&e).await;
    // The database refuses the routing evidence (a failing disk / constraint / failover).
    common::exec(
        e.h().conn(),
        "CREATE FUNCTION test_fail_routing() RETURNS trigger AS $$ BEGIN \
           RAISE EXCEPTION 'injected failure'; END $$ LANGUAGE plpgsql; \
         CREATE TRIGGER trg_test_fail_routing BEFORE INSERT ON enterprise_routing_steps \
           FOR EACH ROW EXECUTE FUNCTION test_fail_routing()",
    )
    .await
    .unwrap();
    let r = e
        .call_extension(
            "VOICE",
            &caller.principal,
            &caller.phone,
            "5300",
            "db-fail-1",
        )
        .await;
    assert!(r.status.is_server_error(), "{}", r.text);
    assert_eq!(counts(&e).await, before, "nothing was committed");
    common::exec(
        e.h().conn(),
        "DROP TRIGGER trg_test_fail_routing ON enterprise_routing_steps; \
         DROP FUNCTION test_fail_routing()",
    )
    .await
    .unwrap();
    // The same request (same Idempotency-Key) now succeeds: nothing half-done blocks it.
    let r = e
        .call_extension(
            "VOICE",
            &caller.principal,
            &caller.phone,
            "5300",
            "db-fail-1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(e
        .a
        .invitation_for(call, member.phone.endpoint)
        .await
        .is_some());
}

/// §192 / §193: with Telephony (and so the SIP edge) unreachable, application enterprise
/// Calls are unaffected; a SIP-dependent leg's commands stay durable and retried — the
/// Call is never corrupted and can always be ended.
#[tokio::test]
async fn telephony_down_degrades_only_sip_paths() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    // This world's Telephony is unconfigured: every Telephony call fails.
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    e.extension("5400", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let call = dial(&e, &caller, "5400").await;
    let offer =
        e.a.invitation_for(call, member.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, offer, &member.phone, v).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "app enterprise call answered: {}",
        r.text
    );

    // A desk phone's dialog (offered by Telephony before it went down) into the IVR-less
    // member extension: the Call exists, its edge commands wait durably.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/sip-devices",
            Some(
                json!({ "deviceRef": "dev-ops-down-01", "principalId": caller.principal.id(),
                         "displayLabel": "Ops desk" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // The environment's owner of Calls not created through the API.
    let credential = e.a.w.token.credential.id.as_uuid();
    common::exec(
        e.h().conn(),
        &format!(
            "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, \
               service_account_id, credential_id, enabled, revision, created_at, updated_at) \
             SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, \
               now(), now() FROM platform_api_credentials WHERE id = '{credential}'"
        ),
    )
    .await
    .unwrap();
    let scope = e.a.w.consumer.env(e.a.w.kind);
    let offer = calling::calls::app::enterprise::EnterpriseDialogOffer {
        schema_version: "1".into(),
        telephony_leg_id: Uuid::new_v4(),
        correlation_id: format!("corr_{}", Uuid::new_v4().simple()),
        scope: calling::calls::app::enterprise::OfferScope {
            tenant_id: e.a.w.consumer.tenant.id.as_uuid(),
            application_id: e.a.w.consumer.application.id.as_uuid(),
            environment_id: scope.id.as_uuid(),
        },
        device_ref: "dev-ops-down-01".into(),
        extension: "5400".into(),
        max_duration_seconds: 600,
    };
    let answer = e
        .h()
        .state
        .app
        .accept_enterprise_offer(&offer)
        .await
        .unwrap();
    let sip_call: Uuid = answer["callId"].as_str().unwrap().parse().unwrap();
    // The member answers on the application: the edge must now answer / bridge the leg.
    let sip_offer =
        e.a.invitation_for(sip_call, member.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(sip_call).await;
    let r = e.a.accept(sip_call, sip_offer, &member.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for _ in 0..3 {
        e.h()
            .state
            .app
            .dispatch_telephony_commands(100)
            .await
            .unwrap();
        e.h().clock.advance(chrono::Duration::seconds(30));
    }
    let commands = |e: &EnterpriseWorld| {
        let conn = e.h().conn().clone();
        async move {
            strings(
                &conn,
                &format!(
                    "SELECT kind || ':' || status FROM call_telephony_commands \
                      WHERE call_id = '{sip_call}' ORDER BY created_at, idempotency_key"
                ),
            )
            .await
        }
    };
    // The bridge waits durably (attempted, retried with backoff), the Call is answered but
    // never claimed CONNECTED without media evidence.
    assert_eq!(commands(&e).await, ["BRIDGE:PENDING"]);
    assert!(
        e.count(&format!(
            "SELECT max(attempts)::bigint FROM call_telephony_commands WHERE call_id = '{sip_call}'"
        ))
        .await
            >= 2
    );
    assert_eq!(e.a.state(sip_call).await.0, CallState::Answered);
    // Ending the SIP Call is never blocked by the outage; the edge cleanup waits durably.
    let (_, v) = e.a.state(sip_call).await;
    let r = e.a.command(sip_call, "end", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.h()
        .state
        .app
        .dispatch_telephony_commands(100)
        .await
        .unwrap();
    let after = commands(&e).await;
    assert!(
        after.iter().any(|c| c.starts_with("TERMINATE:")),
        "a durable terminate for the edge leg: {after:?}"
    );
    assert!(matches!(
        e.a.state(sip_call).await.0,
        CallState::Ending | CallState::Ended
    ));
    // The application Call is still answered and intact.
    assert!(matches!(
        e.a.state(call).await.0,
        CallState::Answered | CallState::Connected
    ));
}

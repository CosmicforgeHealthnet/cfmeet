//! Part F §54 extensions and internal calling: the organization-scoped extension
//! namespace, number format and ambiguity rules, typed destinations, offboarding, and
//! extension-to-extension calling on ONE canonical Call over ENTERPRISE_INTERNAL — no
//! public-network leg, no Telephony command, no spend.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    domain::{EnvironmentKind, PrincipalStatus},
    enterprise::{policy::AdminCapability, EnterpriseFlags},
    platform::scopes::names,
    routing::{policy::RoutingPolicy, store, RoutingClass},
};
use common::enterprise::{credential, default_rules, id_of, EnterpriseWorld};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn extension_namespace_format_and_destinations() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.a.principal("bob").await;
    let ext = e.extension("1001", "ENTERPRISE_MEMBER", alice.id()).await;
    assert_eq!(ext["extensionNumber"], "1001");
    assert_eq!(ext["status"], "ACTIVE");
    assert_eq!(ext["version"], 1);
    assert_eq!(ext["destination"]["type"], "ENTERPRISE_MEMBER");
    common::enterprise::conforms("Extension", &ext);
    let ext_b = e.extension("2002", "PRINCIPAL", bob.id()).await;

    let post = |number: &str, kind: &str, id: Uuid| json!({ "extensionNumber": number, "displayLabel": "x", "destination": { "type": kind, "id": id } });
    for (body, status, what) in [
        (
            post("1001", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::CONFLICT,
            "duplicate",
        ),
        (
            post("100", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::CONFLICT,
            "prefix of 1001",
        ),
        (
            post("10010", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::CONFLICT,
            "1001 is its prefix",
        ),
        (
            post("12", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::BAD_REQUEST,
            "too short",
        ),
        (
            post("+1003", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::BAD_REQUEST,
            "plus",
        ),
        (
            post("1234567", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::BAD_REQUEST,
            "phone-like",
        ),
        (
            post("sales", "PRINCIPAL", bob.id().as_uuid()),
            StatusCode::BAD_REQUEST,
            "word in numeric namespace",
        ),
        (
            post("3003", "PRINCIPAL", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
            "unknown principal",
        ),
        (
            post("3003", "ENTERPRISE_MEMBER", bob.id().as_uuid()),
            StatusCode::NOT_FOUND,
            "not a member",
        ),
        (
            post("3003", "RING_GROUP", Uuid::new_v4()),
            StatusCode::BAD_REQUEST,
            "not yet a target",
        ),
        (
            post("3003", "PUBLIC_CONTACT", Uuid::new_v4()),
            StatusCode::BAD_REQUEST,
            "raw public target",
        ),
        (
            json!({ "extensionNumber": "3003", "displayLabel": "x", "destination": { "type": "PRINCIPAL", "id": bob.id(), "number": "+2348000000000" } }),
            StatusCode::BAD_REQUEST,
            "number field",
        ),
        (
            json!({ "extensionNumber": "3003", "displayLabel": "+2348012345678", "destination": { "type": "PRINCIPAL", "id": bob.id() } }),
            StatusCode::BAD_REQUEST,
            "number as label",
        ),
    ] {
        let r = e
            .admin(Method::POST, "/v1/enterprise/extensions", Some(body))
            .await;
        assert_eq!(r.status, status, "{what}: {}", r.text);
    }

    // An alphanumeric namespace through a named policy: case-insensitive, unique.
    let mut rules = default_rules();
    rules["extensionFormat"] = json!("ALPHANUMERIC");
    rules["extensionMinLength"] = json!(2);
    rules["extensionMaxLength"] = json!(32);
    let named = e
        .create(
            "/v1/enterprise/policies",
            json!({ "name": "Words", "rules": rules }),
        )
        .await;
    let policy = id_of(&named, "policyId");
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "Sales", "displayLabel": "Sales desk",
                          "destination": { "type": "PRINCIPAL", "id": bob.id() }, "policyId": policy })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["extensionNumber"], "sales");
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "SALES", "displayLabel": "dup",
                          "destination": { "type": "PRINCIPAL", "id": bob.id() }, "policyId": policy })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // A policy named by an active extension cannot be disabled.
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/policies/{policy}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // Renumbering with versions; stale is refused.
    let id = id_of(&ext_b, "extensionId");
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/extensions/{id}"),
            Some(json!({ "expectedVersion": 1, "extensionNumber": "2003", "displayLabel": "Bob" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["extensionNumber"], "2003");
    assert_eq!(r.json["version"], 2);
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/extensions/{id}"),
            Some(json!({ "expectedVersion": 1, "displayLabel": "stale" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "VERSION_CONFLICT", "{}", r.text);

    // Disable frees the number; disabled cannot change.
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/extensions/{id}?expectedVersion=2"),
            None,
        )
        .await;
    assert_eq!(r.json["status"], "DISABLED", "{}", r.text);
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/extensions/{id}"),
            Some(json!({ "expectedVersion": 3, "displayLabel": "again" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    e.extension("2003", "PRINCIPAL", bob.id()).await;

    // Listing with filters.
    let r = e
        .admin(
            Method::GET,
            "/v1/enterprise/extensions?status=ACTIVE&destinationType=PRINCIPAL",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let mut numbers: Vec<&str> = r.json["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["extensionNumber"].as_str().unwrap())
        .collect();
    numbers.sort();
    assert_eq!(numbers, ["2003", "sales"]);
    common::enterprise::conforms("ExtensionList", &r.json);

    // The SAME number in another tenant is another namespace.
    let (other, other_admin) = e.other_tenant("globex").await;
    let (carol, _) = e
        .h()
        .platform
        .register_principal(
            other.env(e.a.w.kind).id,
            calling::platform::principals::NewPrincipal::External {
                kind: calling::domain::PrincipalKind::EnterpriseMember,
                external_reference: "carol".into(),
            },
        )
        .await
        .unwrap();
    let r = e
        .send(
            other_admin.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "1001", "displayLabel": "Carol",
                          "destination": { "type": "ENTERPRISE_MEMBER", "id": carol.id } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // ... and cannot point at OUR principal.
    let r = e
        .send(
            other_admin.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "1002", "displayLabel": "Alice?",
                          "destination": { "type": "ENTERPRISE_MEMBER", "id": alice.id() } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // The database refuses a cross-scope destination even from a buggy writer.
    let ours = e.a.w.scope();
    let sql = format!(
        "UPDATE enterprise_extensions SET destination_id = '{}', version = version + 1 WHERE id = '{}'",
        carol.id,
        id_of(&ext, "extensionId")
    );
    assert!(common::exec(e.h().conn(), &sql).await.is_err());
    let _ = ours;
}

#[tokio::test]
async fn extension_ids_of_another_scope_are_not_found() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let ext = e.extension("1001", "ENTERPRISE_MEMBER", alice.id()).await;
    let id = id_of(&ext, "extensionId");
    let (_, other_admin) = e.other_tenant("globex").await;
    let sandbox_admin = credential(
        e.h(),
        &e.a.w.consumer,
        EnvironmentKind::Sandbox,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    for token in [
        other_admin.secret.expose_once(),
        sandbox_admin.secret.expose_once(),
    ] {
        for (method, path, body) in [
            (Method::GET, format!("/v1/enterprise/extensions/{id}"), None),
            (
                Method::PATCH,
                format!("/v1/enterprise/extensions/{id}"),
                Some(json!({ "expectedVersion": 1, "displayLabel": "hijack" })),
            ),
            (
                Method::DELETE,
                format!("/v1/enterprise/extensions/{id}?expectedVersion=1"),
                None,
            ),
        ] {
            let r = e.send(token, method.clone(), &path, body).await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "{method} {path}: {}",
                r.text
            );
        }
        let r = e
            .send(token, Method::GET, "/v1/enterprise/extensions", None)
            .await;
        assert!(!r.text.contains(&id.to_string()));
    }
    // The organization's capability is a second key: without MANAGE_ROUTING no
    // routing configuration, but disabling (restrictive) still works.
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(e.a.w.scope(), vec![AdminCapability::ManageVoicemail])
        .await
        .unwrap();
    let bob = e.a.principal("bob").await;
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "2002", "displayLabel": "Bob",
                          "destination": { "type": "PRINCIPAL", "id": bob.id() } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "ENTERPRISE_POLICY_DENIED");
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/extensions/{id}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

/// §35 internal certification (test media provider): Extension 1001 → Extension 1002.
#[tokio::test]
async fn extension_to_extension_is_one_internal_call() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    e.extension("1001", "ENTERPRISE_MEMBER", alice.id()).await;
    e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;
    let bob_phone = e.a.device(&bob, "bob-mobile").await;
    let bob_laptop = e.a.device(&bob, "bob-laptop").await;

    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "ext-call-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "RINGING");
    assert_eq!(r.json["enterprise"]["extensionNumber"], "1002");
    assert_eq!(r.json["enterprise"]["phase"], "RINGING");
    assert_eq!(
        r.json["liveInvitationCount"], 2,
        "both of Bob's devices ring"
    );
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/CreateCallResult",
        &r.json,
    );
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/CreateCallRequest",
        &json!({ "kind": "VOICE", "initiatorPrincipalId": alice.id(),
                 "initiatorEndpointId": alice_phone.endpoint, "targetExtension": "1002" }),
    );
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // Idempotent replay: the same Call, no second one.
    let again = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "ext-call-1")
        .await;
    assert_eq!(again.json["call"]["callId"], r.json["call"]["callId"]);
    assert_eq!(again.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 1);

    // ENTERPRISE_INTERNAL over PRIVATE_NETWORK: no public party, no Telephony command,
    // no spend, no public bridge.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_routes WHERE call_id = '{call}' \
             AND routing_class = 'ENTERPRISE_INTERNAL' AND route_class = 'PRIVATE_NETWORK'"
        ))
        .await,
        1
    );
    for table in [
        "call_public_parties",
        "call_telephony_commands",
        "call_spend_reservations",
        "call_public_bridges",
    ] {
        assert_eq!(
            e.count(&format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_authorizations WHERE call_id = '{call}' \
             AND policy_version = 'ENTERPRISE_INTERNAL_V1' AND decision = 'APPROVED'"
        ))
        .await,
        1
    );

    // Bob answers on the laptop; the mobile is answered-elsewhere.
    let invitation = e.a.invitation_for(call, bob_laptop.endpoint).await.unwrap();
    let (_, version) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, &bob_laptop, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let view = e.a.call_view(call).await;
    let statuses: Vec<(&str, &str)> = view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["endpointId"].as_str().unwrap(),
                i["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(statuses.contains(&(&bob_phone.endpoint.to_string()[..], "ANSWERED_ELSEWHERE")));
    let caller_session = e.a.session_on(call, alice_phone.endpoint).await.unwrap();
    let callee_session = e.a.session_on(call, bob_laptop.endpoint).await.unwrap();
    e.a.connect(call, caller_session).await;
    e.a.connect(call, callee_session).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);

    // End safely.
    let (_, version) = e.a.state(call).await;
    let r = e.a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (state, _) = e.a.state(call).await;
    assert!(
        matches!(state, CallState::Ending | CallState::Ended),
        "{state}"
    );

    // History on both participants, the enterprise context and its evidence.
    let view = e.a.call_view(call).await;
    let principals: Vec<&str> = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["principalId"].as_str().unwrap())
        .collect();
    assert!(principals.contains(&&alice.id().to_string()[..]));
    assert!(principals.contains(&&bob.id().to_string()[..]));
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' \
             AND answered_principal_id = '{}' AND outcome = 'ANSWERED' AND entry_extension_number = '1002'",
            bob.id()
        ))
        .await,
        1
    );
    let steps = common::enterprise::strings(
        e.h().conn(),
        &format!(
            "SELECT decision FROM enterprise_routing_steps WHERE call_id = '{call}' ORDER BY step_number"
        ),
    )
    .await;
    assert_eq!(steps, ["ENTER", "RING_PRINCIPAL", "ANSWERED"]);
}

#[tokio::test]
async fn extension_calls_are_scoped_and_policy_gated() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    e.extension("1001", "ENTERPRISE_MEMBER", alice.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;

    // Unknown number: 404 and nothing persisted.
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1999", "k-unknown")
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // Calling one's own extension destination.
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1001", "k-self")
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Without enterprise.calls.use: the plain calls.* credential is refused.
    let r =
        e.a.post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": alice.id(),
                    "initiatorEndpointId": alice_phone.endpoint, "targetExtension": "1002" }),
            &[("idempotency-key", "k-scope")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    // Mixed targets are refused.
    let r = e
        .h()
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(e.caller_token()),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": alice.id(),
                         "initiatorEndpointId": alice_phone.endpoint, "targetExtension": "1002",
                         "targetPrincipalIds": [bob.id()] })),
            &[("idempotency-key", "k-mixed")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 0);

    // Another tenant's caller cannot reach "1002" of ours.
    let (other, _) = e.other_tenant("globex").await;
    let other_caller = credential(
        e.h(),
        &other,
        e.a.w.kind,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
            names::PRINCIPALS_WRITE,
        ],
    )
    .await;
    let (eve, _) = e
        .h()
        .platform
        .register_principal(
            other.env(e.a.w.kind).id,
            calling::platform::principals::NewPrincipal::External {
                kind: calling::domain::PrincipalKind::EnterpriseMember,
                external_reference: "eve".into(),
            },
        )
        .await
        .unwrap();
    let r = e
        .send(
            other_caller.secret.expose_once(),
            Method::POST,
            "/v1/calling/endpoints",
            Some(
                json!({ "principalId": eve.id, "endpointKey": "eve-desk", "endpointKind": "WEB",
                         "capabilities": { "audio": true, "video": false } }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let eve_endpoint = r.json["endpointId"].as_str().unwrap().to_string();
    let r = e
        .h()
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(other_caller.secret.expose_once()),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": eve.id,
                         "initiatorEndpointId": eve_endpoint, "targetExtension": "1002" })),
            &[("idempotency-key", "k-foreign")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // Policy: internal calling disabled in the baseline → 403, no Call.
    let mut rules = default_rules();
    rules["internalCallingEnabled"] = json!(false);
    e.create(
        "/v1/enterprise/policies",
        json!({ "name": "Closed", "isDefault": true, "rules": rules }),
    )
    .await;
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "k-policy")
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED");
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 0);
}

#[tokio::test]
async fn routing_policy_and_kill_switch_gate_enterprise_routing() {
    let Some(e) = EnterpriseWorld::with_flags(EnterpriseFlags::default()).await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    // Configuration works with the flag off (configure before enabling).
    e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "k-flag")
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");

    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;
    e.a.device(&bob, "bob-desk").await;
    // An environment routing policy that does not allow ENTERPRISE_INTERNAL.
    let scope = e.a.w.scope();
    store::store_policy(
        e.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc],
            ..RoutingPolicy::default()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "k-route")
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 0);
}

#[tokio::test]
async fn offboarding_disables_routing_and_preserves_history() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    let carol = e.member("carol").await;
    let ext = e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    e.extension("1003", "ENTERPRISE_MEMBER", carol.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;
    let bob_phone = e.a.device(&bob, "bob-desk").await;
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "k-1")
        .await;
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // Bob declines: the Call ends DECLINED (canonical evidence).
    let invitation = e.a.invitation_for(call, bob_phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.decline(call, invitation, &bob_phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Declined);

    // Offboard Bob's extension: future routing stops ...
    let r = e
        .admin(
            Method::DELETE,
            &format!(
                "/v1/enterprise/extensions/{}?expectedVersion=1",
                id_of(&ext, "extensionId")
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "k-2")
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // ... the old Call and its enterprise evidence remain, with the number snapshot.
    let view = e.a.call_view(call).await;
    assert_eq!(view["state"], "DECLINED");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' \
             AND entry_extension_number = '1002'"
        ))
        .await,
        1
    );

    // A deactivated member behind an ACTIVE extension: the Call fails TARGET_UNAVAILABLE
    // (never routed elsewhere silently).
    e.h()
        .platform
        .set_principal_status(carol.id(), PrincipalStatus::Deactivated)
        .await
        .unwrap();
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1003", "k-3")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "FAILED", "{}", r.text);
    assert_eq!(r.json["call"]["failureCategory"], "TARGET_UNAVAILABLE");
    // A member without any endpoint: TARGET_UNAVAILABLE.
    let dave = e.member("dave").await;
    e.extension("1004", "ENTERPRISE_MEMBER", dave.id()).await;
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1004", "k-4")
        .await;
    assert_eq!(r.json["call"]["state"], "FAILED", "{}", r.text);
    assert_eq!(r.json["call"]["failureCategory"], "TARGET_UNAVAILABLE");
}

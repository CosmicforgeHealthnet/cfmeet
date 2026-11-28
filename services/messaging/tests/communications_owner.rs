//! Sections 9–23 private owner interfaces (MS-1 … MS-7) for the Communications Hub,
//! through the REAL router with real PostgreSQL: signed internal calls from the
//! Communications identity carrying the delegated personal scope and its NATIVE_CF_USER.
//! Customer fixtures are created before the scope is governed (the scripted policy client
//! has no Hub target, so governed customer sends would fail closed).
mod common;
use axum::http::{Method, StatusCode};
use cf_service_auth::DelegatedPrincipalKind;
use common::{realtime::cid, *};
use serde_json::{json, Value};
use uuid::Uuid;

/// Validates `value` against a component schema of the private Communications contract
/// (`packages/contracts/messaging/communications-internal/v1.openapi.json`).
fn conforms_internal(component: &str, value: &Value) {
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/contracts/messaging/communications-internal/v1.openapi.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let schema = &doc["components"]["schemas"][component];
    assert!(schema.is_object(), "no component {component}");
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{component} violated: {errors:?}\n{value:#}"
    );
}

/// The personal scope of `c` becomes the one Communications governs.
fn govern(h: &mut Harness, c: &Customer) {
    h.state.communications = Some(cf_communications_policy::PolicyClient::new(
        h.state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
}

async fn hub_as(
    h: &Harness,
    c: &Customer,
    user: Uuid,
    kind: DelegatedPrincipalKind,
    route: &str,
    body: Value,
) -> Response {
    let ctx = c
        .context
        .scope
        .delegated()
        .with_initiating_principal(user, kind);
    h.call_internal(
        &peers().communications,
        Method::POST,
        &format!("/internal/v1/communications/{route}"),
        Some(body),
        Some(ctx),
        &[],
    )
    .await
}

async fn hub(h: &Harness, c: &Customer, user: Uuid, route: &str, body: Value) -> Response {
    hub_as(
        h,
        c,
        user,
        DelegatedPrincipalKind::NativeCfUser,
        route,
        body,
    )
    .await
}

#[tokio::test]
async fn access_reports_members_and_tells_non_members_nothing() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let sent = h
        .send_e2ee(&c, &conv, &mut alice, "client-side test content")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let mid = sent.json["messageId"].clone();
    let outsider = h.principal(&c);
    govern(&mut h, &c);

    let r = hub(
        &h,
        &c,
        alice.principal,
        "conversations/access",
        json!({"principalId": alice.principal, "conversationId": cid(&conv), "messageId": mid}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["schemaVersion"], 1);
    assert_eq!(r.json["member"], true);
    assert_eq!(r.json["conversationId"], json!(cid(&conv)));
    assert_eq!(r.json["contextClass"], "DIRECT");
    assert_eq!(r.json["securityProfile"], "PRIVATE_E2EE");
    assert_eq!(r.json["memberCount"], 2);
    let mut expected = vec![alice.principal, bob.principal];
    expected.sort();
    assert_eq!(r.json["memberPrincipalIds"], json!(expected));
    assert!(r.json["lastSequence"].as_i64().unwrap() >= 1);
    assert_eq!(r.json["message"]["state"], "VISIBLE");
    assert_eq!(
        r.json["message"]["senderPrincipalId"],
        json!(alice.principal)
    );
    // Metadata only: never ciphertext.
    assert!(!r.text.contains("ciphertext"));

    let unknown = hub(
        &h,
        &c,
        alice.principal,
        "conversations/access",
        json!({"principalId": alice.principal, "conversationId": cid(&conv), "messageId": Uuid::new_v4()}),
    )
    .await;
    assert_eq!(
        unknown.json["message"]["state"], "NOT_FOUND",
        "{}",
        unknown.text
    );
    assert!(unknown.json["message"]["sequence"].is_null());

    for (who, conversation) in [(outsider, cid(&conv)), (alice.principal, Uuid::new_v4())] {
        let r = hub(
            &h,
            &c,
            who,
            "conversations/access",
            json!({"principalId": who, "conversationId": conversation, "messageId": mid}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json, json!({"schemaVersion": 1, "member": false}));
    }

    // MS-1b (contacts audit K9): many references in one call, in request order.
    let missing = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    let r = hub(
        &h,
        &c,
        alice.principal,
        "conversations/messages/access",
        json!({"principalId": alice.principal, "items": [
            {"conversationId": cid(&conv), "messageId": mid},
            {"conversationId": cid(&conv), "messageId": missing},
            {"conversationId": elsewhere, "messageId": mid},
        ]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["schemaVersion"], 1);
    conforms_internal("MessagesAccess", &r.json);
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["member"], true);
    assert_eq!(items[0]["state"], "VISIBLE");
    assert_eq!(items[0]["messageId"], mid);
    assert_eq!(items[1]["state"], "NOT_FOUND");
    assert_eq!(
        items[2],
        json!({"conversationId": elsewhere, "messageId": mid, "member": false})
    );
    assert!(!r.text.contains("ciphertext"));
    let r = hub(
        &h,
        &c,
        outsider,
        "conversations/messages/access",
        json!({"principalId": outsider, "items": [{"conversationId": cid(&conv), "messageId": mid}]}),
    )
    .await;
    assert_eq!(r.json["items"][0]["member"], false, "{}", r.text);
    let too_many: Vec<Value> = (0..51)
        .map(|_| json!({"conversationId": cid(&conv), "messageId": mid}))
        .collect();
    let r = hub(
        &h,
        &c,
        alice.principal,
        "conversations/messages/access",
        json!({"principalId": alice.principal, "items": too_many}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

#[tokio::test]
async fn a_principal_other_than_the_signed_user_is_forbidden() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let (conv, alice, bob) = h.e2ee_direct(&c).await;
    govern(&mut h, &c);
    let routes = [
        (
            "conversations/access",
            json!({"principalId": bob.principal, "conversationId": cid(&conv)}),
        ),
        (
            "conversations/attachments",
            json!({"principalId": bob.principal, "conversationId": cid(&conv), "limit": 10}),
        ),
        ("storage-summary", json!({"principalId": bob.principal})),
        (
            "devices/verify",
            json!({"principalId": bob.principal, "deviceId": bob.device_id()}),
        ),
        (
            "devices/revoke",
            json!({"principalId": bob.principal, "deviceId": bob.device_id(), "reason": "USER_REQUESTED"}),
        ),
        (
            "devices/push",
            json!({"principalId": bob.principal, "deviceId": bob.device_id(), "action": "REMOVE"}),
        ),
        ("push-status", json!({"principalId": bob.principal})),
    ];
    for (route, body) in &routes {
        // Alice is the signed user; the body names Bob.
        let r = hub(&h, &c, alice.principal, route, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{route}: {}", r.text);
        assert_eq!(r.code(), "PRINCIPAL_NOT_AUTHORIZED", "{route}");
        // The right id under a non-user initiating principal kind is refused as well.
        let r = hub_as(
            &h,
            &c,
            bob.principal,
            DelegatedPrincipalKind::ServiceAccount,
            route,
            body.clone(),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{route}: {}", r.text);
    }
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_devices WHERE id = '{}' AND status = 'ACTIVE'",
            bob.device_id()
        ))
        .await,
        1,
        "a refused revoke changed nothing"
    );
}

#[tokio::test]
async fn a_scope_communications_does_not_govern_is_not_found() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let other = h.first_party(true);
    let alice = h.member(&c, 0).await;
    let stranger = h.member(&other, 0).await;
    let body = |p: Uuid, d: Uuid| {
        vec![
            (
                "conversations/access",
                json!({"principalId": p, "conversationId": Uuid::new_v4()}),
            ),
            ("storage-summary", json!({"principalId": p})),
            ("devices/verify", json!({"principalId": p, "deviceId": d})),
            (
                "devices/revoke",
                json!({"principalId": p, "deviceId": d, "reason": "COMPROMISED"}),
            ),
            ("push-status", json!({"principalId": p})),
        ]
    };
    // No governed scope at all.
    for (route, b) in body(alice.principal, alice.device_id()) {
        let r = hub(&h, &c, alice.principal, route, b).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{route}: {}", r.text);
    }
    // Another environment is governed: this one still is not.
    govern(&mut h, &c);
    for (route, b) in body(stranger.principal, stranger.device_id()) {
        let r = hub(&h, &other, stranger.principal, route, b).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{route}: {}", r.text);
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_devices WHERE status = 'REVOKED'")
            .await,
        0
    );
}

#[tokio::test]
async fn e2ee_attachments_are_reported_encrypted_within_the_members_window() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let sent = h
        .send_e2ee(&c, &conv, &mut alice, "client-side test content")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let mid = uuid(&sent.json["messageId"]);
    let sequence = sent.json["sequence"].as_i64().unwrap();
    let outsider = h.principal(&c);
    // An uploaded, verified PRIVATE_E2EE attachment of that message (ciphertext object;
    // the server holds no presentation metadata).
    let s = c.context.scope;
    let attachment = Uuid::new_v4();
    h.sql(&format!(
        "INSERT INTO messaging_attachments (id, tenant_id, application_id, environment_id, conversation_id, \
           uploader_principal_id, uploader_device_id, security_profile, kind, state, object_key, declared_size, \
           verified_size, declared_sha256, media_type, crypto_format, scan_status, committed_bytes, message_id, \
           intent_expires_at, created_at, finalized_at, state_changed_at) \
         VALUES ('{attachment}', '{}', '{}', '{}', '{}', '{}', '{}', 'PRIVATE_E2EE', 'IMAGE', 'READY', \
           'att/{}/{}/{}', 4096, 4096, repeat('a', 64), 'application/octet-stream', \
           'CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1', 'NOT_APPLICABLE_ENCRYPTED', 4096, '{mid}', \
           now() + interval '1 hour', now(), now(), now())",
        s.tenant_id,
        s.application_id,
        s.environment_id,
        cid(&conv),
        alice.principal,
        alice.device_id(),
        s.environment_id.simple(),
        cid(&conv).simple(),
        attachment.simple(),
    ))
    .await
    .unwrap();
    govern(&mut h, &c);

    let r = hub(
        &h,
        &c,
        bob.principal,
        "conversations/attachments",
        json!({"principalId": bob.principal, "conversationId": cid(&conv), "limit": 10}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let data = r.json["data"].as_array().unwrap();
    assert_eq!(data.len(), 1, "{}", r.text);
    assert_eq!(data[0]["attachmentId"], json!(attachment));
    assert_eq!(data[0]["messageId"], json!(mid));
    assert_eq!(data[0]["messageSequence"], sequence);
    assert_eq!(data[0]["mediaClass"], "ENCRYPTED");
    assert_eq!(data[0]["sizeBytes"], 4096);
    assert_eq!(data[0]["uploaderPrincipalId"], json!(alice.principal));
    // M3: the file name, media type and thumbnail of an E2EE file live inside the
    // MLS-encrypted descriptor; the server reports none of them (nothing fabricated) and
    // offers only the normal, access-checked download grant of the ciphertext.
    assert_eq!(data[0]["securityProfile"], "PRIVATE_E2EE");
    assert_eq!(data[0]["metadataLocation"], "ENCRYPTED_DESCRIPTOR");
    for absent in ["fileName", "mediaType", "kind", "thumbnail"] {
        assert!(data[0][absent].is_null(), "{absent}: {}", r.text);
    }
    assert!(!r.text.contains("octet-stream"), "{}", r.text);
    assert_eq!(
        data[0]["download"],
        json!({"method": "POST", "path": format!("v1/conversations/{}/attachments/{attachment}/download-grants", cid(&conv))})
    );
    conforms_internal("ConversationAttachments", &r.json);
    assert!(r.json["nextCursor"].is_null());

    // The clear-for-me watermark excludes it.
    let r = hub(
        &h,
        &c,
        bob.principal,
        "conversations/attachments",
        json!({"principalId": bob.principal, "conversationId": cid(&conv), "limit": 10, "afterSequence": sequence}),
    )
    .await;
    assert_eq!(r.json["data"], json!([]), "{}", r.text);

    // A non-member is told nothing.
    let r = hub(
        &h,
        &c,
        outsider,
        "conversations/attachments",
        json!({"principalId": outsider, "conversationId": cid(&conv), "limit": 10}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // The uploader's storage reflects the committed attachment.
    let r = hub(
        &h,
        &c,
        alice.principal,
        "storage-summary",
        json!({"principalId": alice.principal}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["attachmentCount"], 1);
    assert_eq!(r.json["attachmentBytes"], 4096);
    assert_eq!(r.json["backupBytes"], 0);
    assert!(r.json["environmentQuotaBytes"].is_null() || r.json["environmentQuotaBytes"].is_i64());
    let r = hub(
        &h,
        &c,
        bob.principal,
        "storage-summary",
        json!({"principalId": bob.principal}),
    )
    .await;
    assert_eq!(r.json["attachmentCount"], 0, "{}", r.text);
}

#[tokio::test]
async fn revoking_a_foreign_device_is_not_found_and_an_own_device_revokes_once() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let alice = h.member(&c, 0).await;
    let bob = h.member(&c, 0).await;
    govern(&mut h, &c);

    let foreign = hub(
        &h,
        &c,
        alice.principal,
        "devices/revoke",
        json!({"principalId": alice.principal, "deviceId": bob.device_id(), "reason": "USER_REQUESTED"}),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.text);
    let verify = hub(
        &h,
        &c,
        alice.principal,
        "devices/verify",
        json!({"principalId": alice.principal, "deviceId": bob.device_id()}),
    )
    .await;
    assert_eq!(
        verify.json,
        json!({"schemaVersion": 1, "owned": false, "status": null, "pushRegistered": false, "pushPlatform": null})
    );

    let revoke = json!({"principalId": alice.principal, "deviceId": alice.device_id(), "reason": "COMPROMISED"});
    let first = hub(&h, &c, alice.principal, "devices/revoke", revoke.clone()).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(
        first.json,
        json!({"schemaVersion": 1, "revoked": true, "alreadyRevoked": false})
    );
    let again = hub(&h, &c, alice.principal, "devices/revoke", revoke).await;
    assert_eq!(
        again.json,
        json!({"schemaVersion": 1, "revoked": true, "alreadyRevoked": true})
    );
    let verify = hub(
        &h,
        &c,
        alice.principal,
        "devices/verify",
        json!({"principalId": alice.principal, "deviceId": alice.device_id()}),
    )
    .await;
    assert_eq!(verify.json["owned"], true, "{}", verify.text);
    assert_eq!(verify.json["status"], "REVOKED");
    // The canonical revocation ran: reason, key-transparency leaf and Bob untouched.
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_devices WHERE id = '{}' AND revocation_reason = 'COMPROMISED' \
               AND revocation_leaf_index IS NOT NULL",
            alice.device_id()
        ))
        .await,
        1
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_devices WHERE id = '{}' AND status = 'ACTIVE'",
            bob.device_id()
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn push_status_reports_not_configured_and_never_echoes_the_handle() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let alice = h.member(&c, 0).await;
    let bob = h.member(&c, 0).await;
    govern(&mut h, &c);
    let handle = "section-nine-push-handle";

    let missing = hub(
        &h,
        &c,
        alice.principal,
        "devices/push",
        json!({"principalId": alice.principal, "deviceId": alice.device_id(), "action": "REGISTER"}),
    )
    .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST, "{}", missing.text);

    let foreign = hub(
        &h,
        &c,
        alice.principal,
        "devices/push",
        json!({"principalId": alice.principal, "deviceId": bob.device_id(), "action": "REGISTER",
               "registration": {"platform": "LOCAL_TEST", "handle": handle}}),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.text);

    let r = hub(
        &h,
        &c,
        alice.principal,
        "devices/push",
        json!({"principalId": alice.principal, "deviceId": alice.device_id(), "action": "REGISTER",
               "registration": {"platform": "LOCAL_TEST", "handle": handle}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json,
        json!({"schemaVersion": 1, "deviceId": alice.device_id(), "pushRegistered": true, "platform": "LOCAL_TEST"})
    );
    assert!(!r.text.contains(handle));

    let status = hub(
        &h,
        &c,
        alice.principal,
        "push-status",
        json!({"principalId": alice.principal}),
    )
    .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    assert_eq!(
        status.json["pushServices"],
        json!({"APNS": "NOT_CONFIGURED", "FCM": "NOT_CONFIGURED", "WEB_PUSH": "NOT_CONFIGURED"})
    );
    assert_eq!(
        status.json["devices"],
        json!([{"deviceId": alice.device_id(), "pushRegistered": true, "platform": "LOCAL_TEST",
                "lastJobState": null, "lastFailureClass": null, "lastAttemptAt": null}])
    );
    assert_eq!(status.json["pendingJobs"], 0);
    assert_eq!(status.json["deadJobs"], 0);
    assert!(!status.text.contains(handle));

    let removed = hub(
        &h,
        &c,
        alice.principal,
        "devices/push",
        json!({"principalId": alice.principal, "deviceId": alice.device_id(), "action": "REMOVE"}),
    )
    .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    assert_eq!(removed.json["pushRegistered"], false);
    let verify = hub(
        &h,
        &c,
        alice.principal,
        "devices/verify",
        json!({"principalId": alice.principal, "deviceId": alice.device_id()}),
    )
    .await;
    assert_eq!(
        verify.json,
        json!({"schemaVersion": 1, "owned": true, "status": "ACTIVE", "pushRegistered": false, "pushPlatform": null})
    );
}

/// Section 14: the Hub capability answer reports MANAGED messaging on its own. MANAGED
/// follows the personal-context policy; its live delivery is the session device of ADR
/// 0049 A1 (needs `MESSAGING_SESSION_DEVICES_ENABLED` and client-token keys), otherwise
/// clients poll. PRIVATE_E2EE readiness (`securityReady`) is never implied by it.
#[tokio::test]
async fn capabilities_report_managed_messaging_apart_from_e2ee_readiness() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let alice = h.member(&c, 0).await;
    h.set_policy(
        &c,
        json!({"contexts":{"DIRECT":{"allowedProfiles":["PRIVATE_E2EE"],"defaultProfile":"PRIVATE_E2EE"},
                           "GROUP":{"allowedProfiles":["PRIVATE_E2EE"],"defaultProfile":"PRIVATE_E2EE"}}}),
    )
    .await;
    govern(&mut h, &c);
    let body = json!({"principalId": alice.principal});
    let e2ee_only = hub(&h, &c, alice.principal, "capabilities", body.clone()).await;
    assert_eq!(e2ee_only.status, StatusCode::OK, "{}", e2ee_only.text);
    assert_eq!(e2ee_only.json["securityReady"], false);
    assert_eq!(
        e2ee_only.json["managed"],
        json!({"available": false, "reason": "MANAGED_NOT_ALLOWED", "realtime": "POLLING"})
    );
    h.state.communications = None;
    h.set_policy(
        &c,
        json!({"contexts":{"DIRECT":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"MANAGED"},
                           "GROUP":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"MANAGED"}}}),
    )
    .await;
    govern(&mut h, &c);
    let managed = hub(&h, &c, alice.principal, "capabilities", body).await;
    assert_eq!(managed.status, StatusCode::OK, "{}", managed.text);
    assert_eq!(managed.json["managed"]["available"], true);
    assert_eq!(managed.json["managed"]["reason"], "AVAILABLE");
    // Without client-token signing keys live delivery is not claimed.
    if h.state.realtime.keys.is_none() || !h.state.realtime.settings.session_devices_enabled {
        assert_eq!(managed.json["managed"]["realtime"], "POLLING");
    } else {
        assert_eq!(managed.json["managed"]["realtime"], "SESSION_DEVICE");
    }
    // The PRIVATE_E2EE fields keep their previous meaning for older Hub images.
    assert_eq!(managed.json["securityReady"], false);
    assert_eq!(managed.json["available"], false);
}

/// M1: the capabilities answer carries the effective attachment availability the client
/// attach control follows — computed from the same policy, storage and kill switch the
/// upload intent enforces. Off by default (platform policy), on with the CF Meet
/// first-party baseline (25 MiB, five kinds), off again without private storage.
#[tokio::test]
async fn capabilities_report_attachment_availability_from_the_effective_policy() {
    use messaging::conversations::policy::{register_baseline, ApplicationBaseline};
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let alice = h.member(&c, 0).await;
    h.set_policy(
        &c,
        json!({"contexts":{"DIRECT":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"MANAGED"},
                           "GROUP":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"MANAGED"}}}),
    )
    .await;
    h.state.storage = std::sync::Arc::new(cf_object_storage::MemoryObjectStore::new());
    govern(&mut h, &c);
    let body = json!({"principalId": alice.principal});
    let off = hub(&h, &c, alice.principal, "capabilities", body.clone()).await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.text);
    conforms_internal("Capability", &off.json);
    assert_eq!(off.json["attachments"]["enabled"], false, "{}", off.text);
    assert_eq!(off.json["attachments"]["reason"], "DISABLED_BY_POLICY");
    assert_eq!(
        off.json["attachments"]["contexts"],
        json!({"DIRECT": false, "GROUP": false})
    );

    let s = c.context.scope;
    register_baseline(
        h.conn(),
        s.tenant_id,
        s.application_id,
        s.environment_id,
        ApplicationBaseline::CfMeetFirstParty,
        h.now(),
    )
    .await
    .unwrap();
    let on = hub(&h, &c, alice.principal, "capabilities", body.clone()).await;
    conforms_internal("Capability", &on.json);
    assert_eq!(
        on.json["attachments"],
        json!({"enabled": true, "reason": "AVAILABLE", "maxBytes": 26_214_400, "maxPerMessage": 10,
               "allowedKinds": ["DOCUMENT", "FILE", "IMAGE", "VIDEO", "VOICE_NOTE"],
               "contexts": {"DIRECT": true, "GROUP": true}}),
        "{}",
        on.text
    );

    h.state.storage = std::sync::Arc::new(cf_object_storage::UnconfiguredStore);
    let no_storage = hub(&h, &c, alice.principal, "capabilities", body).await;
    assert_eq!(no_storage.json["attachments"]["enabled"], false);
    assert_eq!(
        no_storage.json["attachments"]["reason"],
        "STORAGE_NOT_CONFIGURED"
    );
}

/// M2: groups in common carry the display metadata the server legitimately holds —
/// title and avatar reference of MANAGED groups, the security profile and the current
/// member count — and only for groups both principals are CURRENT members of. A member
/// who left (or was removed) and a non-member see nothing of the group, not even its id.
#[tokio::test]
async fn common_groups_carry_display_metadata_for_current_members_only() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    h.allow_managed(&c, "GROUP").await;
    let people: Vec<Uuid> = (0..3).map(|_| h.principal(&c)).collect();
    let (alice, bob, carol) = (people[0], people[1], people[2]);
    let outsider = h.principal(&c);
    let created = h
        .create_conversation(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED",
                    "title": "Weekend hikers", "avatarRef": "avatar:hikers-1" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let gid = created.json["conversationId"].as_str().unwrap().to_string();
    for p in [bob, carol] {
        let r = h
            .post(
                &c,
                Some(alice),
                &format!("/v1/conversations/{gid}/members"),
                json!({ "principalId": p }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    // A second, untitled group of alice and bob only.
    let untitled = h
        .create_conversation(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(untitled.status, StatusCode::CREATED, "{}", untitled.text);
    let uid = untitled.json["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{uid}/members"),
            json!({ "principalId": bob }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Carol leaves the titled group (removed by its owner).
    let removed = h
        .delete(
            &c,
            Some(alice),
            &format!("/v1/conversations/{gid}/members/{carol}"),
        )
        .await;
    assert!(removed.status.is_success(), "{}", removed.text);
    govern(&mut h, &c);
    let pair = |a: Uuid, b: Uuid| json!({ "actorPrincipalId": a, "targetPrincipalId": b });

    let r = hub(&h, &c, alice, "common-groups", pair(alice, bob)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms_internal("Groups", &r.json);
    let groups = r.json["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{}", r.text);
    let titled = groups
        .iter()
        .find(|g| g["conversationId"] == json!(gid))
        .unwrap();
    assert_eq!(
        titled,
        &json!({"conversationId": gid, "contextClass": "GROUP", "securityProfile": "MANAGED",
                "title": "Weekend hikers", "avatarRef": "avatar:hikers-1", "memberCount": 2})
    );
    let plain = groups
        .iter()
        .find(|g| g["conversationId"] == json!(uid))
        .unwrap();
    assert!(plain["title"].is_null() && plain["avatarRef"].is_null());
    assert_eq!(plain["memberCount"], 2);

    // The member who left, and a non-member, learn nothing about the group.
    for (actor, target) in [(carol, alice), (carol, bob), (outsider, alice)] {
        let r = hub(&h, &c, actor, "common-groups", pair(actor, target)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["groups"], json!([]), "{}", r.text);
        assert!(!r.text.contains("Weekend hikers"));
    }
}

/// M3: the shared-media listing of a MANAGED conversation carries the sanitized file
/// name, media type and kind the server holds, and safe retrieval references — the
/// normal, access-checked download / thumbnail grant operations (relative `v1/...`
/// paths), never a URL or storage key. A thumbnail is offered only once READY. A
/// non-member is told NOT_FOUND.
#[tokio::test]
async fn managed_media_listing_carries_names_types_and_grant_references() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let outsider = h.principal(&c);
    let conversation = uuid(&conv["conversationId"]);
    let mut ids = Vec::new();
    for (name, kind, media) in [
        ("Holiday.png", "IMAGE", "image/png"),
        ("Minutes.pdf", "DOCUMENT", "application/pdf"),
    ] {
        let sent = h.send_managed(&c, &conv, alice, name).await;
        assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
        let mid = uuid(&sent.json["messageId"]);
        let s = c.context.scope;
        let attachment = Uuid::new_v4();
        h.sql(&format!(
            "INSERT INTO messaging_attachments (id, tenant_id, application_id, environment_id, conversation_id, \
               uploader_principal_id, security_profile, kind, state, object_key, declared_size, verified_size, \
               declared_sha256, media_type, safe_file_name, scan_status, committed_bytes, message_id, \
               intent_expires_at, created_at, finalized_at, state_changed_at) \
             VALUES ('{attachment}', '{}', '{}', '{}', '{conversation}', '{alice}', 'MANAGED', '{kind}', 'READY', \
               'att/{}/{}/{}', 2048, 2048, repeat('b', 64), '{media}', '{name}', 'CLEAN', 2048, '{mid}', \
               now() + interval '1 hour', now(), now(), now())",
            s.tenant_id,
            s.application_id,
            s.environment_id,
            s.environment_id.simple(),
            conversation.simple(),
            attachment.simple(),
        ))
        .await
        .unwrap();
        ids.push(attachment);
    }
    // The image has a READY server-made thumbnail (charged to the environment's usage).
    h.sql(&format!(
        "INSERT INTO messaging_storage_usage (environment_id, tenant_id, application_id, committed_bytes, updated_at) \
         VALUES ('{}', '{}', '{}', 4096, now()) ON CONFLICT (environment_id) DO NOTHING",
        c.context.scope.environment_id, c.context.scope.tenant_id, c.context.scope.application_id,
    ))
    .await
    .unwrap();
    h.sql(&format!(
        "INSERT INTO messaging_attachment_derivatives (id, attachment_id, environment_id, security_profile, kind, \
           status, object_key, media_type, width, height, bytes, sha256, attempts, next_attempt_at, created_at, completed_at) \
         VALUES ('{}', '{}', '{}', 'MANAGED', 'THUMBNAIL', 'READY', 'drv/{}/thumbnail/{}/t.png', 'image/png', 32, 32, \
           100, repeat('c', 64), 1, now(), now(), now())",
        Uuid::new_v4(),
        ids[0],
        c.context.scope.environment_id,
        c.context.scope.environment_id,
        ids[0],
    ))
    .await
    .unwrap();
    govern(&mut h, &c);

    let r = hub(
        &h,
        &c,
        bob,
        "conversations/attachments",
        json!({"principalId": bob, "conversationId": conversation, "limit": 10}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms_internal("ConversationAttachments", &r.json);
    let data = r.json["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "{}", r.text);
    let by_id = |id: Uuid| {
        data.iter()
            .find(|a| a["attachmentId"] == json!(id))
            .unwrap()
    };
    let image = by_id(ids[0]);
    assert_eq!(image["fileName"], "Holiday.png");
    assert_eq!(image["mediaType"], "image/png");
    assert_eq!(image["kind"], "IMAGE");
    assert_eq!(image["mediaClass"], "IMAGE");
    assert_eq!(image["securityProfile"], "MANAGED");
    assert_eq!(image["metadataLocation"], "SERVER");
    let base = format!("v1/conversations/{conversation}/attachments/{}", ids[0]);
    assert_eq!(
        image["download"],
        json!({"method": "POST", "path": format!("{base}/download-grants")})
    );
    assert_eq!(
        image["thumbnail"],
        json!({"method": "POST", "path": format!("{base}/thumbnail-grants"), "mediaType": "image/png"})
    );
    let doc = by_id(ids[1]);
    assert_eq!(doc["fileName"], "Minutes.pdf");
    assert_eq!(doc["mediaType"], "application/pdf");
    assert_eq!(doc["mediaClass"], "DOCUMENT");
    assert!(
        doc["thumbnail"].is_null(),
        "no thumbnail until one is READY"
    );
    // References only: never a signed URL, storage key or host.
    for leak in ["http", "att/", "drv/", "X-Amz", "Signature"] {
        assert!(!r.text.contains(leak), "{leak}: {}", r.text);
    }

    let r = hub(
        &h,
        &c,
        outsider,
        "conversations/attachments",
        json!({"principalId": outsider, "conversationId": conversation, "limit": 10}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert!(!r.text.contains("Holiday"));
}

/// SEC-020: the Hub's relationship reads (`blocks/check`, `common-groups`) answer from
/// the SIGNED governed scope only — principals of another environment named in the body
/// have no block and no group in common there — and the relationship / event routes
/// signed for an environment Communications does not govern are NOT_FOUND and write
/// nothing.
#[tokio::test]
async fn relationship_reads_and_events_never_cross_environments() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    // The Communications inbox and revocation tables have their own migration history
    // (`CommunicationsMigrator`); without them the "nothing was written" check below
    // would hold trivially.
    use messaging_migration::MigratorTrait as _;
    messaging_migration::CommunicationsMigrator::up(h.conn(), None)
        .await
        .unwrap();
    let c = h.first_party(true);
    let other = h.first_party(true);
    // Each environment: a GROUP of two and a block between them.
    let mut pairs = Vec::new();
    for customer in [&c, &other] {
        let (_, people) = h.managed_group(customer, 2).await;
        let r = h
            .post(
                customer,
                Some(people[0]),
                "/v1/messaging/blocks",
                json!({ "principalId": people[1] }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        pairs.push((people[0], people[1]));
    }
    let ((ca, cb), (oa, ob)) = (pairs[0], pairs[1]);
    govern(&mut h, &c);
    let pair = |a: Uuid, b: Uuid| json!({ "actorPrincipalId": a, "targetPrincipalId": b });

    // The governed scope sees its own relationship ...
    let r = hub(&h, &c, ca, "blocks/check", pair(ca, cb)).await;
    assert_eq!(r.json["blocked"], true, "{}", r.text);
    let r = hub(&h, &c, ca, "common-groups", pair(ca, cb)).await;
    assert_eq!(
        r.json["groups"].as_array().map(Vec::len),
        Some(1),
        "{}",
        r.text
    );
    // ... and nothing of the other environment's principals.
    let r = hub(&h, &c, ca, "blocks/check", pair(oa, ob)).await;
    assert_eq!(
        (r.status, &r.json["blocked"]),
        (StatusCode::OK, &json!(false)),
        "{}",
        r.text
    );
    let r = hub(&h, &c, ca, "common-groups", pair(oa, ob)).await;
    assert_eq!(
        (r.status, &r.json["groups"]),
        (StatusCode::OK, &json!([])),
        "{}",
        r.text
    );

    // Signed for the environment Communications does not govern: NOT_FOUND, no write.
    let event = json!({ "schemaVersion": 1, "eventId": Uuid::new_v4(), "aggregateId": Uuid::new_v4(),
        "version": 1, "type": "communications.isolation", "data": { "principalId": ob, "credentialsNotBefore": 1 } });
    for (route, body) in [
        ("blocks/check", pair(oa, ob)),
        ("common-groups", pair(oa, ob)),
        ("events", event),
    ] {
        let r = hub(&h, &other, oa, route, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{route}: {}", r.text);
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_communication_inbox")
            .await
            + h.scalar("SELECT count(*) FROM messaging_communication_revocations")
                .await,
        0
    );
}

/// A scripted Communications policy endpoint: counts checks and answers `allow`.
struct PolicyStub {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    allow: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

async fn policy_stub(h: &mut Harness, c: &Customer) -> PolicyStub {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let allow = Arc::new(AtomicBool::new(true));
    let (n, a) = (calls.clone(), allow.clone());
    let app = axum::Router::new().route(
        cf_communications_policy::CHECK_PATH,
        axum::routing::post(move || {
            let (n, a) = (n.clone(), a.clone());
            async move {
                n.fetch_add(1, Ordering::SeqCst);
                let allowed = a.load(Ordering::SeqCst);
                axum::Json(json!({"schemaVersion": 1, "allowed": allowed,
                    "reason": if allowed {"ALLOWED"} else {"NOT_PERMITTED"}, "policyVersion": 1}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let mut targets = cf_service_auth::client::InternalTargets::new(ENV);
    targets
        .insert(
            cf_service_auth::ServiceId::parse("cfmeet.communications").unwrap(),
            &url,
        )
        .unwrap();
    let client = Arc::new(
        cf_service_auth::client::InternalServiceClient::new(
            h.state.service_auth.shared_signer(),
            targets,
            std::time::Duration::from_secs(2),
        )
        .unwrap(),
    );
    h.state.communications = Some(cf_communications_policy::PolicyClient::new(
        client,
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
    PolicyStub { calls, allow }
}

/// A one-to-one send is checked with Communications every time, never served from the B2
/// cache: a block refuses the very next message even before Communications' asynchronous
/// event reaches Messaging (privacy guarantee), and a refusal is never remembered.
#[tokio::test]
async fn one_to_one_send_decisions_are_never_cached_so_a_block_refuses_at_once() {
    use std::sync::atomic::Ordering;
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let (conv, mut alice, _bob) = h.e2ee_direct(&c).await;
    let stub = policy_stub(&mut h, &c).await;
    for text in ["one", "two"] {
        let sent = h.send_e2ee(&c, &conv, &mut alice, text).await;
        assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    }
    let checked = stub.calls.load(Ordering::SeqCst);
    assert!(checked >= 2, "every send is checked, got {checked}");

    // Bob blocks Alice in Communications; its durable event has NOT reached Messaging yet.
    stub.allow.store(false, Ordering::SeqCst);
    let refused = h.send_e2ee(&c, &conv, &mut alice, "after the block").await;
    assert_eq!(refused.code(), "DELIVERY_REFUSED", "{}", refused.text);
    let refused = h.send_e2ee(&c, &conv, &mut alice, "still blocked").await;
    assert_eq!(refused.code(), "DELIVERY_REFUSED", "{}", refused.text);
    // A refusal is not remembered: once allowed again the next send is checked and sent.
    stub.allow.store(true, Ordering::SeqCst);
    let before = stub.calls.load(Ordering::SeqCst);
    let sent = h.send_e2ee(&c, &conv, &mut alice, "unblocked").await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    assert!(stub.calls.load(Ordering::SeqCst) > before);
}

/// Coordinated mode makes warmed direct sends local. A projected block refuses the
/// next send without consulting Communications, and an unblock revalidates eligibility.
#[tokio::test]
async fn local_direct_send_checks_reuse_eligibility_but_never_a_blocked_pair() {
    use messaging_migration::MigratorTrait as _;
    use std::sync::atomic::Ordering;
    let Some(mut h) = Harness::new().await else {
        return;
    };
    messaging_migration::CommunicationsMigrator::up(h.conn(), None)
        .await
        .unwrap();
    let c = h.first_party(true);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let stub = policy_stub(&mut h, &c).await;
    let generation = Uuid::new_v4();
    h.state.local_send_policy = true;
    h.state.local_send_generation = Some(generation);
    for text in ["cold eligibility", "warm local send", "another warm send"] {
        let sent = h.send_e2ee(&c, &conv, &mut alice, text).await;
        assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    }
    assert_eq!(
        stub.calls.load(Ordering::SeqCst),
        1,
        "warm sends must not make owner RPCs"
    );
    let aggregate = Uuid::new_v4();
    let sender = alice.principal;
    let event = |version, blocked| {
        json!({"schemaVersion":1,"eventId":Uuid::new_v4(),"aggregateId":aggregate,
        "version":version,"type":"communications.block.changed","data":{"actorPrincipalId":bob.principal,
        "targetPrincipalId":sender,"blocked":blocked,"projectionGeneration":generation}})
    };
    let applied = hub(&h, &c, bob.principal, "events", event(1, true)).await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.text);
    conforms_internal("EventReceipt", &applied.json);
    assert_eq!(applied.json["sendPolicyGeneration"], json!(generation));
    let denied = h
        .send_e2ee(&c, &conv, &mut alice, "must not pass the block")
        .await;
    assert_eq!(denied.code(), "DELIVERY_REFUSED", "{}", denied.text);
    assert_eq!(
        stub.calls.load(Ordering::SeqCst),
        1,
        "local block refuses even if the remote stub says allowed"
    );
    let applied = hub(&h, &c, bob.principal, "events", event(2, false)).await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.text);
    // A delayed block from before this unblock cannot restore the refusal.
    let stale = hub(&h, &c, bob.principal, "events", event(1, true)).await;
    assert_eq!(stale.status, StatusCode::OK, "{}", stale.text);
    let sent = h
        .send_e2ee(&c, &conv, &mut alice, "revalidated after unblock")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    assert_eq!(stub.calls.load(Ordering::SeqCst), 2);

    // Native Calling application rules use the published event vocabulary and
    // refuse either member locally, without relying on a lazy Hub profile.
    let rule = Uuid::new_v4();
    let native = |version, blocked| {
        json!({"schemaVersion":1,"eventId":Uuid::new_v4(),
        "aggregateId":rule,"version":version,"type":"communications.domain.block.changed",
        "data":{"ruleId":rule,"projectionGeneration":generation,"actorPrincipalId":null,
            "targetPrincipalId":sender,"blocked":blocked}})
    };
    let blocked = native(1, true);
    conforms_internal("CommittedEvent", &blocked);
    let applied = hub(&h, &c, bob.principal, "events", blocked).await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.text);
    assert_eq!(applied.json["sendPolicyGeneration"], json!(generation));
    let denied = h
        .send_e2ee(&c, &conv, &mut alice, "native application block")
        .await;
    assert_eq!(denied.code(), "DELIVERY_REFUSED", "{}", denied.text);
    assert_eq!(stub.calls.load(Ordering::SeqCst), 2);
    let applied = hub(&h, &c, bob.principal, "events", native(2, false)).await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.text);
    let sent = h
        .send_e2ee(&c, &conv, &mut alice, "native rule released")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    assert_eq!(stub.calls.load(Ordering::SeqCst), 3);
}

/// Starred messages (ADR 0073 unified starred content; the per-account store is the Hub's
/// saved messages, MS-1b checks every item with Messaging on each read): a starred message
/// that was deleted for everyone, hidden for the member, expired, or sits in a group the
/// member was removed from never reports VISIBLE again — so the Starred screen never
/// returns it — and the check never carries content.
#[tokio::test]
async fn starred_access_never_reports_deleted_expired_hidden_or_left_messages() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let (group, people) = h.managed_group(&c, 3).await;
    let (owner, member) = (people[0], people[1]);
    let cid = group["conversationId"].as_str().unwrap().to_string();
    let mut ids = Vec::new();
    for text in ["keep", "delete", "hide", "expire"] {
        let r = h.send_managed(&c, &group, owner, text).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        ids.push(r.json["messageId"].as_str().unwrap().to_string());
    }
    let del = h
        .send(
            Method::DELETE,
            Some(&c.token),
            Some(owner),
            &format!("/v1/conversations/{cid}/messages/{}?scope=EVERYONE", ids[1]),
            None,
            &[],
        )
        .await;
    assert!(del.status.is_success(), "{}", del.text);
    let hide = h
        .send(
            Method::DELETE,
            Some(&c.token),
            Some(member),
            &format!("/v1/conversations/{cid}/messages/{}?scope=ME", ids[2]),
            None,
            &[],
        )
        .await;
    assert!(hide.status.is_success(), "{}", hide.text);
    // Disappearing-message expiry, as the retention worker leaves it.
    h.sql(&format!(
        "UPDATE messaging_messages SET lifecycle = 'EXPIRED', lifecycle_reason = 'RETENTION_EXPIRED' WHERE id = '{}'",
        ids[3]
    ))
    .await
    .unwrap();
    govern(&mut h, &c);

    let items: Vec<Value> = ids
        .iter()
        .map(|m| json!({"conversationId": cid, "messageId": m}))
        .collect();
    let check = |who: Uuid| json!({"principalId": who, "items": items.clone()});
    let r = hub(
        &h,
        &c,
        member,
        "conversations/messages/access",
        check(member),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms_internal("MessagesAccess", &r.json);
    let states: Vec<&str> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["VISIBLE", "DELETED", "HIDDEN_FOR_ME", "EXPIRED"],
        "{}",
        r.text
    );
    for text in ["keep", "delete", "hide", "expire"] {
        assert!(
            !r.text.contains(text),
            "the access check never carries content"
        );
    }

    // Removed from the group: nothing about any of its messages, starred or not.
    let removed = h
        .send(
            Method::DELETE,
            Some(&c.token),
            Some(owner),
            &format!("/v1/conversations/{cid}/members/{member}"),
            None,
            &[],
        )
        .await;
    assert!(removed.status.is_success(), "{}", removed.text);
    let r = hub(
        &h,
        &c,
        member,
        "conversations/messages/access",
        check(member),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for item in r.json["items"].as_array().unwrap() {
        assert_eq!(item["member"], false, "{}", r.text);
        assert!(item.get("state").is_none(), "{}", r.text);
    }
    // The owner still sees the kept message as VISIBLE.
    let r = hub(&h, &c, owner, "conversations/messages/access", check(owner)).await;
    assert_eq!(r.json["items"][0]["state"], "VISIBLE", "{}", r.text);
}

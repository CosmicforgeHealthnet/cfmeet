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

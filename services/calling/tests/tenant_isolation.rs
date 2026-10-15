//! B. Tenant isolation: ownership chains are consistent and immutable at the database
//! level, and principal resolution never crosses tenant / application / environment.
//! Calls (Part C) are confined the same way: another tenant, application or environment
//! can neither read nor mutate a Call, and call rows cannot be re-parented.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    platform::principals::{NewPrincipal, PrincipalReference, PrincipalResolutionError},
};
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn children_cannot_point_at_parents_of_another_tenant() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;

    // Domain API: the parent is the only input; ownership is derived from it.
    assert_eq!(a.application.tenant_id, a.tenant.id);
    let env = a.env(EnvironmentKind::Production);
    assert_eq!(
        (env.tenant_id, env.application_id),
        (a.tenant.id, a.application.id)
    );

    // Environment claiming tenant B for application A.
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_environments (tenant_id, application_id, kind) VALUES ('{}', '{}', 'SANDBOX')",
            b.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err(), "environment crossed tenants");

    // Application scopes claiming another tenant.
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_application_scopes (tenant_id, application_id, scope) VALUES ('{}', '{}', 'calls.read')",
            b.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err());

    // An application can never be re-parented.
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_applications SET tenant_id = '{}' WHERE id = '{}'",
            b.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err(), "application re-parented");
}

#[tokio::test]
async fn service_accounts_cannot_cross_application_or_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let a2 = h.application_in(&a.tenant, "second").await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;
    let env_a = a.env(EnvironmentKind::Production).id;

    for (tenant, application, environment, case) in [
        (
            a.tenant.id,
            a2.application.id,
            env_a,
            "other application, same tenant",
        ),
        (b.tenant.id, a.application.id, env_a, "other tenant"),
        (
            a.tenant.id,
            a.application.id,
            b.env(EnvironmentKind::Production).id,
            "foreign environment",
        ),
    ] {
        let err = common::exec(
            h.conn(),
            &format!(
                "INSERT INTO platform_service_accounts (tenant_id, application_id, environment_id, name) \
                 VALUES ('{tenant}', '{application}', '{environment}', 'intruder')"
            ),
        )
        .await;
        assert!(err.is_err(), "service account accepted: {case}");
    }

    // Orphans are impossible.
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_service_accounts (tenant_id, application_id, environment_id, name) \
             VALUES ('{}', '{}', gen_random_uuid(), 'orphan')",
            a.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err());

    // Credentials cannot be attached to a service account through a mismatched chain.
    let issued = h.issue(a.sa(EnvironmentKind::Production), &[]).await;
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_api_credentials SET service_account_id = '{}' WHERE id = '{}'",
            a.sa(EnvironmentKind::Sandbox),
            issued.credential.id
        ),
    )
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn principal_resolution_never_crosses_tenant_application_or_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let a2 = h.application_in(&a.tenant, "second").await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;

    let (principal, _) = h
        .platform
        .register_principal(
            a.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "user-1".into(),
            },
        )
        .await
        .unwrap();
    // Tenant B has its own, unrelated "user-1".
    let (b_principal, _) = h
        .platform
        .register_principal(
            b.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "user-1".into(),
            },
        )
        .await
        .unwrap();
    assert_ne!(principal.id, b_principal.id);

    let read = &["principals.read"];
    let own = h.issue(a.sa(EnvironmentKind::Production), read).await;
    let other_tenant = h.issue(b.sa(EnvironmentKind::Production), read).await;
    let other_app = h.issue(a2.sa(EnvironmentKind::Production), read).await;
    let other_env = h.issue(a.sa(EnvironmentKind::Sandbox), read).await;

    let by_id = PrincipalReference::Id(principal.id);
    let by_ref = PrincipalReference::External {
        kind: PrincipalKind::ExternalApplicationPrincipal,
        external_reference: "user-1".into(),
    };

    // Own scope resolves.
    let ctx = h.ctx(own.secret.expose_once()).await;
    assert_eq!(
        h.state.principals.resolve(&ctx, &by_id).await.unwrap().id(),
        principal.id
    );
    assert_eq!(
        h.state
            .principals
            .resolve(&ctx, &by_ref)
            .await
            .unwrap()
            .id(),
        principal.id
    );

    // Foreign scopes: by id → not found; by reference → the foreign scope's own principal
    // (or not found), never tenant A's.
    for (token, own_ref) in [
        (&other_tenant, Some(b_principal.id)),
        (&other_app, None),
        (&other_env, None),
    ] {
        let ctx = h.ctx(token.secret.expose_once()).await;
        assert!(matches!(
            h.state.principals.resolve(&ctx, &by_id).await,
            Err(PrincipalResolutionError::NotFound)
        ));
        match (h.state.principals.resolve(&ctx, &by_ref).await, own_ref) {
            (Ok(p), Some(expected)) => {
                assert_eq!(p.id(), expected);
                assert_eq!(p.tenant_id(), ctx.tenant_id());
            }
            (Err(PrincipalResolutionError::NotFound), None) => {}
            (other, _) => panic!("unexpected resolution: {other:?}"),
        }
    }

    // Over HTTP a foreign principal is indistinguishable from a nonexistent one.
    let foreign = h
        .request(
            Method::POST,
            "/test/resolve",
            Some(other_tenant.secret.expose_once()),
            Some(json!({ "id": principal.id })),
        )
        .await;
    let nonexistent = h
        .request(
            Method::POST,
            "/test/resolve",
            Some(other_tenant.secret.expose_once()),
            Some(json!({ "id": uuid::Uuid::new_v4() })),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(nonexistent.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.normalized_error(), nonexistent.normalized_error());
}

#[tokio::test]
async fn principal_ownership_is_immutable_and_scoped_unique() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;
    let (p, _) = h
        .platform
        .register_principal(
            a.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::NativeCfUser,
                external_reference: "subject-9".into(),
            },
        )
        .await
        .unwrap();
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_principals SET tenant_id = '{}', application_id = '{}', environment_id = '{}' WHERE id = '{}'",
            b.tenant.id,
            b.application.id,
            b.env(EnvironmentKind::Production).id,
            p.id
        ),
    )
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn calls_are_confined_to_their_tenant_application_and_environment() {
    use calling::calls::{
        kinds::{AuthorizationReason, EndpointCategory},
        CallAction, CallError, CallScope, CallState,
    };
    use common::calls::{sql, CallWorld};

    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let shared = || Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone());
    let others = [
        CallWorld::in_harness(shared(), "tenant-b", EnvironmentKind::Production).await,
        CallWorld::for_consumer(
            shared(),
            w.h.application_in(&w.consumer.tenant, "app-b").await,
            EnvironmentKind::Production,
        )
        .await,
        CallWorld::for_consumer(
            shared(),
            w.h.application_in(&w.consumer.tenant, "app-c").await,
            EnvironmentKind::Sandbox,
        )
        .await,
    ];
    let participant = w.calls().participants(w.scope(), call.id).await.unwrap()[0].id;
    for other in &others {
        let scope = CallScope::of_platform(&other.ctx);
        assert!(matches!(
            other.calls().call(scope, call.id).await,
            Err(CallError::NotFound)
        ));
        assert!(matches!(
            other.calls().call_detail(scope, call.id).await,
            Err(CallError::NotFound)
        ));
        assert!(other
            .calls()
            .events(scope, call.id)
            .await
            .unwrap()
            .is_empty());
        assert!(other
            .calls()
            .participants(scope, call.id)
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            other
                .calls()
                .transition(&other.cmd, call.id, call.version, CallAction::Hold)
                .await,
            Err(CallError::NotFound)
        ));
        assert!(matches!(
            other
                .calls()
                .create_session(&other.cmd, call.id, participant, EndpointCategory::Web)
                .await,
            Err(CallError::NotFound)
        ));
        assert!(matches!(
            other
                .calls()
                .add_participant(&other.cmd, call.id, &other.recipient, None)
                .await,
            Err(CallError::NotFound)
        ));
        assert!(matches!(
            other
                .calls()
                .record_authorization(
                    &other.cmd,
                    call.id,
                    calling::calls::authorizations::NewAuthorization {
                        reason: AuthorizationReason::Revoked,
                        policy_version: calling::calls::routes::PolicyVersion::parse("x").unwrap(),
                        policy_snapshot: Default::default(),
                        limits_snapshot: Default::default(),
                    }
                )
                .await,
            Err(CallError::NotFound)
        ));
        assert!(other
            .calls()
            .list_calls(scope, Default::default())
            .await
            .unwrap()
            .calls
            .is_empty());
    }
    let after = w.call(call.id).await;
    assert_eq!(
        (after.state, after.version),
        (CallState::Connected, call.version)
    );

    // Database: call rows cannot claim another scope or be re-parented.
    let b = &others[0];
    for statement in [
        format!(
            "UPDATE calls SET tenant_id = '{}' WHERE id = '{}'",
            b.consumer.tenant.id, call.id
        ),
        format!(
            "UPDATE call_participants SET call_id = gen_random_uuid() WHERE call_id = '{}'",
            call.id
        ),
        format!(
            "INSERT INTO call_events SELECT gen_random_uuid(), call_id, '{}', application_id, environment_id, \
             sequence + 100, event_type, schema_version, from_state, to_state, call_version, actor_kind, \
             actor_service_account_id, actor_credential_id, actor_service, request_id, correlation_id, payload, \
             occurred_at, created_at FROM call_events WHERE call_id = '{}' AND sequence = 1",
            b.consumer.tenant.id, call.id
        ),
        // A call whose credential belongs to another tenant's service account.
        format!(
            "INSERT INTO calls SELECT gen_random_uuid(), tenant_id, application_id, environment_id, \
             '{}', created_by_credential_id, initiator_principal_id, call_kind, 'REQUESTED', 1, NULL, NULL, NULL, \
             NULL, now(), NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, now(), now() FROM calls WHERE id = '{}'",
            b.ctx.service_account_id(),
            call.id
        ),
    ] {
        assert!(sql(&w.h, &statement).await.is_err(), "{statement}");
    }
}

// ---------------------------------------------------------------------------
// Part H §72.5: cross-application / cross-environment theft of resource ids.
// ---------------------------------------------------------------------------

mod cross_scope {
    use std::sync::Arc;

    use axum::http::{Method, StatusCode};
    use calling::{
        calls::app::enterprise::CapturedMedia,
        domain::EnvironmentKind,
        enterprise::policy::AdminCapability,
        platform::{credentials::IssuedCredential, scopes::names},
        webhooks::WebhookSettings,
    };
    use cf_object_storage::MemoryObjectStore;
    use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::common::{
        self,
        enterprise::{
            allow_enterprise_scopes, credential, id_of, strings, EnterpriseWorld, RecordingCapture,
            ENTERPRISE_SCOPES,
        },
        Consumer, TestResponse, STANDARD_TEST_SCOPES,
    };

    const EXTRA_SCOPES: &[&str] = &[
        names::WEBHOOKS_READ,
        names::WEBHOOKS_MANAGE,
        names::USAGE_READ,
    ];

    fn all_scopes() -> Vec<&'static str> {
        STANDARD_TEST_SCOPES
            .iter()
            .chain(ENTERPRISE_SCOPES)
            .chain(EXTRA_SCOPES)
            .copied()
            .collect()
    }

    async fn allow_all(e: &EnterpriseWorld, consumer: &Consumer) {
        allow_enterprise_scopes(e.h(), consumer).await;
        for scope in EXTRA_SCOPES {
            e.h()
                .platform
                .grant_application_scope(consumer.application.id, scope)
                .await
                .unwrap();
        }
    }

    fn key_bytes() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(11).wrapping_add(3);
        }
        k
    }

    /// A body comparable across requests: request / correlation ids removed.
    fn normalized(r: &TestResponse) -> Value {
        fn strip(v: &mut Value) {
            match v {
                Value::Object(map) => {
                    map.remove("requestId");
                    map.remove("correlationId");
                    map.values_mut().for_each(strip);
                }
                Value::Array(items) => items.iter_mut().for_each(strip),
                _ => {}
            }
        }
        let mut v = r.json.clone();
        strip(&mut v);
        v
    }

    /// Every row of the tables holding scope S's resources (mutation detector).
    async fn snapshot(e: &EnterpriseWorld) -> String {
        let mut out = String::new();
        for table in [
            "enterprise_departments",
            "enterprise_extensions",
            "enterprise_ring_groups",
            "enterprise_call_queues",
            "enterprise_queue_members",
            "enterprise_ring_group_members",
            "enterprise_ivr_flow_versions",
            "enterprise_department_members",
            "webhook_deliveries",
            "enterprise_ivr_flows",
            "voicemail_mailboxes",
            "voicemail_messages",
            "call_recordings",
            "webhook_subscriptions",
            "calls",
            "calling_block_rules",
            "calling_abuse_reports",
            "enterprise_admin_audit",
            "call_recording_access_audit",
        ] {
            out.push_str(
                &strings(
                    e.h().conn(),
                    &format!("SELECT t::text FROM {table} t ORDER BY 1"),
                )
                .await
                .join("\n"),
            );
        }
        out
    }

    async fn audit_rows(e: &EnterpriseWorld) -> i64 {
        e.count(
            "SELECT (SELECT count(*) FROM enterprise_admin_audit) + \
                    (SELECT count(*) FROM call_recording_access_audit)",
        )
        .await
    }

    /// Scope S: one of every resource kind, created through the public API.
    struct World {
        e: EnterpriseWorld,
        token: IssuedCredential,
        ids: Vec<(&'static str, Uuid)>,
        caller: Uuid,
        call: Uuid,
        recording: Uuid,
        message: Uuid,
        mailbox: Uuid,
    }

    impl World {
        async fn send(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
            self.e
                .send(self.token.secret.expose_once(), method, path, body)
                .await
        }

        async fn create(&self, path: &str, body: Value) -> Value {
            let r = self.send(Method::POST, path, Some(body)).await;
            assert!(r.status == StatusCode::CREATED, "{path}: {}", r.text);
            r.json
        }

        async fn new() -> Option<Self> {
            let store = Arc::new(MemoryObjectStore::new());
            let capture = Arc::new(RecordingCapture::default());
            let (s, c) = (store.clone(), capture.clone());
            let e = EnterpriseWorld::configured(move |rt| {
                rt.artifacts = s;
                rt.capture = c;
                rt.webhooks = WebhookSettings {
                    secret_box: Some(Arc::new(SecretBox::from_bytes(&key_bytes()).unwrap())),
                    policy: EndpointPolicy {
                        allow_insecure_http: true,
                        allow_private_networks: true,
                    },
                    ..WebhookSettings::default()
                };
            })
            .await?;
            allow_all(&e, &e.a.w.consumer).await;
            e.h()
                .state
                .enterprise
                .operator_set_capabilities(
                    e.a.w.scope(),
                    vec![
                        AdminCapability::ManageRouting,
                        AdminCapability::ManageVoicemail,
                        AdminCapability::ManageRecording,
                    ],
                )
                .await
                .unwrap();
            let token = credential(e.h(), &e.a.w.consumer, e.a.w.kind, &all_scopes()).await;
            let mut w = Self {
                e,
                token,
                ids: Vec::new(),
                caller: Uuid::nil(),
                call: Uuid::nil(),
                recording: Uuid::nil(),
                message: Uuid::nil(),
                mailbox: Uuid::nil(),
            };
            let e = &w.e;
            let caller = e
                .member(&format!("caller-{}", Uuid::new_v4().simple()))
                .await;
            let callee = e
                .member(&format!("callee-{}", Uuid::new_v4().simple()))
                .await;
            let vm_owner = e.member(&format!("vm-{}", Uuid::new_v4().simple())).await;
            let caller_phone = e.a.device(&caller, "caller-desk").await;
            let callee_phone = e.a.device(&callee, "callee-desk").await;
            e.a.device(&vm_owner, "vm-desk").await;

            let dept = id_of(&e.department("Isolation").await, "departmentId");
            let ext = id_of(
                &e.extension("4100", "ENTERPRISE_MEMBER", callee.id()).await,
                "extensionId",
            );
            let ring = id_of(
                &w.create(
                    "/v1/enterprise/ring-groups",
                    json!({ "name": "Isolation group", "strategy": "SIMULTANEOUS",
                             "memberRingSeconds": 20, "groupTimeoutSeconds": 30 }),
                )
                .await,
                "ringGroupId",
            );
            let queue = id_of(
                &w.create(
                    "/v1/enterprise/queues",
                    json!({ "name": "Isolation queue", "maxWaitSeconds": 60,
                             "offerTimeoutSeconds": 10, "capacity": 5 }),
                )
                .await,
                "queueId",
            );
            let ivr = id_of(
                &w.create(
                    "/v1/enterprise/ivr-flows",
                    json!({ "name": "Isolation menu" }),
                )
                .await,
                "ivrFlowId",
            );
            let mailbox = id_of(
                &w.create(
                    "/v1/enterprise/voicemail/mailboxes",
                    json!({ "owner": { "type": "PRINCIPAL", "id": vm_owner.id().to_string() },
                             "displayLabel": "Isolation box", "maxMessageSeconds": 120,
                             "retentionDays": 30 }),
                )
                .await,
                "mailboxId",
            );
            w.create(
                "/v1/enterprise/forwarding-rules",
                json!({ "principalId": vm_owner.id(), "condition": "NO_ANSWER",
                         "destination": { "type": "VOICEMAIL", "id": mailbox.to_string() } }),
            )
            .await;
            e.extension("4101", "ENTERPRISE_MEMBER", vm_owner.id())
                .await;
            let webhook = w
                .create(
                    "/v1/webhooks",
                    json!({ "url": "http://127.0.0.1:9/hook", "eventTypes": ["call.requested"] }),
                )
                .await["webhook"]["webhookId"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap();
            w.create(
                "/v1/enterprise/recording-policies",
                json!({ "name": "Isolation recording", "mode": "MANUAL_ALLOWED",
                         "noticeRequired": false, "consentParticipants": "NONE",
                         "jurisdictionRef": "NG-NDPA-2023/v2", "retentionDays": 30,
                         "maxRecordingSeconds": 600, "isDefault": true }),
            )
            .await;

            // A connected, recorded, ended Call (history, recording, usage).
            let r = e
                .call_extension(
                    "VOICE",
                    &caller,
                    &caller_phone,
                    "4100",
                    &Uuid::new_v4().to_string(),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
            let inv =
                e.a.invitation_for(call, callee_phone.endpoint)
                    .await
                    .unwrap();
            let (_, v) = e.a.state(call).await;
            assert_eq!(
                e.a.accept(call, inv, &callee_phone, v).await.status,
                StatusCode::OK
            );
            for d in [&caller_phone, &callee_phone] {
                let s = e.a.session_on(call, d.endpoint).await.unwrap();
                e.a.connect(call, s).await;
            }
            let r = w
                .send(
                    Method::POST,
                    &format!("/v1/calls/{call}/recordings"),
                    Some(json!({})),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            let recording = id_of(&r.json["recording"], "recordingId");
            let (_, v) = e.a.state(call).await;
            assert_eq!(e.a.command(call, "end", v).await.status, StatusCode::OK);
            e.h().state.app.expire_due(100).await.unwrap();
            let audio = b"RIFF$\0\0\0WAVEfmt isolation-recording".to_vec();
            e.h()
                .state
                .app
                .deliver_capture(
                    recording,
                    call,
                    CapturedMedia {
                        content_type: "audio/wav".into(),
                        duration_ms: 1000,
                        sha256: cf_object_storage::sha256_hex(&audio),
                        bytes: audio,
                    },
                )
                .await
                .unwrap();

            // A voicemail message (no answer -> the owner's mailbox).
            let r = e
                .call_extension(
                    "VOICE",
                    &caller,
                    &caller_phone,
                    "4101",
                    &Uuid::new_v4().to_string(),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            let vm_call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
            e.h().clock.advance(chrono::Duration::seconds(46));
            e.h().state.app.expire_due(100).await.unwrap();
            let message: Uuid = strings(
                e.h().conn(),
                &format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{vm_call}'"),
            )
            .await
            .pop()
            .expect("a voicemail message")
            .parse()
            .unwrap();
            let audio = b"RIFF$\0\0\0WAVEfmt isolation-voicemail".to_vec();
            e.h()
                .state
                .app
                .deliver_capture(
                    message,
                    vm_call,
                    CapturedMedia {
                        content_type: "audio/wav".into(),
                        duration_ms: 1000,
                        sha256: cf_object_storage::sha256_hex(&audio),
                        bytes: audio,
                    },
                )
                .await
                .unwrap();
            assert_eq!(store.len(), 2);

            // A block rule and an abuse report of the caller.
            let stranger = e
                .member(&format!("stranger-{}", Uuid::new_v4().simple()))
                .await;
            let block = id_of(
                &w.create(
                    "/v1/blocks",
                    json!({ "principalId": caller.id(), "blockedPrincipalId": stranger.id(),
                             "reasonCode": "UNWANTED" }),
                )
                .await,
                "blockId",
            );
            let r = w
                .send(
                    Method::POST,
                    &format!("/v1/calls/{call}/abuse-reports"),
                    Some(json!({ "principalId": caller.id(), "category": "SPAM" })),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);

            w.ids = vec![
                ("department", dept),
                ("extension", ext),
                ("ring group", ring),
                ("queue", queue),
                ("ivr flow", ivr),
                ("mailbox", mailbox),
                ("message", message),
                ("recording", recording),
                ("webhook", webhook),
                ("call", call),
                ("block", block),
            ];
            w.caller = caller.id().as_uuid();
            w.call = call;
            w.recording = recording;
            w.message = message;
            w.mailbox = mailbox;
            Some(w)
        }

        /// Every id-addressed request of every resource kind ({id} is substituted).
        fn requests(&self) -> Vec<(Method, String, Option<Value>, Uuid)> {
            let id = |name: &str| self.ids.iter().find(|(n, _)| *n == name).unwrap().1;
            let v1 = json!({ "expectedVersion": 1 });
            let mut out = Vec::new();
            let mut add = |m: Method, path: &str, body: Option<Value>, target: Uuid| {
                out.push((m, path.to_string(), body, target));
            };
            for (kind, base, patch) in [
                (
                    "department",
                    "/v1/enterprise/departments",
                    json!({ "expectedVersion": 1, "name": "hijack" }),
                ),
                (
                    "extension",
                    "/v1/enterprise/extensions",
                    json!({ "expectedVersion": 1, "displayLabel": "hijack" }),
                ),
                (
                    "ring group",
                    "/v1/enterprise/ring-groups",
                    json!({ "expectedVersion": 1, "name": "hijack" }),
                ),
                (
                    "queue",
                    "/v1/enterprise/queues",
                    json!({ "expectedVersion": 1, "name": "hijack" }),
                ),
                (
                    "ivr flow",
                    "/v1/enterprise/ivr-flows",
                    json!({ "expectedVersion": 1, "name": "hijack" }),
                ),
                (
                    "mailbox",
                    "/v1/enterprise/voicemail/mailboxes",
                    json!({ "expectedVersion": 1, "displayLabel": "hijack" }),
                ),
                (
                    "webhook",
                    "/v1/webhooks",
                    json!({ "expectedVersion": 1, "description": "hijack" }),
                ),
            ] {
                let target = id(kind);
                add(Method::GET, &format!("{base}/{{id}}"), None, target);
                add(
                    Method::PATCH,
                    &format!("{base}/{{id}}"),
                    Some(patch),
                    target,
                );
                add(
                    Method::DELETE,
                    &format!("{base}/{{id}}?expectedVersion=1"),
                    None,
                    target,
                );
            }
            add(
                Method::PUT,
                "/v1/enterprise/departments/{id}/members",
                Some(json!({ "expectedVersion": 1, "members": [] })),
                id("department"),
            );
            add(
                Method::PUT,
                "/v1/enterprise/ring-groups/{id}/members",
                Some(json!({ "expectedVersion": 1, "members": [] })),
                id("ring group"),
            );
            add(
                Method::GET,
                "/v1/enterprise/queues/{id}/entries",
                None,
                id("queue"),
            );
            add(
                Method::PUT,
                "/v1/enterprise/queues/{id}/members",
                Some(json!({ "expectedVersion": 1, "members": [] })),
                id("queue"),
            );
            add(
                Method::GET,
                "/v1/enterprise/ivr-flows/{id}/versions",
                None,
                id("ivr flow"),
            );
            add(
                Method::PUT,
                "/v1/enterprise/ivr-flows/{id}/activation",
                Some(json!({ "expectedVersion": 1, "versionNumber": 1 })),
                id("ivr flow"),
            );
            add(
                Method::GET,
                "/v1/enterprise/voicemail/messages?mailboxId={id}",
                None,
                self.mailbox,
            );
            let message = self.message;
            add(
                Method::GET,
                "/v1/enterprise/voicemail/messages/{id}",
                None,
                message,
            );
            add(
                Method::GET,
                &format!(
                    "/v1/enterprise/voicemail/messages/{{id}}?principalId={}",
                    self.caller
                ),
                None,
                message,
            );
            add(
                Method::POST,
                "/v1/enterprise/voicemail/messages/{id}/media-access",
                Some(json!({})),
                message,
            );
            add(
                Method::PUT,
                "/v1/enterprise/voicemail/messages/{id}/read-state",
                Some(json!({ "expectedVersion": 1, "readState": "READ" })),
                message,
            );
            add(
                Method::DELETE,
                "/v1/enterprise/voicemail/messages/{id}?expectedVersion=1",
                None,
                message,
            );
            let recording = self.recording;
            add(
                Method::GET,
                "/v1/enterprise/recordings/{id}",
                None,
                recording,
            );
            add(
                Method::GET,
                &format!(
                    "/v1/enterprise/recordings/{{id}}?principalId={}",
                    self.caller
                ),
                None,
                recording,
            );
            add(
                Method::POST,
                "/v1/enterprise/recordings/{id}/media-access",
                Some(json!({})),
                recording,
            );
            add(
                Method::POST,
                "/v1/enterprise/recordings/{id}/media-access",
                Some(json!({ "principalId": self.caller })),
                recording,
            );
            add(
                Method::GET,
                "/v1/enterprise/recordings/{id}/access-audit",
                None,
                recording,
            );
            add(
                Method::DELETE,
                "/v1/enterprise/recordings/{id}?expectedVersion=1",
                None,
                recording,
            );
            add(
                Method::POST,
                &format!("/v1/calls/{}/recordings/{{id}}/stop", self.call),
                Some(json!({})),
                recording,
            );
            let call = self.call;
            add(Method::GET, "/v1/calls/{id}", None, call);
            add(Method::GET, "/v1/calls/{id}/actions", None, call);
            add(Method::GET, "/v1/calls/{id}/recordings", None, call);
            add(
                Method::GET,
                "/v1/calls/{id}/recording-authorizations",
                None,
                call,
            );
            add(Method::POST, "/v1/calls/{id}/end", Some(v1.clone()), call);
            add(
                Method::POST,
                "/v1/calls/{id}/abuse-reports",
                Some(json!({ "principalId": self.caller, "category": "SPAM" })),
                call,
            );
            add(
                Method::GET,
                "/v1/webhooks/{id}/deliveries",
                None,
                id("webhook"),
            );
            add(
                Method::POST,
                "/v1/webhooks/{id}/rotate-secret",
                Some(v1),
                id("webhook"),
            );
            add(Method::DELETE, "/v1/blocks/{id}", None, id("block"));
            // Principal-addressed reads: the caller's history, blocks and reports.
            let caller = self.caller;
            add(Method::GET, "/v1/calls?principalId={id}", None, caller);
            add(Method::GET, "/v1/blocks?principalId={id}", None, caller);
            add(
                Method::GET,
                "/v1/abuse-reports?principalId={id}",
                None,
                caller,
            );
            out
        }
    }

    async fn foreign_ids_behave_like_unknown_ones(
        w: &World,
        foreign: &IssuedCredential,
        case: &str,
    ) {
        let token = foreign.secret.expose_once();
        let before_rows = snapshot(&w.e).await;
        let before_audit = audit_rows(&w.e).await;
        for (method, template, body, target) in w.requests() {
            let path = template.replace("{id}", &target.to_string());
            let unknown = template.replace("{id}", &Uuid::new_v4().to_string());
            let unknown_id = unknown
                .split(['/', '?', '='])
                .find(|part| {
                    part.len() == 36 && Uuid::parse_str(part).is_ok() && !path.contains(*part)
                })
                .unwrap()
                .to_string();
            let stolen = w.e.send(token, method.clone(), &path, body.clone()).await;
            let missing = w.e.send(token, method.clone(), &unknown, body).await;
            // An id the request itself carried may be echoed back (e.g. a filter):
            // compare with the addressed id blanked in both answers.
            let blank = |r: &TestResponse, id: &str| -> String {
                normalized(r).to_string().replace(id, "{id}")
            };
            assert_eq!(
                (stolen.status, blank(&stolen, &target.to_string())),
                (missing.status, blank(&missing, &unknown_id)),
                "{case}: {method} {template} differs from an unknown id"
            );
            assert!(
                !stolen.status.is_success()
                    || stolen.json.get("items").is_some()
                    || stolen.json.get("data").is_some(),
                "{case}: {method} {path} succeeded: {}",
                stolen.text
            );
            for text in [&stolen.text, &missing.text] {
                assert!(
                    !text.contains("memory://") && !text.contains("\"url\""),
                    "{case}: {text}"
                );
                assert!(
                    !text.contains(&target.to_string()) || template.contains("?principalId"),
                    "{case}: {method} {path} echoed the id: {text}"
                );
            }
        }
        // Usage: the other scope's listing shows nothing of scope S.
        let usage = w.e.send(token, Method::GET, "/v1/usage", None).await;
        assert_eq!(usage.status, StatusCode::OK, "{case}: {}", usage.text);
        assert!(
            !usage.text.contains(&w.call.to_string()),
            "{case}: {}",
            usage.text
        );
        assert!(!usage.text.contains(&w.recording.to_string()), "{case}");
        assert_eq!(
            audit_rows(&w.e).await,
            before_audit,
            "{case}: an audit row was written"
        );
        assert_eq!(snapshot(&w.e).await, before_rows, "{case}: scope S changed");
    }

    /// Every resource kind of scope S, addressed by id from another application of the
    /// same tenant and from another environment of the same application, answers exactly
    /// like a random unknown id: same status, same body (request ids aside), no audit
    /// row, no signed URL, no mutation. Its own scope still sees everything.
    #[tokio::test]
    async fn ids_from_another_application_or_environment_behave_like_unknown_ones() {
        let Some(w) = World::new().await else {
            return;
        };
        // Scope S itself sees its resources (the test would be vacuous otherwise).
        for (method, template, _, target) in w.requests() {
            if method != Method::GET
                || template.contains("?principalId") && template.starts_with("/v1/enterprise")
            {
                continue;
            }
            let r = w
                .send(method, &template.replace("{id}", &target.to_string()), None)
                .await;
            assert!(r.status.is_success(), "own scope: {template}: {}", r.text);
        }
        let own_usage = w.send(Method::GET, "/v1/usage", None).await;
        assert_eq!(own_usage.status, StatusCode::OK, "{}", own_usage.text);
        assert!(
            !own_usage.json["data"].as_array().unwrap().is_empty(),
            "{}",
            own_usage.text
        );

        // (a) Another application of the same tenant.
        let app_b =
            w.e.h()
                .application_in(&w.e.a.w.consumer.tenant, "app-b")
                .await;
        allow_all(&w.e, &app_b).await;
        let other_app = credential(w.e.h(), &app_b, w.e.a.w.kind, &all_scopes()).await;
        foreign_ids_behave_like_unknown_ones(&w, &other_app, "other application").await;

        // (b) Another environment of the same application.
        let other_env = credential(
            w.e.h(),
            &w.e.a.w.consumer,
            EnvironmentKind::Sandbox,
            &all_scopes(),
        )
        .await;
        foreign_ids_behave_like_unknown_ones(&w, &other_env, "other environment").await;

        // The artifacts are untouched and no URL was minted for them.
        assert_eq!(
            common::scalar_i64(
                w.e.h().conn(),
                "SELECT count(*) FROM call_recordings WHERE state = 'READY'"
            )
            .await,
            1
        );
    }
}

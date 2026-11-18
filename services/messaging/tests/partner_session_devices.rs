//! CF Meet Platform "Build" (ADR 0062 / 0063; `docs/platform/build-internal-contracts.md`
//! §4–§6): partner session devices exchanged from CF Meet user tokens, their MANAGED-only
//! REST allowlist and realtime, revocation, environment origins, the partner policy
//! baseline, MESSAGE_SENT facts and partner push — against the real router, a real
//! PostgreSQL database and real WebSockets, with Platform Access and the Notification
//! Service scripted in-process.

mod common;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::http::{Method, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use common::{flows::*, realtime::*, *};
use messaging::{
    identity::{AuthorityUnavailable, OwnerScope, PrincipalStatus},
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
    partner::{
        push::{PartnerRegistrationSink, RegistrationResult},
        ChangePage, Grants, Partner, PartnerDirectory, PartnerSettings, Verification, VerifiedUser,
        VerifiedUserToken,
    },
    push_mirror::SyncFailure,
};
use serde_json::{json, Value};
use uuid::Uuid;

// ------------------------------------------------------------------ scripted peers

#[derive(Default)]
struct FakeDirectory {
    tokens: Mutex<HashMap<String, Result<VerifiedUserToken, &'static str>>>,
    revoked_credentials: Mutex<HashSet<Uuid>>,
    origins: Mutex<HashMap<Uuid, Vec<String>>>,
    changes: Mutex<Vec<Value>>,
    verifications: Mutex<usize>,
}

#[async_trait::async_trait]
impl PartnerDirectory for FakeDirectory {
    async fn verify_user_token(&self, token: &str) -> Result<Verification, AuthorityUnavailable> {
        *self.verifications.lock().unwrap() += 1;
        Ok(match self.tokens.lock().unwrap().get(token) {
            Some(Ok(v)) => Verification::Valid(Box::new(v.clone())),
            Some(Err(code)) => Verification::Rejected(code),
            None => Verification::Rejected("USER_TOKEN_INVALID"),
        })
    }

    async fn credential_active(
        &self,
        _scope: &OwnerScope,
        credential_id: Uuid,
    ) -> Result<bool, AuthorityUnavailable> {
        Ok(!self
            .revoked_credentials
            .lock()
            .unwrap()
            .contains(&credential_id))
    }

    async fn environment_origins(
        &self,
        environment_id: Uuid,
    ) -> Result<Vec<String>, AuthorityUnavailable> {
        Ok(self
            .origins
            .lock()
            .unwrap()
            .get(&environment_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn identity_changes(&self, after: i64) -> Result<ChangePage, AuthorityUnavailable> {
        let changes: Vec<Value> = self
            .changes
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["sequence"].as_i64().unwrap_or(0) > after)
            .cloned()
            .collect();
        let next_after = changes
            .iter()
            .filter_map(|c| c["sequence"].as_i64())
            .max()
            .unwrap_or(after);
        Ok(ChangePage {
            changes,
            next_after,
            has_more: false,
        })
    }
}

#[derive(Default)]
struct FakeRegistrations {
    items: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl PartnerRegistrationSink for FakeRegistrations {
    async fn sync(&self, registrations: &[Value]) -> Result<Vec<RegistrationResult>, SyncFailure> {
        self.items
            .lock()
            .unwrap()
            .extend(registrations.iter().cloned());
        Ok(registrations
            .iter()
            .map(|r| RegistrationResult {
                outcome: if r["enabled"] == true {
                    "CREATED".into()
                } else {
                    "REMOVED".into()
                },
                reason: None,
            })
            .collect())
    }
}

#[derive(Default)]
struct FakeWakeups {
    events: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl NotificationEventSink for FakeWakeups {
    async fn ingest(
        &self,
        events: &[Value],
        shadow: bool,
    ) -> Result<Vec<IngestResult>, PublishFailure> {
        assert!(!shadow, "partner wake-ups are never shadow copies");
        self.events.lock().unwrap().extend(events.iter().cloned());
        Ok(events
            .iter()
            .map(|e| IngestResult {
                event_id: e["eventId"].as_str().unwrap().to_string(),
                outcome: "ACCEPTED".into(),
                reason: None,
            })
            .collect())
    }
}

struct Peers {
    directory: Arc<FakeDirectory>,
    registrations: Arc<FakeRegistrations>,
    wakeups: Arc<FakeWakeups>,
}

fn all_on() -> PartnerSettings {
    PartnerSettings {
        session_devices_enabled: true,
        push_enabled: true,
        message_sent_export_enabled: false,
    }
}

/// A realtime instance with the partner features of `settings`.
async fn start(settings: PartnerSettings) -> Option<(Rt, Peers)> {
    // The process-wide switches (policy baseline, MESSAGE_SENT recording) of this test
    // binary: on, as in a deployment with the partner flag on.
    messaging::partner::install_process_flags(&all_on());
    let peers = Peers {
        directory: Arc::new(FakeDirectory::default()),
        registrations: Arc::new(FakeRegistrations::default()),
        wakeups: Arc::new(FakeWakeups::default()),
    };
    let (d, r, w) = (
        peers.directory.clone(),
        peers.registrations.clone(),
        peers.wakeups.clone(),
    );
    let rt = Rt::with_state(test_settings(), move |s| {
        s.partner = Arc::new(Partner::new(settings, d).with_push(r, w));
    })
    .await?;
    Some((rt, peers))
}

/// An EXTERNAL application environment Platform Access recognizes as a valid delegated
/// scope (`first_party`: a FIRST_PARTY tenant instead).
fn environment(h: &Harness, first_party: bool) -> Customer {
    let c = h.customer(ALL_SCOPES);
    h.authority.delegated.lock().unwrap().insert(
        c.context.scope.environment_id,
        (c.context.scope, first_party, true),
    );
    c
}

/// A CF Meet user token for `principal`, scripted as Platform Access verifies it.
fn user_token(
    peers: &Peers,
    c: &Customer,
    principal: Uuid,
    msg: &[&str],
) -> (String, VerifiedUserToken) {
    let jti = Uuid::new_v4();
    let header = URL_SAFE_NO_PAD
        .encode(br#"{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"0011223344556677"}"#);
    let claims = URL_SAFE_NO_PAD.encode(json!({ "jti": jti, "sub": principal }).to_string());
    let token = format!("cfut_sbx_{header}.{claims}.c2lnbmF0dXJl");
    let verified = VerifiedUserToken {
        token_id: jti,
        tenant_id: c.context.scope.tenant_id,
        application_id: c.context.scope.application_id,
        environment_id: c.context.scope.environment_id,
        environment_kind: "SANDBOX".into(),
        credential_id: c.context.credential_id,
        service_account_id: c.context.scope.service_account_id,
        user: VerifiedUser {
            id: principal,
            external_id: Some("rider-42".into()),
            display_name: None,
            image_url: None,
            status: "ACTIVE".into(),
        },
        grants: Grants {
            msg: msg.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
        issued_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
    };
    peers
        .directory
        .tokens
        .lock()
        .unwrap()
        .insert(token.clone(), Ok(verified.clone()));
    (token, verified)
}

async fn exchange(h: &Harness, token: &str, body: Value) -> Response {
    h.send(
        Method::POST,
        Some(token),
        None,
        "/v1/messaging/user-sessions",
        Some(body),
        &[],
    )
    .await
}

/// A partner session (exchange) for `principal`; returns (client token, response).
async fn session(h: &Harness, peers: &Peers, c: &Customer, principal: Uuid) -> (String, Response) {
    let (token, _) = user_token(peers, c, principal, &["read", "send"]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    (r.json["clientToken"].as_str().unwrap().to_string(), r)
}

async fn with_client(
    h: &Harness,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Response {
    h.send(
        method,
        None,
        None,
        path,
        body,
        &[("authorization", &format!("CF-Client-Token {token}"))],
    )
    .await
}

async fn wait_for(h: &Harness, sql: &str) {
    for _ in 0..100 {
        if h.scalar(sql).await > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never true: {sql}");
}

// ---------------------------------------------------------------------------- tests

/// The happy path: exchange, MANAGED send / receive over REST and realtime, renewal, the
/// closed REST allowlist, the partner baseline and the MESSAGE_SENT fact.
#[tokio::test]
async fn partner_users_chat_in_managed_conversations_over_rest_and_realtime() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    let c = environment(h, false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let cid = cid(&conv);

    let (token, verified) = user_token(&peers, &c, alice, &["read", "send"]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/UserSession",
        &r.json,
    );
    assert_eq!(uuid(&r.json["principalId"]), alice);
    let device = uuid(&r.json["sessionDeviceId"]);
    let client = r.json["clientToken"].as_str().unwrap().to_string();
    let expires =
        chrono::DateTime::parse_from_rfc3339(r.json["clientTokenExpiresAt"].as_str().unwrap())
            .unwrap();
    assert!(
        expires <= Utc::now() + chrono::Duration::minutes(5),
        "short-lived"
    );
    // Bound to the binding digest, the installation and the minting credential; never an
    // E2EE device.
    let binding = messaging::partner::binding_reference(
        c.context.scope.environment_id,
        alice,
        "web-installation-1",
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_session_devices WHERE id = '{device}' AND binding_kind = 'PLATFORM_USER_TOKEN' \
             AND account_session_sha256 = '{binding}' AND installation_id = 'web-installation-1' \
             AND minting_credential_id = '{}' AND minting_service_account_id = '{}'",
            verified.credential_id, verified.service_account_id
        ))
        .await,
        1
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_devices WHERE principal_id = '{alice}'"
        ))
        .await,
        0
    );
    // A second exchange within 30 s reuses the verification (keyed by the token id).
    let calls = *peers.directory.verifications.lock().unwrap();
    let r = exchange(
        h,
        &token,
        json!({ "installationId": "web-installation-1", "sessionDeviceId": device }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        uuid(&r.json["sessionDeviceId"]),
        device,
        "renewed, same device"
    );
    assert_eq!(
        *peers.directory.verifications.lock().unwrap(),
        calls,
        "cached verification"
    );
    // Another installation cannot renew it.
    let r = exchange(
        h,
        &token,
        json!({ "installationId": "other-installation", "sessionDeviceId": device }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // REST: the MANAGED conversation, its members and messages; send.
    let r = with_client(h, &client, Method::GET, "/v1/conversations", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| uuid(&i["conversationId"]) == cid));
    let r = with_client(
        h,
        &client,
        Method::GET,
        &format!("/v1/conversations/{cid}/members"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = with_client(
        h,
        &client,
        Method::POST,
        &format!("/v1/conversations/{cid}/messages"),
        Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "on my way" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(uuid(&r.json["senderPrincipalId"]), alice);
    let r = with_client(
        h,
        &client,
        Method::GET,
        &format!("/v1/conversations/{cid}/messages"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.text.contains("on my way"));

    // The closed allowlist: nothing outside it (pins, E2EE / device routes, backups).
    for (m, path) in [
        (Method::GET, format!("/v1/conversations/{cid}/pins")),
        (
            Method::POST,
            format!("/v1/messaging/devices/{device}/key-packages"),
        ),
        (
            Method::GET,
            format!("/v1/messaging/devices/{device}/welcomes"),
        ),
        (Method::POST, format!("/v1/conversations/{cid}/mls/commits")),
        (Method::GET, "/v1/messaging/backups/latest".to_string()),
        (Method::GET, "/v1/messaging/search?q=way".to_string()),
    ] {
        let r = with_client(h, &client, m.clone(), &path, Some(json!({}))).await;
        assert_eq!(
            (r.code(), r.reason()),
            ("CLIENT_TOKENS_NOT_ENABLED", "PARTNER_ROUTE_NOT_ALLOWED"),
            "{m} {path}: {}",
            r.text
        );
    }
    // A customer route is never reached with a user token.
    let r = h
        .get(
            &Customer {
                token: token.clone(),
                context: c.context.clone(),
            },
            None,
            "/v1/conversations",
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);

    // Realtime: ticket, subscription and live delivery.
    let ticket = rt.ticket_with(&client).await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{}", ticket.text);
    let mut ws = rt
        .open(ticket.json["ticket"].as_str().unwrap(), None)
        .await
        .expect("connected");
    let sub = ws.subscribe(&conv, None).await;
    assert_eq!(sub["type"], "subscribed", "{sub}");
    h.send_managed(&c, &conv, bob, "delivered to the partner app")
        .await;
    let events = ws.events(1).await;
    assert!(
        events
            .iter()
            .any(|e| e["data"]["message"]["managed"]["text"] == "delivered to the partner app"),
        "{events:?}"
    );

    // The partner baseline applies to the EXTERNAL environment: MANAGED is the default
    // of every context the application did not set itself.
    let r = h.get(&c, None, "/v1/messaging/policy").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["applicationBaseline"], "CFMEET_PLATFORM_PARTNER",
        "{}",
        r.text
    );
    assert_eq!(
        r.json["effective"]["contexts"]["DIRECT"]["defaultProfile"], "MANAGED",
        "{}",
        r.text
    );
    assert_eq!(
        r.json["effective"]["attachments"]["enabled"], true,
        "{}",
        r.text
    );

    // MESSAGE_SENT facts of the EXTERNAL environment (ids, class, profile only).
    let env = c.context.scope.environment_id;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_message_sent_facts WHERE environment_id = '{env}' \
             AND context_class = 'GROUP' AND security_profile = 'MANAGED' AND environment_kind = 'SANDBOX'"
        ))
        .await,
        2
    );
    use cf_usage_facts::exporter::EventSource;
    let source = messaging::usage_export::MessageSent::new(h.conn().clone());
    let facts = source
        .facts_after(None, Utc::now() + chrono::Duration::minutes(1), 100)
        .await
        .unwrap();
    let mine: Vec<_> = facts
        .iter()
        .filter(|(_, f)| f.environment_id == env)
        .collect();
    assert_eq!(mine.len(), 2);
    for (_, f) in &mine {
        assert_eq!(
            (f.usage_type.as_str(), f.unit.as_str(), f.quantity),
            ("MESSAGE_SENT", "OPERATION", 1)
        );
        assert_eq!(f.dimensions["contextClass"], "GROUP");
        assert_eq!(f.dimensions["securityProfile"], "MANAGED");
        assert_eq!(f.digest, f.compute_digest());
        if messaging::usage_export::MessageSent::catalogued() {
            f.validate(cf_usage_facts::producers::MESSAGING)
                .expect("a valid MESSAGE_SENT fact");
        }
    }
    // Resuming after the first fact returns only the second.
    let after = source
        .facts_after(
            Some(&facts[0].0),
            Utc::now() + chrono::Duration::minutes(1),
            100,
        )
        .await
        .unwrap();
    assert_eq!(after.len(), facts.len() - 1);
    // FIRST_PARTY environments record none.
    let fp = h.first_party(true);
    let (fconv, fpeople) = h.managed_group(&fp, 2).await;
    h.send_managed(&fp, &fconv, fpeople[0], "first party").await;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_message_sent_facts WHERE environment_id = '{}'",
            fp.context.scope.environment_id
        ))
        .await,
        0
    );
}

/// With the flags off nothing partner-specific is reachable and Platform Access is never
/// asked; the first-party session-device path is unchanged with the flags on.
#[tokio::test]
async fn partner_features_are_off_by_default_and_first_party_paths_are_unchanged() {
    let Some((rt, peers)) = start(PartnerSettings::default()).await else {
        return;
    };
    let h = &rt.h;
    let c = environment(h, false);
    let alice = h.principal(&c);
    let (token, _) = user_token(&peers, &c, alice, &["read", "send"]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "FEATURE_DISABLED",
            "PARTNER_SESSION_DEVICES_DISABLED"
        ),
        "{}",
        r.text
    );
    let r = h
        .send(
            Method::PUT,
            Some(&token),
            None,
            "/v1/messaging/user-push-registrations/web-installation-1",
            Some(json!({ "platform": "FCM", "handle": "fcm-token" })),
            &[],
        )
        .await;
    assert_eq!(r.reason(), "PARTNER_PUSH_DISABLED", "{}", r.text);
    assert_eq!(*peers.directory.verifications.lock().unwrap(), 0);
    // No baseline is registered for an EXTERNAL environment while the flag is off.
    let r = h.get(&c, None, "/v1/messaging/policy").await;
    assert_eq!(r.json["applicationBaseline"], Value::Null, "{}", r.text);

    // First-party session devices keep working as before (and keep their refusals).
    let fp = h.first_party(true);
    let p = h.principal(&fp);
    let r = h
        .post(
            &fp,
            Some(p),
            "/v1/messaging/session-devices",
            json!({ "accountSessionRef": "a1".repeat(32), "notAfter": (h.now() + chrono::Duration::minutes(30)).to_rfc3339() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let ft = r.json["token"].as_str().unwrap().to_string();
    let r = with_client(h, &ft, Method::GET, "/v1/conversations", None).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CLIENT_TOKENS_NOT_ENABLED", "SESSION_DEVICE_REALTIME_ONLY"),
        "{}",
        r.text
    );
}

/// A FIRST_PARTY first-party session device stays realtime-only even with partner
/// devices enabled; a FIRST_PARTY tenant cannot exchange a user token; refused tokens,
/// missing grants and inactive users are refused.
#[tokio::test]
async fn only_active_external_users_with_a_msg_grant_get_partner_devices() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    // FIRST_PARTY tenant (Platform Access would refuse the token too).
    let fp = environment(h, true);
    let p = h.principal(&fp);
    let (token, _) = user_token(&peers, &fp, p, &["read"]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("PRINCIPAL_NOT_AUTHORIZED", "PARTNER_SESSIONS_EXTERNAL_ONLY"),
        "{}",
        r.text
    );
    let fpc = h.first_party(true);
    let fpp = h.principal(&fpc);
    let r = h
        .post(
            &fpc,
            Some(fpp),
            "/v1/messaging/session-devices",
            json!({ "accountSessionRef": "b2".repeat(32), "notAfter": (h.now() + chrono::Duration::minutes(30)).to_rfc3339() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = with_client(
        h,
        r.json["token"].as_str().unwrap(),
        Method::GET,
        "/v1/conversations",
        None,
    )
    .await;
    assert_eq!(r.reason(), "SESSION_DEVICE_REALTIME_ONLY", "{}", r.text);

    let c = environment(h, false);
    let alice = h.principal(&c);
    // No msg grant.
    let (token, _) = user_token(&peers, &c, alice, &[]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("INSUFFICIENT_SCOPE", "MSG_GRANT_REQUIRED"),
        "{}",
        r.text
    );
    // Read only: realtime works, sending does not.
    let (token, _) = user_token(&peers, &c, alice, &["read"]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let client = r.json["clientToken"].as_str().unwrap().to_string();
    assert_eq!(rt.ticket_with(&client).await.status, StatusCode::CREATED);
    // Refused tokens (Platform Access's codes), malformed tokens and installations.
    for code in [
        "USER_TOKEN_EXPIRED",
        "USER_TOKEN_REVOKED",
        "USER_TOKEN_INVALID",
    ] {
        let (token, _) = user_token(&peers, &c, alice, &["read"]);
        peers
            .directory
            .tokens
            .lock()
            .unwrap()
            .insert(token.clone(), Err(code));
        let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
        assert_eq!(
            (r.status, r.reason()),
            (StatusCode::UNAUTHORIZED, code),
            "{}",
            r.text
        );
    }
    let r = exchange(
        h,
        &c.token,
        json!({ "installationId": "web-installation-1" }),
    )
    .await;
    assert_eq!(r.reason(), "USER_TOKEN_REQUIRED", "{}", r.text);
    let r = exchange(
        h,
        "cfut_sbx_not-a-jws",
        json!({ "installationId": "web-installation-1" }),
    )
    .await;
    assert_eq!(r.reason(), "USER_TOKEN_INVALID", "{}", r.text);
    let (token, _) = user_token(&peers, &c, alice, &["read"]);
    let r = exchange(h, &token, json!({ "installationId": "bad id!" })).await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    // An inactive principal.
    let bob = h.principal(&c);
    h.set_principal(&c, bob, PrincipalStatus::Inactive);
    let (token, _) = user_token(&peers, &c, bob, &["read"]);
    let r = exchange(h, &token, json!({ "installationId": "web-installation-1" })).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("PRINCIPAL_NOT_AUTHORIZED", "PRINCIPAL_INACTIVE"),
        "{}",
        r.text
    );
}

/// PRIVATE_E2EE stays out of reach of partner devices (404 on REST, absent from lists,
/// E2EE_DEVICE_REQUIRED on realtime), and another tenant's conversations are 404.
#[tokio::test]
async fn partner_devices_never_reach_e2ee_or_another_tenant() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    let c = environment(h, false);
    // An explicit PRIVATE_E2EE DIRECT conversation (the partner baseline defaults to
    // MANAGED, PRIVATE_E2EE stays available).
    let mut a = h.member(&c, 1).await;
    let mut b = h.member(&c, 1).await;
    let r = h
        .create_conversation(
            &c,
            a.principal,
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": b.principal, "securityProfile": "PRIVATE_E2EE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let e2ee = r.json;
    h.establish_group(&c, &e2ee, &mut a, &mut [&mut b]).await;
    let eid = cid(&e2ee);
    let (client, _) = session(h, &peers, &c, a.principal).await;
    for (m, path, body) in [
        (Method::GET, format!("/v1/conversations/{eid}"), None),
        (
            Method::GET,
            format!("/v1/conversations/{eid}/messages"),
            None,
        ),
        (
            Method::GET,
            format!("/v1/conversations/{eid}/members"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/conversations/{eid}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "x" })),
        ),
    ] {
        let r = with_client(h, &client, m.clone(), &path, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{m} {path}: {}", r.text);
    }
    let r = with_client(h, &client, Method::GET, "/v1/conversations", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| uuid(&i["conversationId"]) != eid));
    // The backend still sees it (the filter is the partner device's only).
    let r = h.get(&c, Some(a.principal), "/v1/conversations").await;
    assert!(r.json["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| uuid(&i["conversationId"]) == eid));
    let ticket = rt.ticket_with(&client).await;
    let mut ws = rt
        .open(ticket.json["ticket"].as_str().unwrap(), None)
        .await
        .expect("connected");
    let refused = ws.subscribe(&e2ee, None).await;
    assert_eq!(refused["code"], "E2EE_DEVICE_REQUIRED", "{refused}");

    // Cross-tenant: tenant A's partner device against tenant B's MANAGED conversation.
    let other = environment(h, false);
    let (bconv, _) = h.managed_group(&other, 2).await;
    let bid = cid(&bconv);
    for (m, path, body) in [
        (Method::GET, format!("/v1/conversations/{bid}"), None),
        (
            Method::GET,
            format!("/v1/conversations/{bid}/messages"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/conversations/{bid}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "x" })),
        ),
    ] {
        let r = with_client(h, &client, m.clone(), &path, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{m} {path}: {}", r.text);
    }
    let sub = ws.subscribe(&bconv, None).await;
    assert_eq!(sub["code"], "NOT_FOUND", "{sub}");
    // A's user token cannot renew a device of tenant B either.
    let (bclient, br) = session(h, &peers, &other, h.principal(&other)).await;
    let (token, _) = user_token(&peers, &c, a.principal, &["read"]);
    let r = exchange(
        h,
        &token,
        json!({ "installationId": "web-installation-1", "sessionDeviceId": br.json["sessionDeviceId"] }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    drop(bclient);
}

/// Revoking the minting credential, suspending the user or revoke-sessions ends partner
/// devices: at use, and at once through the identity change feed (open sockets close).
#[tokio::test]
async fn revocation_ends_partner_devices_and_closes_their_connections() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    let c = environment(h, false);
    let (conv, people) = h.managed_group(&c, 3).await;
    let (alice, bob, carol) = (people[0], people[1], people[2]);

    // At use: the minting credential revoked before the device was used.
    let other_credential = h.credential(&c, ALL_SCOPES);
    let (token, mut v) = user_token(&peers, &c, carol, &["read", "send"]);
    v.credential_id = other_credential.context.credential_id;
    peers
        .directory
        .tokens
        .lock()
        .unwrap()
        .insert(token.clone(), Ok(v.clone()));
    let r = exchange(h, &token, json!({ "installationId": "carol-installation" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let carol_client = r.json["clientToken"].as_str().unwrap().to_string();
    peers
        .directory
        .revoked_credentials
        .lock()
        .unwrap()
        .insert(v.credential_id);
    let r = with_client(h, &carol_client, Method::GET, "/v1/conversations", None).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::UNAUTHORIZED, "SESSION_REVOKED"),
        "{}",
        r.text
    );

    // The feed: a CREDENTIAL change revokes the devices it minted and closes sockets.
    let (alice_client, r) = session(h, &peers, &c, alice).await;
    let alice_device = uuid(&r.json["sessionDeviceId"]);
    let ticket = rt.ticket_with(&alice_client).await;
    let mut ws = rt
        .open(ticket.json["ticket"].as_str().unwrap(), None)
        .await
        .expect("connected");
    assert_eq!(ws.subscribe(&conv, None).await["type"], "subscribed");
    peers.directory.changes.lock().unwrap().push(json!({
        "sequence": 1, "entityKind": "CREDENTIAL", "operation": "UPDATE", "entityKey": "c",
        "revision": 2, "occurredAt": h.now().to_rfc3339(),
        "snapshot": { "id": c.context.credential_id, "status": "REVOKED" },
    }));
    let after = messaging::partner::revocation::poll(&h.state, 0).await;
    assert_eq!(after, 1);
    let (code, _) = ws.closed().await;
    assert_eq!(code, Some(4401));
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_session_devices WHERE id = '{alice_device}' AND status = 'REVOKED' \
             AND revoked_reason = 'CREDENTIAL_REVOKED'"
        ))
        .await,
        1
    );
    let r = with_client(h, &alice_client, Method::GET, "/v1/conversations", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    peers.directory.changes.lock().unwrap().clear();

    // A suspended user: refused at use (Platform Access resolves the principal afresh),
    // and the feed revokes the device.
    let (bob_client, r) = session(h, &peers, &c, bob).await;
    let bob_device = uuid(&r.json["sessionDeviceId"]);
    assert_eq!(
        with_client(h, &bob_client, Method::GET, "/v1/conversations", None)
            .await
            .status,
        StatusCode::OK
    );
    h.set_principal(&c, bob, PrincipalStatus::Inactive);
    let r = with_client(h, &bob_client, Method::GET, "/v1/conversations", None).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::UNAUTHORIZED, "PRINCIPAL_INACTIVE"),
        "{}",
        r.text
    );
    peers.directory.changes.lock().unwrap().push(json!({
        "sequence": 2, "entityKind": "PRINCIPAL", "operation": "UPDATE", "entityKey": "p",
        "revision": 3, "occurredAt": h.now().to_rfc3339(),
        "snapshot": { "id": bob, "environmentId": c.context.scope.environment_id, "status": "SUSPENDED" },
    }));
    messaging::partner::revocation::poll(&h.state, 1).await;
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_session_devices WHERE id = '{bob_device}' AND status = 'REVOKED'")).await,
        1
    );

    // revoke-sessions (tokensNotBefore): devices created before it end.
    h.set_principal(
        &c,
        bob,
        PrincipalStatus::Active {
            kind: "EXTERNAL_APPLICATION_PRINCIPAL".into(),
        },
    );
    let fresh_credential = h.credential(&c, ALL_SCOPES);
    let (token, mut v) = user_token(&peers, &c, alice, &["read", "send"]);
    v.credential_id = fresh_credential.context.credential_id;
    peers
        .directory
        .tokens
        .lock()
        .unwrap()
        .insert(token.clone(), Ok(v));
    let r = exchange(h, &token, json!({ "installationId": "web-installation-2" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let device = uuid(&r.json["sessionDeviceId"]);
    h.clock.advance(chrono::Duration::seconds(1));
    peers.directory.changes.lock().unwrap().push(json!({
        "sequence": 3, "entityKind": "PRINCIPAL", "operation": "UPDATE", "entityKey": "p",
        "revision": 4, "occurredAt": h.now().to_rfc3339(),
        "snapshot": { "id": alice, "environmentId": c.context.scope.environment_id, "status": "ACTIVE",
                      "tokensNotBefore": h.now().to_rfc3339() },
    }));
    messaging::partner::revocation::poll(&h.state, 2).await;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_session_devices WHERE id = '{device}' AND status = 'REVOKED' AND revoked_reason = 'SESSIONS_REVOKED'"
        ))
        .await,
        1
    );
    // Replaying the whole feed never revokes a device created after the changes.
    h.clock.advance(chrono::Duration::seconds(1));
    let (later, r) = session(h, &peers, &c, bob).await;
    let later_device = uuid(&r.json["sessionDeviceId"]);
    peers
        .directory
        .changes
        .lock()
        .unwrap()
        .retain(|c| c["entityKind"] == "PRINCIPAL");
    messaging::partner::revocation::poll(&h.state, 0).await;
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_session_devices WHERE id = '{later_device}' AND status = 'ACTIVE'")).await,
        1
    );
    assert_eq!(
        with_client(h, &later, Method::GET, "/v1/conversations", None)
            .await
            .status,
        StatusCode::OK
    );
}

/// Tickets and WebSocket upgrades of partner devices accept the environment's allowed
/// websites in addition to the static allow-list; nothing else.
#[tokio::test]
async fn partner_websites_are_allowed_origins_of_their_own_environment_only() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    let c = environment(h, false);
    let other = environment(h, false);
    peers.directory.origins.lock().unwrap().insert(
        c.context.scope.environment_id,
        vec!["https://shop.example.com".into()],
    );
    peers.directory.origins.lock().unwrap().insert(
        other.context.scope.environment_id,
        vec!["https://other.example.com".into()],
    );
    let alice = h.principal(&c);
    let (client, _) = session(h, &peers, &c, alice).await;
    let ticket = |origin: &'static str| {
        let client = client.clone();
        async move {
            h.send(
                Method::POST,
                None,
                None,
                "/v1/realtime/tickets",
                None,
                &[
                    ("authorization", &format!("CF-Client-Token {client}")),
                    ("origin", origin),
                ],
            )
            .await
        }
    };
    let r = ticket("https://shop.example.com").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(
        r.headers.get("access-control-allow-origin").is_none(),
        "the gateway answers partner CORS"
    );
    let ok = r.json["ticket"].as_str().unwrap().to_string();
    for refused in [
        "https://other.example.com",
        "https://evil.example.com",
        "http://shop.example.com",
    ] {
        let r = ticket(refused).await;
        assert_eq!(r.reason(), "ORIGIN_NOT_ALLOWED", "{refused}: {}", r.text);
    }
    // The static allow-list keeps working.
    assert_eq!(
        ticket("https://app.example.test").await.status,
        StatusCode::CREATED
    );
    // WebSocket upgrades.
    assert!(rt.open(&ok, Some("https://shop.example.com")).await.is_ok());
    let t = rt.ticket_with(&client).await.json["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        rt.open(&t, Some("https://other.example.com")).await.err(),
        Some(403)
    );
    // A non-partner client (an E2EE device's token) never gets partner origins.
    let m = h.member(&c, 1).await;
    let token = rt.token(&c, &m).await;
    let r = h
        .send(
            Method::POST,
            None,
            None,
            "/v1/realtime/tickets",
            None,
            &[
                ("authorization", &format!("CF-Client-Token {token}")),
                ("origin", "https://shop.example.com"),
            ],
        )
        .await;
    assert_eq!(r.reason(), "ORIGIN_NOT_ALLOWED", "{}", r.text);
}

/// Partner push: registrations synchronised with the platform-principal subject (the
/// handle only in that body), removal, and metadata-only wake-ups of MANAGED messages
/// addressed to `platform-principal:<environmentId>:<principalId>`.
#[tokio::test]
async fn partner_push_registrations_and_wakeups_reach_the_notification_service() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    let c = environment(h, false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let env = c.context.scope.environment_id;
    let (bob_token, _) = user_token(&peers, &c, bob, &["read"]);
    let put = |platform: &'static str| {
        let t = bob_token.clone();
        async move {
            h.send(
                Method::PUT,
                Some(&t),
                None,
                "/v1/messaging/user-push-registrations/ios-installation-7",
                Some(json!({ "platform": platform, "handle": "apns-device-token-0123" })),
                &[],
            )
            .await
        }
    };
    let r = put("APNS").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/UserPushRegistration",
        &r.json,
    );
    assert_eq!(
        r.json,
        json!({ "installationId": "ios-installation-7", "platform": "APNS", "status": "ACTIVE" })
    );
    assert_eq!(put("APNS_VOIP").await.status, StatusCode::OK);
    assert_eq!(put("WEB_PUSH").await.code(), "VALIDATION_ERROR");
    {
        let items = peers.registrations.items.lock().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            json!({
                "subject": {
                    "kind": "platform-principal",
                    "tenantId": c.context.scope.tenant_id,
                    "applicationId": c.context.scope.application_id,
                    "environmentId": env,
                    "principalId": bob,
                    "installationId": "ios-installation-7",
                },
                "platform": "APNS",
                "handle": "apns-device-token-0123",
                "enabled": true,
            })
        );
    }
    // Messaging keeps no handle.
    assert!(!h.dump().await.contains("apns-device-token-0123"));

    // Alice's MANAGED message wakes Bob's installation, metadata only.
    h.send_managed(&c, &conv, alice, "secret text never in a push")
        .await;
    wait_for(
        h,
        &format!("SELECT count(*) FROM messaging_partner_push_publications WHERE environment_id = '{env}'"),
    )
    .await;
    let report = messaging::partner::push::relay_due(&h.state).await.unwrap();
    assert_eq!(report.acked, 1, "{report:?}");
    {
        let events = peers.wakeups.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e["eventType"], "messaging.message.created");
        assert_eq!(
            e["recipients"],
            json!([{ "ref": format!("platform-principal:{env}:{bob}") }])
        );
        assert_eq!(e["data"]["securityProfile"], "MANAGED");
        assert_eq!(uuid(&e["data"]["conversationId"]), cid(&conv));
        assert!(!e.to_string().contains("secret text"), "{e}");
    }
    // Bob's own message wakes nobody (Alice has no partner registration).
    h.send_managed(&c, &conv, bob, "from bob").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_partner_push_publications WHERE environment_id = '{env}'")).await,
        1
    );

    // Removal: every platform of the installation; then no more wake-ups.
    let r = h
        .send(
            Method::DELETE,
            Some(&bob_token),
            None,
            "/v1/messaging/user-push-registrations/ios-installation-7",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    {
        let items = peers.registrations.items.lock().unwrap();
        let removals: Vec<&Value> = items.iter().filter(|i| i["enabled"] == false).collect();
        assert_eq!(removals.len(), 3);
        assert!(removals
            .iter()
            .all(|i| i.get("handle").is_none() && i["subject"]["kind"] == "platform-principal"));
    }
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_partner_push_registrations WHERE environment_id = '{env}' AND status = 'ACTIVE'"
        ))
        .await,
        0
    );
    h.send_managed(&c, &conv, alice, "after removal").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_partner_push_publications WHERE environment_id = '{env}'")).await,
        1
    );
}

/// ADR 0062 §8: the Meeting Service (`messaging-meeting.sync-platform`) synchronises a
/// partner meeting's MANAGED chat in the EXTERNAL environment's own scope; partner users
/// read it with their partner session devices. Off with the partner flag off; never
/// PRIVATE_E2EE.
#[tokio::test]
async fn partner_meeting_chat_is_synchronised_in_the_partner_scope() {
    let Some((rt, peers)) = start(all_on()).await else {
        return;
    };
    let h = &rt.h;
    let meeting = common::peers().meeting;
    let c = environment(h, false);
    let system = h.principal_of_kind(&c, "BOT_OR_SERVICE");
    let alice = h.principal(&c);
    let scope = c.context.scope.delegated();
    let resolve = |profile: &'static str| {
        let (meeting, scope) = (meeting.clone(), scope.clone());
        async move {
            h.call_internal(
                &meeting,
                Method::POST,
                "/internal/v1/meeting-conversations",
                Some(json!({ "meetingRef": "conf:partner-1", "ownerPrincipalId": system, "securityProfile": profile })),
                Some(scope),
                &[],
            )
            .await
        }
    };
    let r = resolve("PRIVATE_E2EE").await;
    assert_eq!(
        r.reason(),
        "PARTNER_MEETING_CHAT_MANAGED_ONLY",
        "{}",
        r.text
    );
    let r = resolve("MANAGED").await;
    assert!(r.status.is_success(), "{}", r.text);
    let conv = r.json["conversation"].clone();
    let id = cid(&conv);
    let r = h
        .call_internal(
            &meeting,
            Method::PUT,
            &format!("/internal/v1/meeting-conversations/{id}/members/{alice}"),
            Some(json!({ "state": "ACTIVE", "sourceVersion": 1 })),
            Some(scope.clone()),
            &[],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let (client, _) = session(h, &peers, &c, alice).await;
    let r = with_client(
        h,
        &client,
        Method::GET,
        &format!("/v1/conversations/{id}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["securityProfile"], "MANAGED");

    // The lifecycle route accepts the partner scope too.
    let r = h
        .call_internal(
            &meeting,
            Method::POST,
            &format!("/internal/v1/meeting-conversations/{id}/lifecycle"),
            Some(json!({ "state": "ENDED", "sourceVersion": 2 })),
            Some(scope.clone()),
            &[],
        )
        .await;
    assert_ne!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_ne!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.text);
    // Another environment's principal is never admitted; nor a non-partner kind; nor a
    // non-system owner.
    let other = environment(h, false);
    let stranger = h.principal(&other);
    let r2 = resolve("MANAGED").await;
    assert!(r2.status.is_success(), "{}", r2.text);
    for (who, reason) in [
        (stranger, "PRINCIPAL_UNKNOWN"),
        (
            h.principal_of_kind(&c, "NATIVE_CF_USER"),
            "PARTNER_MEETING_MEMBER_KIND",
        ),
    ] {
        let r = h
            .call_internal(
                &meeting,
                Method::PUT,
                &format!("/internal/v1/meeting-conversations/{id}/members/{who}"),
                Some(json!({ "state": "ACTIVE", "sourceVersion": 3 })),
                Some(scope.clone()),
                &[],
            )
            .await;
        assert_eq!(
            (r.code(), r.reason()),
            ("PRINCIPAL_NOT_AUTHORIZED", reason),
            "{}",
            r.text
        );
    }
    let r = h
        .call_internal(
            &meeting,
            Method::POST,
            "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conf:partner-9", "ownerPrincipalId": alice, "securityProfile": "MANAGED" })),
            Some(scope.clone()),
            &[],
        )
        .await;
    assert_eq!(r.reason(), "PARTNER_MEETING_OWNER_KIND", "{}", r.text);
    // A peer without either meeting permission is refused.
    let r = h
        .call_internal(
            &common::peers().privacy_relay,
            Method::POST,
            "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conf:partner-1", "ownerPrincipalId": system, "securityProfile": "MANAGED" })),
            Some(scope.clone()),
            &[],
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED", "{}", r.text);

    // A Meeting Service holding ONLY messaging-meeting.sync-platform: the partner scope is
    // accepted, a FIRST_PARTY scope is refused.
    let (runtime, platform_only) = platform_only_meeting();
    let (d, rg, w) = (
        peers.directory.clone(),
        peers.registrations.clone(),
        peers.wakeups.clone(),
    );
    let Some(rt3) = Rt::with_state(test_settings(), move |s| {
        s.partner = Arc::new(Partner::new(all_on(), d).with_push(rg, w));
        s.service_auth = runtime;
    })
    .await
    else {
        return;
    };
    let h3 = &rt3.h;
    let partner = environment(h3, false);
    let first_party = environment(h3, true);
    for (env, ok) in [(&partner, true), (&first_party, false)] {
        let owner = h3.principal_of_kind(env, "BOT_OR_SERVICE");
        let r = h3
            .call_internal(
                &platform_only,
                Method::POST,
                "/internal/v1/meeting-conversations",
                Some(json!({ "meetingRef": "conf:p", "ownerPrincipalId": owner, "securityProfile": "MANAGED" })),
                Some(env.context.scope.delegated()),
                &[],
            )
            .await;
        if ok {
            assert!(r.status.is_success(), "{}", r.text);
        } else {
            assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED", "{}", r.text);
        }
    }

    // The partner flag off: an EXTERNAL scope is refused exactly as before.
    let Some((off, _)) = start(PartnerSettings::default()).await else {
        return;
    };
    let c2 = environment(&off.h, false);
    let system2 = off.h.principal_of_kind(&c2, "BOT_OR_SERVICE");
    let r = off
        .h
        .call_internal(
            &meeting,
            Method::POST,
            "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conf:partner-2", "ownerPrincipalId": system2, "securityProfile": "MANAGED" })),
            Some(c2.context.scope.delegated()),
            &[],
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.text);
}

/// The additive migration rolls back and re-applies cleanly (it is the newest of the
/// gap-closure history), and widens the baseline CHECK without touching existing rows.
#[tokio::test]
async fn the_partner_migration_rolls_back_and_reapplies() {
    use messaging_migration::MigratorTrait;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let last = messaging_migration::GapClosureMigrator::migrations()
        .last()
        .unwrap()
        .name()
        .to_string();
    assert_eq!(last, "m20261208_000036_partner_session_devices");
    messaging_migration::GapClosureMigrator::down(&db.conn, Some(1))
        .await
        .unwrap();
    let present = |t: &'static str| {
        let conn = db.conn.clone();
        async move {
            use sea_orm::ConnectionTrait;
            conn.query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT to_regclass('{t}') IS NOT NULL AS p"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<bool>("", "p")
            .unwrap()
        }
    };
    assert!(!present("messaging_message_sent_facts").await);
    assert!(!present("messaging_partner_push_publications").await);
    messaging_migration::GapClosureMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(present("messaging_message_sent_facts").await);
    assert!(present("messaging_partner_push_registrations").await);
}

/// Messaging's runtime trusting a Meeting Service that holds ONLY
/// `messaging-meeting.sync-platform`, and that Meeting Service's signer.
fn platform_only_meeting() -> (
    messaging::config::service_auth::ServiceAuthRuntime,
    Arc<cf_service_auth::ServiceTokenSigner>,
) {
    use cf_service_auth::{
        client::{InternalServiceClient, InternalTargets},
        services, ServiceId, ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock,
        TokenTtl, TrustPolicy,
    };
    let clock: SharedClock = Arc::new(SystemClock);
    let meeting_key = common::key("meeting-platform-only-1");
    let bundle = json!({
        "bundle_version": 1,
        "receiver": services::MESSAGING,
        "runtime_environment": ENV,
        "trusted_issuers": [{
            "issuer": services::MEETING_SERVICE,
            "audience": services::MESSAGING,
            "runtime_environment": ENV,
            "keys": [meeting_key.public_jwk()],
            "permissions": ["messaging-meeting.sync-platform"],
        }],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::messaging(), ENV).unwrap();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        ENV,
        common::key("messaging-test-2"),
        TokenTtl::default(),
        clock.clone(),
    ));
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock.clone()));
    let client = Arc::new(
        InternalServiceClient::new(
            signer.clone(),
            InternalTargets::new(ENV),
            Duration::from_secs(2),
        )
        .unwrap(),
    );
    let runtime =
        messaging::config::service_auth::ServiceAuthRuntime::new(signer, verifier, client).unwrap();
    let meeting = Arc::new(ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        ENV,
        meeting_key,
        TokenTtl::default(),
        clock,
    ));
    (runtime, meeting)
}

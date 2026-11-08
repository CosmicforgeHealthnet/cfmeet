//! Real Account, Platform Access, Communications, Calling, Messaging and LiveKit.
//! Isolated databases and first-party TEST scope; no frontend or telecom provider.
use anyhow::{bail, Context};
use base64::Engine;
use cf_service_auth::{
    services, ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use cfmeet_certification::{
    personal_stack::PersonalStack,
    provision::{Operator, Resp},
};
use futures_util::FutureExt;
use reqwest::Method;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

async fn request(
    s: &PersonalStack,
    owner: &str,
    token: Option<&str>,
    actor: Option<Uuid>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<Resp> {
    let mut r = s
        .http
        .request(method, format!("{}{path}", s.urls[owner]))
        .header("Idempotency-Key", Uuid::new_v4().to_string());
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    if let Some(p) = actor {
        r = r.header("CF-Acting-Principal", p.to_string());
    }
    if let Some(body) = body {
        r = r.json(&body);
    }
    Ok(Resp::from(r.send().await?).await)
}
fn status(r: Resp, expected: u16, operation: &str) -> anyhow::Result<Value> {
    if r.status != expected {
        bail!(
            "{operation}: expected {expected}, got {} code {:?}",
            r.status,
            r.code()
        );
    }
    Ok(r.body)
}
struct Account {
    id: Uuid,
    email: String,
    password: String,
    session: String,
    refresh: String,
    principal: Uuid,
    public_id: String,
}
async fn account(s: &PersonalStack) -> anyhow::Result<Account> {
    let email = format!(
        "communications-{}@acceptance.invalid",
        Uuid::new_v4().simple()
    );
    let password = format!("P9!{}", Uuid::new_v4().simple());
    let username = format!("member{}", &Uuid::new_v4().simple().to_string()[..10]);
    let r=request(s,"meeting-service",None,None,Method::POST,"/api/v1/auth/register",Some(json!({"email":email,"username":username,"password":password,"confirm_password":password}))).await?;
    if !(200..300).contains(&r.status) {
        bail!(
            "registered account setup failed: {} {:?}",
            r.status,
            r.code()
        );
    }
    let login = status(
        request(
            s,
            "meeting-service",
            None,
            None,
            Method::POST,
            "/api/v1/auth/login",
            Some(json!({"email":email,"password":password})),
        )
        .await?,
        200,
        "account login",
    )?;
    let session = login["access_token"]
        .as_str()
        .context("access token")?
        .to_owned();
    let refresh = login["refresh_token"]
        .as_str()
        .context("refresh token")?
        .to_owned();
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(session.split('.').nth(1).context("JWT claims")?)?,
    )?;
    let id = Uuid::parse_str(claims["sub"].as_str().context("JWT subject")?)?;
    let profile = status(
        request(
            s,
            "meeting-service",
            Some(&session),
            None,
            Method::GET,
            "/api/v2/communications/me",
            None,
        )
        .await?,
        200,
        "lazy native communications activation",
    )?;
    assert_eq!(profile["activationState"], "ACTIVE");
    assert_eq!(profile["phoneVerification"]["required"], false);
    assert!(profile.get("principalId").is_none());
    assert!(profile.get("email").is_none());
    // Test-only owner evidence. No service reads another service's database.
    let db = Database::connect(&s.owner_urls["meeting-service"]).await?;
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT principal_id::text FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1::uuid AND environment_id=$2::uuid",[id.to_string().into(),s.customer.environment_id.to_string().into()])).await?.context("stable Account-owned mapping")?;
    let principal = Uuid::parse_str(&row.try_get::<String>("", "principal_id")?)?;
    db.close().await?;
    Ok(Account {
        id,
        email,
        password,
        session,
        refresh,
        principal,
        public_id: profile["cfMeetId"].as_str().context("CF Meet ID")?.into(),
    })
}
async fn hub(
    s: &PersonalStack,
    a: &Account,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<Resp> {
    request(
        s,
        "meeting-service",
        Some(&a.session),
        None,
        method,
        &format!("/api/v2/communications{path}"),
        body,
    )
    .await
}
async fn create_call(
    s: &PersonalStack,
    a: &Account,
    b: &Account,
    endpoint: Uuid,
    kind: &str,
) -> anyhow::Result<Resp> {
    request(s,"calling",Some(&s.customer.secret),None,Method::POST,"/v1/calls",Some(json!({"kind":kind,"initiatorPrincipalId":a.principal,"initiatorEndpointId":endpoint,"targetPrincipalIds":[b.principal]}))).await
}
async fn direct(s: &PersonalStack, a: &Account, b: &Account) -> anyhow::Result<Resp> {
    request(s,"messaging",Some(&s.customer.secret),Some(a.principal),Method::POST,"/v1/conversations",Some(json!({"contextClass":"DIRECT","securityProfile":"MANAGED","counterpartPrincipalId":b.principal}))).await
}
async fn connected(s: &PersonalStack, a: &Account, b: &Account) -> anyhow::Result<()> {
    let r = status(
        hub(
            s,
            a,
            Method::POST,
            "/contact-requests",
            Some(json!({"cfMeetId":b.public_id})),
        )
        .await?,
        200,
        "connection request",
    )?;
    status(
        hub(
            s,
            b,
            Method::POST,
            &format!(
                "/contact-requests/{}/accept",
                r["requestId"].as_str().context("request id")?
            ),
            Some(json!({"expectedVersion":r["version"]})),
        )
        .await?,
        200,
        "connection acceptance",
    )?;
    Ok(())
}

async fn native_message_request(
    s: &PersonalStack,
    a: &Account,
    method: Method,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> anyhow::Result<Value> {
    status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            method,
            &format!("/api/v2/messaging/relay/v1/{path}"),
            body,
        )
        .await?,
        expected,
        "native Messaging relay",
    )
}

async fn enroll_native(
    s: &PersonalStack,
    a: &Account,
) -> anyhow::Result<cf_messaging_crypto::device::Device> {
    use cf_messaging_crypto::device::Device;
    let ch = status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            Method::POST,
            "/api/v2/messaging/devices/registration-challenges",
            Some(json!({})),
        )
        .await?,
        201,
        "native challenge",
    )?;
    let device = Device::generate(Uuid::parse_str(
        ch["deviceId"].as_str().context("device id")?,
    )?)?;
    let nonce: [u8; 32] = hex::decode(ch["nonce"].as_str().context("challenge nonce")?)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("nonce length"))?;
    let registration = device.registration_key_package(
        Uuid::parse_str(ch["challengeId"].as_str().context("challenge id")?)?,
        &nonce,
    )?;
    status(request(s,"meeting-service",Some(&a.session),None,Method::POST,
        "/api/v2/messaging/devices",Some(json!({"challengeId":ch["challengeId"],"keyPackage":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(registration)}))).await?,201,"native proof of possession")?;
    status(request(s,"meeting-service",Some(&a.session),None,Method::POST,
        &format!("/api/v2/messaging/devices/{}/key-packages",device.device_id()),
        Some(json!({"keyPackages":[base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(device.key_package()?)]}))).await?,201,"native KeyPackage")?;
    Ok(device)
}

/// Actual encrypted delivery through native authenticated Account APIs, five REAL
/// service processes and PostgreSQL. TEST runtime only; never production certification.
#[tokio::test]
async fn native_contacts_exchange_opaque_mls_messages_and_block_alternate_devices(
) -> anyhow::Result<()> {
    let mut s = PersonalStack::start().await?;
    let result = std::panic::AssertUnwindSafe(encrypted_messaging_journey(&mut s))
        .catch_unwind()
        .await;
    if !matches!(&result, Ok(Ok(()))) {
        s.dump_logs();
    }
    s.close().await;
    match result {
        Ok(r) => r,
        Err(p) => std::panic::resume_unwind(p),
    }
}

async fn encrypted_messaging_journey(s: &mut PersonalStack) -> anyhow::Result<()> {
    use cf_messaging_crypto::{
        binding::{application_message_aad, ApplicationMessageBinding},
        group::Processed,
    };
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let a = account(s).await?;
    let b = account(s).await?;
    let da = enroll_native(s, &a).await?;
    let db = enroll_native(s, &b).await?;
    // Principal impersonation headers do not establish the required relationship.
    let body = json!({"contextClass":"DIRECT","securityProfile":"PRIVATE_E2EE","counterpartPrincipalId":b.principal});
    native_message_request(
        s,
        &a,
        Method::POST,
        "conversations",
        Some(body.clone()),
        403,
    )
    .await?;
    connected(s, &a, &b).await?;
    native_message_request(s, &a, Method::POST, "conversations", Some(json!({
        "contextClass":"DIRECT", "counterpartPrincipalId":b.principal, "counterpartCfMeetId":b.public_id
    })), 400).await?;
    let body = json!({"contextClass":"DIRECT","securityProfile":"PRIVATE_E2EE","counterpartCfMeetId":b.public_id});
    let conversation = native_message_request(
        s,
        &a,
        Method::POST,
        "conversations",
        Some(body.clone()),
        201,
    )
    .await?;
    let id = conversation["conversationId"]
        .as_str()
        .context("conversation id")?;
    let reused =
        native_message_request(s, &a, Method::POST, "conversations", Some(body), 200).await?;
    assert_eq!(reused["conversationId"], conversation["conversationId"]);
    let mut ga = da.create_group(
        &b64.decode(
            conversation["encryption"]["mlsGroupId"]
                .as_str()
                .context("group id")?,
        )?,
    )?;
    let claim = native_message_request(
        s,
        &a,
        Method::POST,
        &format!("conversations/{id}/mls/key-package-claims"),
        Some(json!({"principalIds":[b.principal]})),
        200,
    )
    .await?;
    let packages = claim["keyPackages"]
        .as_array()
        .context("claimed packages")?
        .iter()
        .map(|k| b64.decode(k["keyPackage"].as_str().unwrap()))
        .collect::<Result<Vec<_>, _>>()?;
    let commit = da.prepare_add_devices(&mut ga, &packages)?;
    native_message_request(s,&a,Method::POST,&format!("conversations/{id}/mls/commits"),Some(json!({
        "clientMessageId":Uuid::new_v4(),"deviceId":da.device_id(),"epoch":commit.epoch,
        "commit":b64.encode(&commit.commit),"welcome":b64.encode(commit.welcome.as_ref().context("Welcome")?),
        "welcomeDeviceIds":[db.device_id()],"removedDeviceIds":[]})),201).await?;
    da.accept_pending_commit(&mut ga)?;
    let welcomes = native_message_request(
        s,
        &b,
        Method::GET,
        &format!("messaging/devices/{}/welcomes", db.device_id()),
        None,
        200,
    )
    .await?;
    let welcome = welcomes["welcomes"]
        .as_array()
        .context("pending Welcomes")?
        .iter()
        .find(|w| w["conversationId"] == conversation["conversationId"])
        .context("own Welcome")?;
    let mut gb = db.join(&b64.decode(welcome["welcome"].as_str().context("Welcome bytes")?)?)?;
    native_message_request(
        s,
        &b,
        Method::POST,
        &format!(
            "messaging/devices/{}/welcomes/{}/consume",
            db.device_id(),
            welcome["welcomeId"].as_str().unwrap()
        ),
        Some(json!({})),
        200,
    )
    .await?;
    let mut last_sequence = 0;
    let mut message_ids = Vec::new();
    // Bob remains offline between reads; canonical history catches up in order.
    for direction in 0..2 {
        let (sender, recipient, device, group, peer_device, peer_group) = if direction == 0 {
            (&a, &b, &da, &mut ga, &db, &mut gb)
        } else {
            (&b, &a, &db, &mut gb, &da, &mut ga)
        };
        let client_id = Uuid::new_v4();
        let aad = application_message_aad(&ApplicationMessageBinding {
            tenant_id: s.customer.tenant_id,
            application_id: s.customer.application_id,
            environment_id: s.customer.environment_id,
            conversation_id: Uuid::parse_str(id)?,
            client_message_id: client_id,
            sender_device_id: device.device_id(),
        });
        let plaintext = format!("test-only-{}", Uuid::new_v4());
        let ciphertext = device.encrypt(group, &aad, plaintext.as_bytes())?;
        let payload = json!({"clientMessageId":client_id,"deviceId":device.device_id(),"epoch":group.epoch(),"ciphertext":b64.encode(&ciphertext)});
        let sent = native_message_request(
            s,
            sender,
            Method::POST,
            &format!("conversations/{id}/messages"),
            Some(payload.clone()),
            201,
        )
        .await?;
        let replay = native_message_request(
            s,
            sender,
            Method::POST,
            &format!("conversations/{id}/messages"),
            Some(payload),
            200,
        )
        .await?;
        assert_eq!(sent["messageId"], replay["messageId"]);
        let sequence = sent["sequence"].as_i64().context("committed sequence")?;
        assert!(sequence > last_sequence);
        last_sequence = sequence;
        let message_id = sent["messageId"].as_str().context("message id")?;
        let received = native_message_request(
            s,
            recipient,
            Method::GET,
            &format!("conversations/{id}/messages/{message_id}"),
            None,
            200,
        )
        .await?;
        let bytes = b64.decode(
            received["message"]["e2ee"]["ciphertext"]
                .as_str()
                .context("stored ciphertext")?,
        )?;
        assert_eq!(bytes, ciphertext);
        match peer_device.process(peer_group, &bytes)? {
            Processed::Application {
                aad: actual,
                plaintext: opened,
            } => {
                assert_eq!(actual, aad);
                assert!(
                    opened == plaintext.as_bytes(),
                    "authorized client decryption did not match"
                );
            }
            _ => bail!("expected an authenticated application message"),
        }
        message_ids.push(message_id.to_owned());
        native_message_request(
            s,
            recipient,
            Method::PUT,
            &format!("conversations/{id}/read-cursor"),
            Some(json!({"sequence":sequence})),
            200,
        )
        .await?;
    }
    // Group resolution uses GROUP_INVITATION, independently of direct-chat privacy.
    let mut privacy = status(
        hub(s, &b, Method::GET, "/me/privacy", None).await?,
        200,
        "privacy",
    )?;
    privacy["privacy"]["directChatPolicy"] = json!("NOBODY");
    status(
        hub(
            s,
            &b,
            Method::PUT,
            "/me/privacy",
            Some(json!({
                "expectedVersion":privacy["version"], "privacy":privacy["privacy"]
            })),
        )
        .await?,
        200,
        "disable direct chat",
    )?;
    native_message_request(
        s,
        &a,
        Method::POST,
        "conversations",
        Some(json!({
            "contextClass":"DIRECT", "counterpartCfMeetId":b.public_id
        })),
        403,
    )
    .await?;
    let group = native_message_request(
        s,
        &a,
        Method::POST,
        "conversations",
        Some(json!({
            "contextClass":"GROUP", "securityProfile":"PRIVATE_E2EE"
        })),
        201,
    )
    .await?;
    let group_id = group["conversationId"].as_str().context("group id")?;
    native_message_request(
        s,
        &a,
        Method::POST,
        &format!("conversations/{group_id}/members"),
        Some(json!({"cfMeetId":b.public_id,"principalId":b.principal,"invite":true})),
        400,
    )
    .await?;
    native_message_request(
        s,
        &a,
        Method::POST,
        &format!("conversations/{group_id}/members"),
        Some(json!({"cfMeetId":b.public_id,"invite":true})),
        201,
    )
    .await?;
    privacy["privacy"]["directChatPolicy"] = json!("CONNECTIONS");
    status(
        hub(
            s,
            &b,
            Method::PUT,
            "/me/privacy",
            Some(json!({
                "expectedVersion":privacy["version"].as_i64().context("privacy version")? + 1,
                "privacy":privacy["privacy"]
            })),
        )
        .await?,
        200,
        "restore direct chat before independent block test",
    )?;
    let alt = enroll_native(s, &a).await?;
    status(
        hub(
            s,
            &b,
            Method::PUT,
            &format!("/profiles/{}/block", a.public_id),
            Some(json!({})),
        )
        .await?,
        200,
        "contact block",
    )?;
    native_message_request(
        s,
        &a,
        Method::POST,
        &format!("conversations/{group_id}/members"),
        Some(json!({"cfMeetId":b.public_id,"invite":true})),
        403,
    )
    .await?;
    native_message_request(s,&a,Method::POST,&format!("conversations/{id}/messages"),Some(json!({
        "clientMessageId":Uuid::new_v4(),"deviceId":alt.device_id(),"epoch":ga.epoch(),"ciphertext":b64.encode(&commit.commit)})),403).await?;
    status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            Method::POST,
            &format!("/api/v2/messaging/devices/{}/revoke", alt.device_id()),
            Some(json!({"reason":"COMPROMISED"})),
        )
        .await?,
        200,
        "device revocation",
    )?;
    let denied = request(
        s,
        "meeting-service",
        Some(&a.session),
        None,
        Method::POST,
        "/api/v2/messaging/client-tokens",
        Some(json!({"deviceId":alt.device_id()})),
    )
    .await?;
    assert!((400..500).contains(&denied.status));
    let owner = Database::connect(&s.owner_urls["messaging"]).await?;
    let row = owner.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT count(*) n FROM messaging_messages WHERE conversation_id=$1::uuid AND message_kind='APPLICATION' AND security_profile='PRIVATE_E2EE' AND mls_ciphertext IS NOT NULL AND managed_sealed IS NULL",
        [id.to_string().into()])).await?.context("ciphertext storage evidence")?;
    assert_eq!(row.try_get::<i64>("", "n")?, 2);
    owner.close().await?;
    println!("EVIDENCE native-messaging TEST conversation={id} messages={} encrypted-storage=PASS client-decryption=PASS replay=PASS block=PASS revocation=PASS",message_ids.join(","));
    Ok(())
}

#[tokio::test]
async fn personal_identity_contacts_and_authoritative_policy_across_real_services(
) -> anyhow::Result<()> {
    let mut s = PersonalStack::start().await?;
    let result = std::panic::AssertUnwindSafe(journey(&mut s))
        .catch_unwind()
        .await;
    if !matches!(&result, Ok(Ok(()))) {
        s.dump_logs();
    }
    s.close().await;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
async fn journey(s: &mut PersonalStack) -> anyhow::Result<()> {
    status(
        request(
            s,
            "meeting-service",
            None,
            None,
            Method::GET,
            "/api/v2/communications/me",
            None,
        )
        .await?,
        401,
        "consumer authentication",
    )
    .context("public ingress")?;
    let backfill_scope = DelegatedPlatformContext::new(
        s.customer.tenant_id,
        s.customer.application_id,
        s.customer.environment_id,
        ApplicationEnvironmentKind::Test,
        s.customer.service_account_id,
    );
    for expected in [2, 0] {
        let headers = s
            .keys
            .signer(services::OPERATOR_CONSOLE)
            .sign(
                &OutboundServiceRequest::new_operation(ServiceId::communications())
                    .with_delegated_context(backfill_scope.clone()),
            )?
            .headers();
        let result = Resp::from(
            s.http
                .post(format!(
                    "{}/internal/v1/communications/backfill",
                    s.urls["communications"]
                ))
                .headers(headers)
                .json(&json!({"limit":100,"cursor":null}))
                .send()
                .await?,
        )
        .await;
        let value = status(
            result,
            200,
            "bounded backfill of existing local and Google accounts",
        )?;
        assert_eq!(value["created"], expected);
    }
    let db = Database::connect(&s.owner_urls["meeting-service"]).await?;
    for id in &s.legacy_accounts {
        let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT auth_type::text AS auth, status::text AS status FROM users WHERE id=$1::uuid",
            [id.to_string().into()])).await?.context("pre-migration account remains present")?;
        assert!(["local", "google"].contains(&row.try_get::<String>("", "auth")?.as_str()));
        assert_eq!(row.try_get::<String>("", "status")?, "active");
    }
    db.close().await?;
    let (mut a, b) = tokio::try_join!(account(s), account(s))?;
    assert_ne!(a.principal, b.principal);
    assert_ne!(a.public_id, b.public_id);
    let swagger = status(
        request(
            s,
            "meeting-service",
            None,
            None,
            Method::GET,
            "/api-docs/communications.json",
            None,
        )
        .await?,
        200,
        "public Swagger family",
    )?;
    assert_eq!(
        swagger["paths"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_object().unwrap().len())
            .sum::<usize>(),
        73
    );
    assert!(swagger["paths"]
        .as_object()
        .unwrap()
        .keys()
        .all(|p| p.starts_with("/api/v2/communications/")));
    assert_eq!(
        request(
            s,
            "communications",
            None,
            None,
            Method::POST,
            "/internal/v1/communications/policy/check",
            Some(json!({}))
        )
        .await?
        .status,
        401
    );
    let capabilities = status(
        hub(s, &a, Method::GET, "/me/capabilities", None).await?,
        200,
        "actual capability readiness",
    )?;
    assert_eq!(capabilities["messaging"]["securityReady"], false);
    assert_eq!(capabilities["messaging"]["available"], false);
    assert_eq!(capabilities["phoneVerification"]["status"], "PROVIDER_BLOCKED");
    let policy = status(
        request(
            s,
            "messaging",
            Some(&s.customer.secret),
            None,
            Method::GET,
            "/v1/messaging/policy",
            None,
        )
        .await?,
        200,
        "canonical Messaging policy",
    )?;
    // Explicit MANAGED TEST policy tests the existing canonical backend without
    // claiming MLS activation or changing the production personal security policy.
    status(request(s,"messaging",Some(&s.customer.secret),None,Method::PUT,"/v1/messaging/policy",Some(json!({"expectedVersion":policy["version"],"policy":{"contexts":{"DIRECT":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"PRIVATE_E2EE","requestsRequireAcceptance":false},"GROUP":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"PRIVATE_E2EE"}}}}))).await?,200,"isolated managed test policy")?;
    let mut endpoints = Vec::new();
    for person in [&a, &b] {
        let r=status(request(s,"calling",Some(&s.customer.secret),None,Method::POST,"/v1/calling/endpoints",Some(json!({"principalId":person.principal,"endpointKey":format!("personal-{}",Uuid::new_v4()),"endpointKind":"WEB","capabilities":{"audio":true,"video":true}}))).await?,201,"canonical Calling endpoint")?;
        endpoints.push(Uuid::parse_str(
            r["endpointId"].as_str().context("endpoint")?,
        )?);
    }
    assert_eq!(
        create_call(s, &a, &b, endpoints[0], "VOICE").await?.code(),
        Some("TARGET_UNAVAILABLE")
    );
    assert_eq!(direct(s, &a, &b).await?.code(), Some("DELIVERY_REFUSED"));
    let invite = status(
        hub(
            s,
            &a,
            Method::POST,
            "/invites",
            Some(json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1})),
        )
        .await?,
        200,
        "opaque invitation",
    )?;
    let token = invite["payload"]
        .as_str()
        .context("invitation payload")?
        .rsplit('/')
        .next()
        .unwrap();
    let resolved = status(
        hub(
            s,
            &b,
            Method::POST,
            "/discovery/invites/resolve",
            Some(json!({"purpose":"CONNECT","token":token})),
        )
        .await?,
        200,
        "private invitation resolution",
    )?;
    assert_eq!(resolved["cfMeetId"], a.public_id);
    assert_eq!(resolved["relationship"], "NONE");
    assert!(resolved.get("principalId").is_none());
    assert!(resolved.get("email").is_none());
    connected(s, &a, &b).await?;
    let conversation = status(
        direct(s, &a, &b).await?,
        201,
        "accepted relationship creates canonical direct conversation",
    )?;
    let conversation_id = conversation["conversationId"]
        .as_str()
        .context("canonical conversation")?;
    assert_eq!(direct(s, &a, &b).await?.status, 200);
    status(request(s,"messaging",Some(&s.customer.secret),Some(a.principal),Method::POST,&format!("/v1/conversations/{conversation_id}/messages"),Some(json!({"clientMessageId":Uuid::new_v4(),"text":"Sections 4-7 isolated backend acceptance"}))).await?,201,"canonical message send after connection")?;
    for kind in ["VOICE", "VIDEO"] {
        let created = status(
            create_call(s, &a, &b, endpoints[0], kind).await?,
            201,
            "accepted relationship creates real LiveKit call",
        )?;
        let call = created["call"]["callId"]
            .as_str()
            .context("canonical call id")?;
        let current = status(
            request(
                s,
                "calling",
                Some(&s.customer.secret),
                None,
                Method::GET,
                &format!("/v1/calls/{call}"),
                None,
            )
            .await?,
            200,
            "canonical call state",
        )?;
        status(
            request(
                s,
                "calling",
                Some(&s.customer.secret),
                None,
                Method::POST,
                &format!("/v1/calls/{call}/cancel"),
                Some(json!({"expectedVersion":current["version"]})),
            )
            .await?,
            200,
            "call cancellation and provider cleanup",
        )?;
    }
    let contacts = status(
        hub(s, &a, Method::GET, "/contacts", None).await?,
        200,
        "owner contacts",
    )?;
    let contact = contacts["data"][0]["contactId"]
        .as_str()
        .context("contact id")?;
    status(
        hub(
            s,
            &a,
            Method::PATCH,
            &format!("/contacts/{contact}"),
            Some(json!({"expectedVersion":1,"alias":"Saved personal contact","favorite":true})),
        )
        .await?,
        200,
        "owner-specific alias",
    )?;
    let other = status(
        hub(s, &b, Method::GET, "/contacts", None).await?,
        200,
        "independent peer preferences",
    )?;
    assert_eq!(other["data"][0]["alias"], Value::Null);
    status(
        hub(
            s,
            &a,
            Method::GET,
            &format!("/contacts/{contact}/common-groups"),
            None,
        )
        .await?,
        200,
        "Messaging-owned common groups",
    )?;
    status(
        hub(
            s,
            &b,
            Method::PUT,
            &format!("/profiles/{}/block", a.public_id),
            None,
        )
        .await?,
        200,
        "immediate block",
    )?;
    assert_eq!(
        create_call(s, &a, &b, endpoints[0], "VOICE").await?.code(),
        Some("TARGET_UNAVAILABLE")
    );
    assert_eq!(direct(s, &a, &b).await?.code(), Some("DELIVERY_REFUSED"));
    assert_eq!(
        request(
            s,
            "messaging",
            Some(&s.customer.secret),
            Some(a.principal),
            Method::POST,
            &format!("/v1/conversations/{conversation_id}/messages"),
            Some(json!({"clientMessageId":Uuid::new_v4(),"text":"must be refused"}))
        )
        .await?
        .code(),
        Some("DELIVERY_REFUSED")
    );
    status(
        hub(
            s,
            &b,
            Method::DELETE,
            &format!("/profiles/{}/block", a.public_id),
            None,
        )
        .await?,
        200,
        "unblock preserves revoked relationship",
    )?;
    assert_eq!(direct(s, &a, &b).await?.code(), Some("DELIVERY_REFUSED"));
    connected(s, &a, &b).await?;
    let old = status(
        hub(s, &a, Method::GET, "/me", None).await?,
        200,
        "stable identity before rotation",
    )?;
    let rotated = status(
        hub(
            s,
            &a,
            Method::POST,
            "/me/public-id/rotate",
            Some(json!({"expectedVersion":old["version"]})),
        )
        .await?,
        200,
        "public identity rotation",
    )?;
    assert_ne!(rotated["cfMeetId"], a.public_id);
    assert_eq!(
        hub(
            s,
            &b,
            Method::POST,
            "/discovery/public-id/resolve",
            Some(json!({"cfMeetId":a.public_id}))
        )
        .await?
        .status,
        404
    );
    assert_eq!(
        hub(
            s,
            &b,
            Method::POST,
            "/discovery/invites/resolve",
            Some(json!({"purpose":"CONNECT","token":token}))
        )
        .await?
        .status,
        404
    );
    let operator = Operator::new(
        s.keys.signer(services::OPERATOR_CONSOLE),
        &s.urls["platform-access"],
        "http://127.0.0.1:9",
        s.http.clone(),
    );
    let foreign = operator
        .customer("FIRST_PARTY", "TEST", &["calls.read", "calls.write"])
        .await?;
    let scope = DelegatedPlatformContext::new(
        foreign.tenant_id,
        foreign.application_id,
        foreign.environment_id,
        ApplicationEnvironmentKind::Test,
        foreign.service_account_id,
    );
    let headers = s
        .keys
        .signer(services::CALLING)
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::communications())
                .with_delegated_context(scope),
        )?
        .headers();
    let denied=Resp::from(s.http.post(format!("{}/internal/v1/communications/policy/check",s.urls["communications"])).headers(headers).json(&json!({"action":"DIRECT_CALL","actorPrincipalId":a.principal,"targetPrincipalId":b.principal,"credentialIssuedAt":null,"credentialPrincipalId":null})).send().await?).await;
    assert!(denied.status == 403 || denied.body["allowed"] == false);
    let group = status(
        request(
            s,
            "messaging",
            Some(&s.customer.secret),
            Some(a.principal),
            Method::POST,
            "/v1/conversations",
            Some(json!({"contextClass":"GROUP","securityProfile":"MANAGED"})),
        )
        .await?,
        201,
        "active personal group creation",
    )?;
    let group_id = group["conversationId"].as_str().context("group id")?;
    let db = Database::connect(&s.owner_urls["meeting-service"]).await?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET status='suspended' WHERE id=$1::uuid",
        [a.id.to_string().into()],
    ))
    .await?;
    assert_eq!(request(s,"messaging",Some(&s.customer.secret),Some(a.principal),Method::POST,
        &format!("/v1/conversations/{group_id}/messages"),Some(json!({"clientMessageId":Uuid::new_v4(),"text":"must be denied for suspended account"}))).await?.code(),Some("DELIVERY_REFUSED"));
    assert_eq!(
        create_call(s, &a, &b, endpoints[0], "VOICE").await?.code(),
        Some("TARGET_UNAVAILABLE")
    );
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET status='active' WHERE id=$1::uuid",
        [a.id.to_string().into()],
    ))
    .await?;
    db.close().await?;
    assert_eq!(
        hub(s, &a, Method::GET, "/me", None).await?.status,
        401,
        "old native session cannot reactivate after suspension"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let login = status(
        request(
            s,
            "meeting-service",
            None,
            None,
            Method::POST,
            "/api/v1/auth/login",
            Some(json!({"email":a.email,"password":a.password})),
        )
        .await?,
        200,
        "fresh authentication after suspension",
    )?;
    a.session = login["access_token"]
        .as_str()
        .context("new access token")?
        .into();
    a.refresh = login["refresh_token"]
        .as_str()
        .context("new refresh token")?
        .into();
    // Section 14: reactivation also returns the Platform Access principal to ACTIVE
    // through Account's durable outbox (delivered within a worker tick).
    let mut resumed = hub(s, &a, Method::GET, "/me", None).await?;
    for _ in 0..40 {
        if resumed.status == 200 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        resumed = hub(s, &a, Method::GET, "/me", None).await?;
    }
    let resumed = status(
        resumed,
        200,
        "server-controlled reactivation with fresh credentials",
    )?;
    assert_eq!(resumed["cfMeetId"], rotated["cfMeetId"]);
    // N07 against the real Messaging process: its PushStatus `pushServices` is repeated
    // exactly as `providers`; without push credentials every service is NOT_CONFIGURED.
    let delivery = status(
        hub(s, &a, Method::GET, "/me/notification-delivery-status", None).await?,
        200,
        "truthful notification delivery status",
    )?;
    assert_eq!(
        delivery["providers"],
        json!({"APNS":"NOT_CONFIGURED","FCM":"NOT_CONFIGURED","WEB_PUSH":"NOT_CONFIGURED"})
    );
    assert!(delivery["devices"].is_array() && delivery["pendingJobs"].is_u64());
    assert_eq!(delivery["physicalDeviceAcceptance"], "NOT_RUN");
    // With Messaging unavailable the Hub reports a dependency failure, never a null or
    // optimistic provider state.
    s.stop("messaging").await;
    let down = hub(s, &a, Method::GET, "/me/notification-delivery-status", None).await?;
    assert_eq!(
        (down.status, down.code()),
        (503, Some("COMMUNICATIONS_UNAVAILABLE"))
    );
    s.restart("messaging").await?;
    status(
        hub(s, &a, Method::GET, "/me/notification-delivery-status", None).await?,
        200,
        "delivery status after Messaging recovers",
    )?;
    s.stop("communications").await;
    assert_eq!(
        create_call(s, &a, &b, endpoints[0], "VOICE").await?.status,
        503
    );
    assert_eq!(direct(s, &a, &b).await?.status, 503);
    // Meetings authenticate through their existing account path while Hub is stopped.
    status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            Method::GET,
            "/api/v2/meeting-spaces",
            None,
        )
        .await?,
        200,
        "Meetings remain available during Hub outage",
    )?;
    status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            Method::POST,
            "/api/v1/auth/logout",
            None,
        )
        .await?,
        200,
        "logout persists revocation during Hub outage",
    )?;
    assert_eq!(
        request(
            s,
            "meeting-service",
            None,
            None,
            Method::POST,
            "/api/v1/auth/refresh",
            Some(json!({"refresh_token":a.refresh}))
        )
        .await?
        .status,
        401
    );
    let db = Database::connect(&s.owner_urls["meeting-service"]).await?;
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT version,credentials_not_before IS NOT NULL AS revoked FROM account_communication_state WHERE user_id=$1::uuid",[a.id.to_string().into()])).await?.context("durable Account revocation")?;
    assert!(row.try_get::<bool>("", "revoked")?);
    db.close().await?;
    // Keep the fixture login values private even on assertion failure.
    assert!(!a.email.is_empty() && !a.password.is_empty());
    println!("PERSONAL_COMMUNICATIONS_REAL_E2E: identity_without_phone, principal_stability, public_swagger, private_internal_api, truthful_security_readiness, invitations, accepted_connections, direct_conversation_reuse, message_send_policy, voice_call_policy, video_call_policy, separate_aliases, common_groups, blocking, revocation, rotation, invitation_invalidation, tenant_isolation, truthful_delivery_status, delivery_status_owner_outage, fail_closed_outage, independent_meetings, durable_logout: PASS");
    Ok(())
}

// ---------------------------------------------------------------------------
// Blueprint v1.0 Section 14 (MSG-010, ACC-008, ACC-010): the Hub facades against the
// REAL Messaging, Calling and Meeting processes, device <-> account-session binding and
// account revocation fan-out for an account that never opened the Hub.
// ---------------------------------------------------------------------------

fn claims(token: &str) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token.split('.').nth(1).context("JWT claims")?)?,
    )?)
}
async fn login(s: &PersonalStack, email: &str, password: &str) -> anyhow::Result<String> {
    let login = status(
        request(
            s,
            "meeting-service",
            None,
            None,
            Method::POST,
            "/api/v1/auth/login",
            Some(json!({"email":email,"password":password})),
        )
        .await?,
        200,
        "additional sign-in",
    )?;
    Ok(login["access_token"]
        .as_str()
        .context("access token")?
        .to_owned())
}
/// Polls an owner database (test-only evidence; no service reads another's database).
async fn eventually(
    url: &str,
    sql: &str,
    values: Vec<sea_orm::Value>,
    what: &str,
) -> anyhow::Result<()> {
    let db = Database::connect(url).await?;
    for _ in 0..60 {
        if db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values.clone(),
            ))
            .await?
            .is_some()
        {
            db.close().await?;
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    db.close().await?;
    bail!("{what}: not observed within 15 s")
}
async fn endpoint(s: &PersonalStack, a: &Account) -> anyhow::Result<Uuid> {
    let r = status(request(s,"calling",Some(&s.customer.secret),None,Method::POST,"/v1/calling/endpoints",Some(json!({"principalId":a.principal,"endpointKey":format!("hub-{}",Uuid::new_v4()),"endpointKind":"WEB","capabilities":{"audio":true,"video":true}}))).await?,201,"canonical Calling endpoint")?;
    Ok(Uuid::parse_str(
        r["endpointId"].as_str().context("endpoint")?,
    )?)
}
async fn device_state(s: &PersonalStack, a: &Account, device: &str) -> anyhow::Result<Value> {
    let devices = status(
        hub(s, a, Method::GET, "/devices", None).await?,
        200,
        "device list",
    )?;
    devices["data"]
        .as_array()
        .context("devices")?
        .iter()
        .find(|d| d["deviceId"] == device)
        .cloned()
        .context("listed device")
}

#[tokio::test]
async fn hub_facades_device_sessions_and_revocation_fan_out_across_real_owners(
) -> anyhow::Result<()> {
    let mut s = PersonalStack::start().await?;
    let result = std::panic::AssertUnwindSafe(section_14(&mut s))
        .catch_unwind()
        .await;
    if !matches!(&result, Ok(Ok(()))) {
        s.dump_logs();
    }
    s.close().await;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
async fn section_14(s: &mut PersonalStack) -> anyhow::Result<()> {
    let (a, b) = tokio::try_join!(account(s), account(s))?;
    // MANAGED capability is reported apart from E2EE readiness: not allowed by the
    // default (E2EE-only) policy, available with live session-device delivery once the
    // isolated TEST policy allows MANAGED.
    let before = status(
        hub(s, &a, Method::GET, "/me/capabilities", None).await?,
        200,
        "capabilities before MANAGED",
    )?;
    assert_eq!(before["messaging"]["managed"]["available"], false);
    assert_eq!(before["messaging"]["securityReady"], false);
    let policy = status(
        request(
            s,
            "messaging",
            Some(&s.customer.secret),
            None,
            Method::GET,
            "/v1/messaging/policy",
            None,
        )
        .await?,
        200,
        "canonical Messaging policy",
    )?;
    status(request(s,"messaging",Some(&s.customer.secret),None,Method::PUT,"/v1/messaging/policy",Some(json!({"expectedVersion":policy["version"],"policy":{"contexts":{"DIRECT":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"MANAGED","requestsRequireAcceptance":false},"GROUP":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"MANAGED"}}}}))).await?,200,"isolated managed test policy")?;
    let after = status(
        hub(s, &a, Method::GET, "/me/capabilities", None).await?,
        200,
        "capabilities with MANAGED",
    )?;
    assert_eq!(after["messaging"]["available"], true, "{after}");
    assert_eq!(after["messaging"]["managed"]["realtime"], "SESSION_DEVICE");
    assert_eq!(after["messaging"]["privateE2ee"]["available"], false);

    // MSG-010: conversation access -> call launch -> timeline card -> meeting share.
    connected(s, &a, &b).await?;
    let conversation = status(direct(s, &a, &b).await?, 201, "direct conversation")?;
    let conversation = conversation["conversationId"]
        .as_str()
        .context("conversation")?
        .to_owned();
    let context = status(
        hub(
            s,
            &a,
            Method::GET,
            &format!("/conversations/{conversation}/context"),
            None,
        )
        .await?,
        200,
        "Hub conversation context from real Messaging access",
    )?;
    assert_eq!(context["contextClass"], "DIRECT");
    assert_eq!(context["securityProfile"], "MANAGED");
    assert_eq!(context["actions"]["voiceCall"], true);
    let ep = endpoint(s, &a).await?;
    let call = status(
        hub(
            s,
            &a,
            Method::POST,
            &format!("/conversations/{conversation}/calls"),
            Some(json!({"kind":"VOICE","clientRequestId":Uuid::new_v4(),"endpointId":ep})),
        )
        .await?,
        200,
        "Hub-launched call through real Calling",
    )?;
    let call_id = call["callId"].as_str().context("call id")?.to_owned();
    let current = status(
        request(
            s,
            "calling",
            Some(&s.customer.secret),
            None,
            Method::GET,
            &format!("/v1/calls/{call_id}"),
            None,
        )
        .await?,
        200,
        "canonical call created by the Hub facade",
    )?;
    assert_eq!(current["callId"], call_id.as_str());
    // Best-effort cleanup: the call may already have moved on (ringing timeout,
    // auto-end), which Calling answers with CALL_STATE_CONFLICT.
    let cleanup = request(
        s,
        "calling",
        Some(&s.customer.secret),
        None,
        Method::POST,
        &format!("/v1/calls/{call_id}/cancel"),
        Some(json!({"expectedVersion":current["version"]})),
    )
    .await?;
    assert!(
        cleanup.status == 200 || cleanup.code() == Some("CALL_STATE_CONFLICT"),
        "call cleanup: {} {:?}",
        cleanup.status,
        cleanup.code()
    );
    let instant = status(
        hub(
            s,
            &a,
            Method::POST,
            &format!("/conversations/{conversation}/meetings/instant"),
            Some(json!({"clientRequestId":Uuid::new_v4(),"title":"Section 14 journey"})),
        )
        .await?,
        200,
        "instant meeting through real Meetings",
    )?;
    let code = instant["meetingCode"]
        .as_str()
        .context("meeting code")?
        .to_owned();
    let timeline = status(
        hub(
            s,
            &b,
            Method::GET,
            &format!("/conversations/{conversation}/timeline"),
            None,
        )
        .await?,
        200,
        "peer timeline",
    )?;
    let kinds: Vec<&str> = timeline["data"]
        .as_array()
        .context("timeline")?
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"CALL"), "{timeline}");
    assert!(kinds.contains(&"MEETING_INVITATION"), "{timeline}");
    let group = status(
        request(
            s,
            "messaging",
            Some(&s.customer.secret),
            Some(a.principal),
            Method::POST,
            "/v1/conversations",
            Some(json!({"contextClass":"GROUP","securityProfile":"MANAGED"})),
        )
        .await?,
        201,
        "group for the meeting share",
    )?;
    let group = group["conversationId"]
        .as_str()
        .context("group")?
        .to_owned();
    status(
        hub(
            s,
            &a,
            Method::POST,
            &format!("/conversations/{group}/meetings/share"),
            Some(json!({"clientRequestId":Uuid::new_v4(),"meetingCode":code})),
        )
        .await?,
        200,
        "owner shares the meeting into another conversation",
    )?;
    let inbox = status(
        hub(s, &a, Method::GET, "/inbox", None).await?,
        200,
        "membership-checked inbox",
    )?;
    assert!(inbox["data"]
        .as_array()
        .context("inbox")?
        .iter()
        .any(|r| r["conversationId"] == conversation.as_str()));

    // ACC-008: a device is bound to the sign-in that linked it. Ending that sign-in
    // revokes the device at Calling; revoking a device ends the sign-in it was bound to.
    let second = login(s, &a.email, &a.password).await?;
    let second_session = claims(&second)?["sid"]
        .as_str()
        .context("session id")?
        .to_owned();
    let as_second = Account {
        session: second.clone(),
        ..clone_account(&a)
    };
    let ep1 = endpoint(s, &a).await?;
    let linked = status(
        hub(
            s,
            &as_second,
            Method::POST,
            "/devices/link",
            Some(json!({"platform":"WEB","callEndpointId":ep1})),
        )
        .await?,
        200,
        "device linked from the second sign-in",
    )?;
    assert_eq!(linked["device"]["accountSession"]["bound"], true);
    let device1 = linked["device"]["deviceId"]
        .as_str()
        .context("device")?
        .to_owned();
    status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            Method::POST,
            &format!("/api/v2/me/sessions/{second_session}/revoke"),
            None,
        )
        .await?,
        200,
        "sign out the second session",
    )?;
    let mut ended = Value::Null;
    for _ in 0..60 {
        ended = device_state(s, &a, &device1).await?;
        if ended["state"] == "REVOKED" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(ended["state"], "REVOKED", "{ended}");
    assert_eq!(ended["revocationReason"], "ACCOUNT_SESSION_ENDED");
    let third = login(s, &a.email, &a.password).await?;
    let as_third = Account {
        session: third.clone(),
        ..clone_account(&a)
    };
    let ep2 = endpoint(s, &a).await?;
    let linked = status(
        hub(
            s,
            &as_third,
            Method::POST,
            "/devices/link",
            Some(json!({"platform":"ANDROID","callEndpointId":ep2})),
        )
        .await?,
        200,
        "device linked from the third sign-in",
    )?;
    let device2 = linked["device"]["deviceId"]
        .as_str()
        .context("device")?
        .to_owned();
    let revoked = status(
        hub(
            s,
            &a,
            Method::POST,
            &format!("/devices/{device2}/revoke"),
            Some(json!({"reason":"COMPROMISED"})),
        )
        .await?,
        200,
        "revoke the lost device",
    )?;
    assert_eq!(revoked["effects"]["accountSession"], "ENDED");
    assert!(
        revoked.get("boundAccountSessionRef").is_none(),
        "the session reference never reaches the client"
    );
    assert_eq!(
        request(
            s,
            "meeting-service",
            Some(&third),
            None,
            Method::GET,
            "/api/v2/me/sessions",
            None
        )
        .await?
        .status,
        401,
        "the lost device's sign-in is over"
    );
    status(
        request(
            s,
            "meeting-service",
            Some(&a.session),
            None,
            Method::GET,
            "/api/v2/me/sessions",
            None,
        )
        .await?,
        200,
        "other sign-ins continue",
    )?;

    // ACC-010: an account that uses Messaging through the web bridge but never opened
    // the Hub. A password change reaches Messaging and Calling by principal, and a
    // suspension is mirrored onto its Platform Access principal (and back).
    let email = format!("no-hub-{}@acceptance.invalid", Uuid::new_v4().simple());
    let password = format!("P9!{}", Uuid::new_v4().simple());
    let username = format!("nohub{}", &Uuid::new_v4().simple().to_string()[..10]);
    let r=request(s,"meeting-service",None,None,Method::POST,"/api/v1/auth/register",Some(json!({"email":email,"username":username,"password":password,"confirm_password":password}))).await?;
    if !(200..300).contains(&r.status) {
        bail!("registration failed: {} {:?}", r.status, r.code());
    }
    let token = login(s, &email, &password).await?;
    let user = claims(&token)?["sub"]
        .as_str()
        .context("subject")?
        .to_owned();
    status(
        request(
            s,
            "meeting-service",
            Some(&token),
            None,
            Method::POST,
            "/api/v2/messaging/session-tokens",
            Some(json!({})),
        )
        .await?,
        201,
        "MANAGED realtime through the bridge (no Hub profile)",
    )?;
    let db = Database::connect(&s.owner_urls["meeting-service"]).await?;
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT principal_id::text FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1::uuid",[user.clone().into()])).await?.context("bridge principal")?;
    let principal = row.try_get::<String>("", "principal_id")?;
    db.close().await?;
    let hub_db = Database::connect(&s.owner_urls["communications"]).await?;
    assert!(hub_db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM communication_profiles WHERE principal_id=$1::uuid",
            [principal.clone().into()]
        ))
        .await?
        .is_none());
    hub_db.close().await?;
    let new_password = format!("Q8!{}", Uuid::new_v4().simple());
    status(
        request(
            s,
            "meeting-service",
            Some(&token),
            None,
            Method::POST,
            "/api/v1/auth/change-password",
            Some(json!({"current_password":password,"new_password":new_password,"confirm_password":new_password})),
        )
        .await?,
        200,
        "password change",
    )?;
    eventually(
        &s.owner_urls["messaging"],
        "SELECT 1 FROM messaging_communication_revocations WHERE principal_id=$1::uuid",
        vec![principal.clone().into()],
        "Messaging revocation for an account without a Hub profile",
    )
    .await?;
    eventually(
        &s.owner_urls["calling"],
        "SELECT 1 FROM calling_communication_revocations WHERE principal_id=$1::uuid",
        vec![principal.clone().into()],
        "Calling revocation for an account without a Hub profile",
    )
    .await?;
    let db = Database::connect(&s.owner_urls["meeting-service"]).await?;
    for status_value in ["suspended", "active"] {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET status=$2::user_status WHERE id=$1::uuid",
            [user.clone().into(), status_value.into()],
        ))
        .await?;
        let expected = if status_value == "suspended" {
            "SUSPENDED"
        } else {
            "ACTIVE"
        };
        eventually(
            &s.owner_urls["platform-access"],
            "SELECT 1 FROM platform_principals WHERE id=$1::uuid AND status=$2",
            vec![principal.clone().into(), expected.into()],
            "Platform Access principal mirrors the account status",
        )
        .await?;
    }
    db.close().await?;
    assert!(!a.email.is_empty() && !a.password.is_empty() && !b.email.is_empty());
    println!("SECTION_14_REAL_E2E: managed_capability, conversation_context, hub_call_launch, timeline_cards, instant_meeting, meeting_share, membership_inbox, session_ends_devices, device_ends_session, reference_never_relayed, revocation_without_profile_messaging, revocation_without_profile_calling, principal_status_mirror: PASS");
    Ok(())
}
fn clone_account(a: &Account) -> Account {
    Account {
        id: a.id,
        email: a.email.clone(),
        password: a.password.clone(),
        session: a.session.clone(),
        refresh: a.refresh.clone(),
        principal: a.principal,
        public_id: a.public_id.clone(),
    }
}

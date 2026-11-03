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

#[tokio::test]
async fn personal_identity_contacts_and_authoritative_policy_across_real_services(
) -> anyhow::Result<()> {
    let mut s = PersonalStack::start().await?;
    let result = journey(&mut s).await;
    if result.is_err() {
        s.dump_logs();
    }
    s.close().await;
    result
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
    let (a, b) = tokio::try_join!(account(s), account(s))?;
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
        29
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
    assert_eq!(capabilities["phoneVerification"]["status"], "DEFERRED");
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
    status(request(s,"messaging",Some(&s.customer.secret),None,Method::PUT,"/v1/messaging/policy",Some(json!({"expectedVersion":policy["version"],"policy":{"contexts":{"DIRECT":{"allowedProfiles":["MANAGED","PRIVATE_E2EE"],"defaultProfile":"PRIVATE_E2EE"}}}}))).await?,200,"isolated managed test policy")?;
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
    println!("PERSONAL_COMMUNICATIONS_REAL_E2E: identity_without_phone, principal_stability, public_swagger, private_internal_api, truthful_security_readiness, invitations, accepted_connections, direct_conversation_reuse, message_send_policy, voice_call_policy, video_call_policy, separate_aliases, common_groups, blocking, revocation, rotation, invitation_invalidation, tenant_isolation, fail_closed_outage, independent_meetings, durable_logout: PASS");
    Ok(())
}

//! Final closure (Blueprint Section 18: J-04, J-05, J-06, J-10, J-14, J-19): the REAL
//! Organizations Service, Meeting Service, Platform Access, Messaging and Calling as
//! processes — each with its own database and migrations, real Ed25519 service
//! authentication and trust bundles derived from the permission registry — driven through
//! the Meeting Service's public first-party API exactly as the web and mobile apps call it.
//! No owner is simulated.
//!
//! Journey:
//! 1. An owner registers, verifies and signs in; creates an organization (one canonical
//!    UUID, creator and creation time); the Organizations worker provisions its own
//!    FIRST_PARTY scope in Platform Access and verifies it (READY).
//! 2. The owner invites an employee by e-mail; only the invited account can accept; the
//!    workspace list shows the organization.
//! 3. An operator binds a Messaging service account inside the organization's scope; the
//!    owner and the employee exchange messages in an organization conversation through
//!    the organization Messaging relay; the employee holds an organization realtime
//!    subscription. The employee also has a conversation in a separate first-party scope
//!    (the personal-scope stand-in: a different environment and principal).
//! 4. The owner removes the employee. Organizations delivers `membership.removed` to the
//!    Meeting Service (signed outbox delivery), which suspends the employee's organization
//!    principal in Platform Access: the organization relay refuses the employee, Messaging
//!    itself refuses the organization principal (PRINCIPAL_INACTIVE) and closes its
//!    realtime connection — while the employee's other-scope principal keeps working.
//! 5. Suspending the organization fails organization operations closed (relay and
//!    Messaging: the tenant is suspended in Platform Access); reactivation restores them.
//! 6. A second organization with an overlapping member: conversations never cross.
//! 7. Delivery evidence: every consumer row delivered, none dead-lettered.

use std::time::Duration;

use cf_service_auth::{
    services, ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, ServiceId,
};
use cfmeet_certification::{
    provision::{register_user, Resp, User},
    stack::{self, Stack},
    ws, StackConfig, StackOptions,
};
use reqwest::Method;
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

fn tag() -> String {
    Uuid::new_v4().simple().to_string()[..10].to_string()
}

fn email(role: &str) -> String {
    format!("{role}-{}@cert.example", tag())
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

async fn send(
    stack: &Stack,
    method: Method,
    url: String,
    bearer: Option<&str>,
    headers: Vec<(String, String)>,
    body: Option<Value>,
) -> Resp {
    let mut r = stack.http.request(method, url);
    if let Some(t) = bearer {
        r = r.bearer_auth(t);
    }
    for (k, v) in headers {
        r = r.header(k, v);
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    Resp::from(r.send().await.expect("request sent")).await
}

/// A public first-party Meeting Service call with the user's session.
async fn meeting(
    stack: &Stack,
    method: Method,
    path: &str,
    user: &User,
    body: Option<Value>,
) -> Resp {
    send(
        stack,
        method,
        format!("{}{path}", stack.url(stack::MEETING_SERVICE)),
        Some(&user.session),
        vec![],
        body,
    )
    .await
}

/// The organization Messaging relay (`/api/v2/organizations/{id}/messaging/relay/{rest}`).
async fn org_messaging(
    stack: &Stack,
    org: Uuid,
    method: Method,
    rest: &str,
    user: &User,
    body: Option<Value>,
) -> Resp {
    let idem = vec![("idempotency-key".to_string(), Uuid::new_v4().to_string())];
    send(
        stack,
        method,
        format!(
            "{}/api/v2/organizations/{org}/messaging/relay/{rest}",
            stack.url(stack::MEETING_SERVICE)
        ),
        Some(&user.session),
        idem,
        body,
    )
    .await
}

/// A call signed with `issuer`'s real key (optionally in a delegated scope).
#[allow(clippy::too_many_arguments)]
async fn signed(
    stack: &Stack,
    issuer: &str,
    audience: ServiceId,
    base: &str,
    method: Method,
    path: &str,
    context: Option<DelegatedPlatformContext>,
    extra: Vec<(String, String)>,
    body: Option<Value>,
) -> Resp {
    let mut request = OutboundServiceRequest::new_operation(audience);
    if let Some(c) = context {
        request = request.with_delegated_context(c);
    }
    let signed = stack.signer(issuer).sign(&request).expect("signed");
    let mut headers: Vec<(String, String)> = signed
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    headers.extend(extra);
    send(stack, method, format!("{base}{path}"), None, headers, body).await
}

/// One verified organization scope.
#[derive(Debug, Clone, Copy)]
struct Scope {
    tenant: Uuid,
    application: Uuid,
    environment: Uuid,
}

impl Scope {
    fn context(
        &self,
        service_account: Uuid,
        kind: ApplicationEnvironmentKind,
    ) -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            self.tenant,
            self.application,
            self.environment,
            kind,
            service_account,
        )
    }
}

async fn eventually<F, Fut>(what: &str, within: Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("{what} did not happen within {within:?}");
}

/// Creates an organization through the Meeting Service and waits for READY provisioning.
async fn organization(stack: &Stack, owner: &User, name: &str) -> (Uuid, Scope) {
    let slug = format!("cert-{}", tag());
    let r = meeting(
        stack,
        Method::POST,
        "/api/v2/organizations",
        owner,
        Some(json!({ "name": name, "slug": slug })),
    )
    .await;
    status(&r, 201, "create organization");
    let id = Uuid::parse_str(r.body["id"].as_str().unwrap()).expect("canonical UUID");
    // J-04: one canonical UUID, creator and creation time recorded by Organizations.
    assert_eq!(r.body["createdBy"], json!(owner.id), "creator recorded");
    assert!(r.body["createdAt"].is_string(), "creation time recorded");
    assert_eq!(r.body["slug"], json!(slug));
    assert_eq!(r.body["viewer"]["role"], "OWNER");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let scope = loop {
        let p = meeting(
            stack,
            Method::GET,
            &format!("/api/v2/organizations/{id}/provisioning"),
            owner,
            None,
        )
        .await;
        if p.body["provisioningState"] == "READY" {
            let s = &p.body["scopes"][0];
            let field = |k: &str| Uuid::parse_str(s[k].as_str().unwrap()).unwrap();
            break Scope {
                tenant: field("tenantId"),
                application: field("applicationId"),
                environment: field("environmentId"),
            };
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "provisioning READY: {}",
            p.text
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    // The scope exists in Platform Access as a FIRST_PARTY tenant (truthful provisioning).
    let operator = stack.operator();
    let t = operator
        .pa(
            Method::GET,
            &format!("/admin/tenants/{}", scope.tenant),
            None,
        )
        .await
        .unwrap();
    status(&t, 200, "provisioned tenant in Platform Access");
    assert_eq!(t.body["classification"], "FIRST_PARTY", "{}", t.text);
    (id, scope)
}

/// Operator step: a Messaging service account inside the organization's own scope, bound
/// in Organizations; the organization environment's policy allows MANAGED conversations.
async fn enable_messaging(stack: &Stack, org: Uuid, scope: Scope) -> Uuid {
    let operator = stack.operator();
    operator
        .grant_scopes(scope.application, &["messages.read"])
        .await
        .expect("grant a standard scope");
    let sa = operator
        .service_account(scope.environment)
        .await
        .expect("service account in the organization scope");
    let (credential, _secret) = operator
        .credential(sa, &["messages.read"])
        .await
        .expect("credential");
    let r = signed(
        stack,
        services::OPERATOR_CONSOLE,
        ServiceId::organizations(),
        stack.url(stack::ORGANIZATIONS),
        Method::PUT,
        &format!(
            "/internal/v1/admin/organizations/{org}/scopes/primary/service-bindings/messaging"
        ),
        None,
        vec![("x-cf-operator".into(), "cfmeet-certification".into())],
        Some(json!({ "serviceAccountId": sa, "credentialId": credential })),
    )
    .await;
    status(&r, 200, "bind the organization's Messaging service account");
    // Organization environments are PRODUCTION-kind (ADR 0058).
    let ctx = || Some(scope.context(sa, ApplicationEnvironmentKind::Production));
    let messaging = stack.url(stack::MESSAGING);
    let current = signed(
        stack,
        services::MEETING_SERVICE,
        ServiceId::messaging(),
        messaging,
        Method::GET,
        "/internal/v1/delegated/v1/messaging/policy",
        ctx(),
        vec![],
        None,
    )
    .await;
    status(&current, 200, "read the organization Messaging policy");
    let managed =
        json!({ "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" });
    let r = signed(
        stack,
        services::MEETING_SERVICE,
        ServiceId::messaging(),
        messaging,
        Method::PUT,
        "/internal/v1/delegated/v1/messaging/policy",
        ctx(),
        vec![],
        Some(json!({ "expectedVersion": current.body["version"],
                     "policy": { "contexts": { "GROUP": managed, "DIRECT": managed } } })),
    )
    .await;
    status(&r, 200, "allow MANAGED in the organization environment");
    sa
}

/// The member's principal in an environment (as the Meeting Service recorded it).
async fn principal_in(stack: &Stack, user: Uuid, environment: Uuid) -> Option<Uuid> {
    let db = stack.db(stack::MEETING_SERVICE).await.unwrap();
    db.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        format!(
            "SELECT principal_id::text FROM communication_principal_links WHERE subject_kind = 'USER' AND subject_id = '{user}' AND environment_id = '{environment}'"
        ),
    ))
    .await
    .unwrap()
    .map(|r| Uuid::parse_str(&r.try_get_by_index::<String>(0).unwrap()).unwrap())
}

/// Messaging called directly (not through the Meeting relay) as `principal` in a scope.
async fn as_principal(
    stack: &Stack,
    ctx: DelegatedPlatformContext,
    principal: Uuid,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Resp {
    let ctx = ctx.with_initiating_principal(principal, DelegatedPrincipalKind::NativeCfUser);
    signed(
        stack,
        services::MEETING_SERVICE,
        ServiceId::messaging(),
        stack.url(stack::MESSAGING),
        method,
        path,
        Some(ctx),
        vec![("idempotency-key".into(), Uuid::new_v4().to_string())],
        body,
    )
    .await
}

fn text_message(text: &str) -> Value {
    json!({ "clientMessageId": Uuid::new_v4(), "text": text })
}

fn texts(list: &Resp) -> Vec<String> {
    list.body["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter_map(|x| x["managed"]["text"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn organization_lifecycle_across_real_services() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    let options = StackOptions::default()
        .with_organizations()
        // Realtime sessions are revalidated every 5 s (default 30 s) to bound the test.
        .set(
            stack::MESSAGING,
            "MESSAGING_REALTIME_REVALIDATE_SECONDS",
            "5",
        );
    let stack = Stack::start_with(config, options)
        .await
        .expect("certification stack with organizations starts");
    let meeting_url = stack.url(stack::MEETING_SERVICE).to_string();
    let redis = stack.config.redis_url.clone();
    let owner = register_user(&meeting_url, &redis, &stack.http, &email("owner"))
        .await
        .unwrap();
    let employee = register_user(&meeting_url, &redis, &stack.http, &email("employee"))
        .await
        .unwrap();
    let stranger = register_user(&meeting_url, &redis, &stack.http, &email("stranger"))
        .await
        .unwrap();

    // ---- J-04: creation, canonical identity, truthful provisioning.
    let (org, scope) = organization(&stack, &owner, "Lagos General").await;

    println!(
        "ORG_LIFECYCLE STEP J-04 created {org}, provisioned scope env {}",
        scope.environment
    );
    // ---- J-05: e-mail-bound invitation, acceptance, workspace list.
    let inv = meeting(
        &stack,
        Method::POST,
        &format!("/api/v2/organizations/{org}/invitations"),
        &owner,
        Some(json!({ "email": employee.email, "role": "MEMBER" })),
    )
    .await;
    status(&inv, 201, "invite the employee");
    let token = inv.body["token"]
        .as_str()
        .expect("one-time token")
        .to_string();
    let wrong = meeting(
        &stack,
        Method::POST,
        "/api/v2/organization-invitations/accept",
        &stranger,
        Some(json!({ "token": token })),
    )
    .await;
    assert!(
        wrong.status == 403 || wrong.status == 404,
        "another account cannot accept: {}",
        wrong.text
    );
    let accepted = meeting(
        &stack,
        Method::POST,
        "/api/v2/organization-invitations/accept",
        &employee,
        Some(json!({ "token": token })),
    )
    .await;
    status(&accepted, 200, "the invited account accepts");
    let membership = Uuid::parse_str(accepted.body["id"].as_str().unwrap()).unwrap();
    let membership_version = accepted.body["version"].as_i64().unwrap();
    let ws_list = meeting(
        &stack,
        Method::GET,
        "/api/v2/me/workspaces",
        &employee,
        None,
    )
    .await;
    status(&ws_list, 200, "workspaces");
    assert!(
        ws_list.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["organizationId"] == json!(org)),
        "the organization is in the employee's workspaces: {}",
        ws_list.text
    );
    // A non-member learns nothing about the organization.
    let r = meeting(
        &stack,
        Method::GET,
        &format!("/api/v2/organizations/{org}"),
        &stranger,
        None,
    )
    .await;
    status(&r, 404, "non-member read");

    println!("ORG_LIFECYCLE STEP J-05 invitation accepted, membership {membership}");
    // ---- J-10 (part 1): organization conversation in the organization's own scope.
    let sa = enable_messaging(&stack, org, scope).await;
    let r = org_messaging(
        &stack,
        org,
        Method::POST,
        "v1/conversations",
        &owner,
        Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "title": "Ward 7" })),
    )
    .await;
    status(&r, 201, "owner creates an organization conversation");
    let conversation = r.body["conversationId"].as_str().unwrap().to_string();
    let r = org_messaging(
        &stack,
        org,
        Method::GET,
        "v1/conversations",
        &employee,
        None,
    )
    .await;
    status(&r, 200, "employee lists organization conversations");
    // The owner resolves the colleague to their organization-scope principal as the apps
    // do (only active members of this organization are answered).
    let r = meeting(
        &stack,
        Method::POST,
        &format!("/api/v2/organizations/{org}/messaging/members/resolve"),
        &owner,
        Some(json!({ "userIds": [employee.id, stranger.id] })),
    )
    .await;
    status(&r, 200, "resolve organization colleagues");
    let people = r.body["people"].as_array().unwrap().clone();
    assert_eq!(
        people.len(),
        1,
        "a non-member is never resolved: {}",
        r.text
    );
    assert_eq!(people[0]["userId"], json!(employee.id));
    let employee_org_principal =
        Uuid::parse_str(people[0]["principalId"].as_str().unwrap()).unwrap();
    assert_eq!(
        principal_in(&stack, employee.id, scope.environment).await,
        Some(employee_org_principal),
        "the same principal the relay acts as"
    );
    let r = org_messaging(
        &stack,
        org,
        Method::POST,
        &format!("v1/conversations/{conversation}/members"),
        &owner,
        Some(json!({ "principalId": employee_org_principal })),
    )
    .await;
    status(&r, 201, "owner adds the employee");
    let r = org_messaging(
        &stack,
        org,
        Method::POST,
        &format!("v1/conversations/{conversation}/messages"),
        &owner,
        Some(text_message("handover at 7")),
    )
    .await;
    status(&r, 201, "owner sends");
    let r = org_messaging(
        &stack,
        org,
        Method::POST,
        &format!("v1/conversations/{conversation}/messages"),
        &employee,
        Some(text_message("acknowledged")),
    )
    .await;
    status(&r, 201, "employee sends");
    let list = org_messaging(
        &stack,
        org,
        Method::GET,
        &format!("v1/conversations/{conversation}/messages"),
        &employee,
        None,
    )
    .await;
    status(&list, 200, "employee reads");
    assert_eq!(texts(&list), ["handover at 7", "acknowledged"]);

    // Organization realtime: the employee subscribes to the organization conversation.
    let t = meeting(
        &stack,
        Method::POST,
        &format!("/api/v2/organizations/{org}/messaging/session-tokens"),
        &employee,
        Some(json!({})),
    )
    .await;
    status(&t, 201, "organization realtime token");
    let client_token = t.body["token"].as_str().unwrap().to_string();
    let ticket = send(
        &stack,
        Method::POST,
        format!("{}/v1/realtime/tickets", stack.url(stack::MESSAGING)),
        None,
        vec![(
            "authorization".into(),
            format!("CF-Client-Token {client_token}"),
        )],
        Some(json!({})),
    )
    .await;
    status(&ticket, 201, "realtime ticket");
    let proto = format!(
        "cfmeet.messaging.v1, cfmeet.ticket.{}",
        ticket.body["ticket"].as_str().unwrap()
    );
    let mut socket = ws::open(
        &format!("{}/v1/realtime", stack.url(stack::MESSAGING)),
        Some(&proto),
        None,
    )
    .await
    .unwrap()
    .expect("organization realtime connection");
    let wait = Duration::from_secs(10);
    socket.next_of("welcome", wait).await.unwrap();
    socket
        .send_json(
            &json!({ "type": "subscribe", "conversationId": conversation, "requestId": "s1" }),
        )
        .await
        .unwrap();
    socket.next_of("subscribed", wait).await.unwrap();

    // The employee's conversation in a separate first-party scope (personal stand-in).
    let operator = stack.operator();
    let other = operator
        .customer("FIRST_PARTY", "TEST", &["messages.read"])
        .await
        .unwrap();
    let other_scope = Scope {
        tenant: other.tenant_id,
        application: other.application_id,
        environment: other.environment_id,
    };
    let other_ctx =
        || other_scope.context(other.service_account_id, ApplicationEnvironmentKind::Test);
    let ensure = signed(&stack, services::MEETING_SERVICE, ServiceId::platform_access(), stack.url(stack::PLATFORM_ACCESS),
        Method::POST, "/internal/v1/principals/ensure", Some(other_ctx()), vec![],
        Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": format!("cfmeet-user:{}", employee.id) }))).await;
    assert!(
        (200..300).contains(&ensure.status),
        "principal in the other scope: {}",
        ensure.text
    );
    let employee_other_principal =
        Uuid::parse_str(ensure.body["principal"]["id"].as_str().unwrap()).unwrap();
    assert_ne!(
        employee_other_principal, employee_org_principal,
        "different scopes, different principals"
    );
    let policy = as_principal(
        &stack,
        other_ctx(),
        employee_other_principal,
        Method::GET,
        "/internal/v1/delegated/v1/messaging/policy",
        None,
    )
    .await;
    let managed =
        json!({ "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" });
    let r = signed(&stack, services::MEETING_SERVICE, ServiceId::messaging(), stack.url(stack::MESSAGING), Method::PUT,
        "/internal/v1/delegated/v1/messaging/policy", Some(other_ctx()), vec![],
        Some(json!({ "expectedVersion": policy.body["version"], "policy": { "contexts": { "GROUP": managed } } }))).await;
    status(&r, 200, "allow MANAGED in the other scope");
    let r = as_principal(
        &stack,
        other_ctx(),
        employee_other_principal,
        Method::POST,
        "/internal/v1/delegated/v1/conversations",
        Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "title": "Family" })),
    )
    .await;
    status(&r, 201, "employee's conversation in the other scope");
    let personal_conversation = r.body["conversationId"].as_str().unwrap().to_string();

    println!("ORG_LIFECYCLE STEP J-10a organization conversation {conversation}: 2 messages, realtime subscribed");
    // ---- J-10 (part 2): offboarding.
    let r = meeting(
        &stack,
        Method::PATCH,
        &format!("/api/v2/organizations/{org}/members/{membership}"),
        &owner,
        Some(json!({ "expectedVersion": membership_version, "status": "REMOVED" })),
    )
    .await;
    status(&r, 200, "owner removes the employee");
    // Organizations refuses at once (the relay asks Organizations on every request).
    let r = org_messaging(
        &stack,
        org,
        Method::GET,
        &format!("v1/conversations/{conversation}/messages"),
        &employee,
        None,
    )
    .await;
    assert!(
        r.status == 403 || r.status == 404,
        "removed member refused by the relay: {}",
        r.text
    );
    let r = org_messaging(
        &stack,
        org,
        Method::POST,
        &format!("v1/conversations/{conversation}/messages"),
        &employee,
        Some(text_message("still here?")),
    )
    .await;
    assert!(
        r.status == 403 || r.status == 404,
        "removed member cannot send: {}",
        r.text
    );
    // The event reaches the Meeting Service, which suspends the organization principal:
    // Messaging itself now refuses it, whoever presents it.
    let org_ctx = || scope.context(sa, ApplicationEnvironmentKind::Production);
    eventually(
        "Messaging refuses the removed member's organization principal",
        Duration::from_secs(60),
        || async {
            let r = as_principal(
                &stack,
                org_ctx(),
                employee_org_principal,
                Method::GET,
                &format!("/internal/v1/delegated/v1/conversations/{conversation}/messages"),
                None,
            )
            .await;
            r.status == 403 && r.body["error"]["reason"] == "PRINCIPAL_INACTIVE"
        },
    )
    .await;
    // The open realtime connection is closed at the next revalidation.
    let mut closed = false;
    for _ in 0..12 {
        match socket.next_json(Duration::from_secs(5)).await {
            Ok(v) if v["type"] == "error" => {
                closed = true;
                break;
            }
            Ok(_) => {}
            Err(e) => {
                closed = e.to_string().contains("closed");
                if closed {
                    break;
                }
            }
        }
    }
    assert!(
        closed,
        "the removed member's organization realtime connection was revoked"
    );
    // The other-scope (personal stand-in) principal keeps working.
    let r = as_principal(
        &stack,
        other_ctx(),
        employee_other_principal,
        Method::POST,
        &format!("/internal/v1/delegated/v1/conversations/{personal_conversation}/messages"),
        Some(text_message("dinner at 8")),
    )
    .await;
    status(
        &r,
        201,
        "personal-scope messaging unaffected by organization offboarding",
    );
    // The owner keeps organization access; history is retained for remaining members.
    let list = org_messaging(
        &stack,
        org,
        Method::GET,
        &format!("v1/conversations/{conversation}/messages"),
        &owner,
        None,
    )
    .await;
    status(&list, 200, "owner reads after the removal");
    assert_eq!(texts(&list), ["handover at 7", "acknowledged"]);

    println!("ORG_LIFECYCLE STEP J-10b removal: relay refused, PRINCIPAL_INACTIVE, realtime closed, other scope still sends");
    // ---- J-14: organization suspension fails closed; reactivation restores.
    let r = meeting(
        &stack,
        Method::POST,
        &format!("/api/v2/organizations/{org}/suspend"),
        &owner,
        Some(json!({})),
    )
    .await;
    status(&r, 200, "suspend the organization");
    let r = org_messaging(&stack, org, Method::GET, "v1/conversations", &owner, None).await;
    assert!(
        r.status == 409 || r.status == 403,
        "suspended organization refused: {}",
        r.text
    );
    let owner_org_principal = principal_in(&stack, owner.id, scope.environment)
        .await
        .unwrap();
    eventually(
        "Messaging refuses the suspended organization's scope",
        Duration::from_secs(60),
        || async {
            let r = as_principal(
                &stack,
                org_ctx(),
                owner_org_principal,
                Method::GET,
                "/internal/v1/delegated/v1/conversations",
                None,
            )
            .await;
            r.status == 403 && r.body["error"]["code"] == "AUTHORITY_SUSPENDED"
        },
    )
    .await;
    let r = meeting(
        &stack,
        Method::POST,
        &format!("/api/v2/organizations/{org}/reactivate"),
        &owner,
        Some(json!({})),
    )
    .await;
    status(&r, 200, "reactivate the organization");
    eventually(
        "the reactivated organization's Messaging works again",
        Duration::from_secs(60),
        || async {
            org_messaging(
                &stack,
                org,
                Method::GET,
                &format!("v1/conversations/{conversation}/messages"),
                &owner,
                None,
            )
            .await
            .status
                == 200
        },
    )
    .await;

    println!(
        "ORG_LIFECYCLE STEP J-14 suspended (relay refused, AUTHORITY_SUSPENDED) and reactivated"
    );
    // ---- J-06: a second organization with an overlapping member; nothing crosses.
    let (org_b, scope_b) = organization(&stack, &employee, "Abuja Clinic").await;
    let inv = meeting(
        &stack,
        Method::POST,
        &format!("/api/v2/organizations/{org_b}/invitations"),
        &employee,
        Some(json!({ "email": owner.email, "role": "ADMIN" })),
    )
    .await;
    status(&inv, 201, "invite the overlapping member");
    let r = meeting(
        &stack,
        Method::POST,
        "/api/v2/organization-invitations/accept",
        &owner,
        Some(json!({ "token": inv.body["token"] })),
    )
    .await;
    status(&r, 200, "overlapping member joins organization B");
    enable_messaging(&stack, org_b, scope_b).await;
    assert_ne!(
        scope_b.environment, scope.environment,
        "distinct technical scopes"
    );
    let r = org_messaging(
        &stack,
        org_b,
        Method::POST,
        "v1/conversations",
        &owner,
        Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "title": "B only" })),
    )
    .await;
    status(&r, 201, "conversation in organization B");
    let b_conversation = r.body["conversationId"].as_str().unwrap().to_string();
    let a_list = org_messaging(&stack, org, Method::GET, "v1/conversations", &owner, None).await;
    status(&a_list, 200, "organization A conversations");
    assert!(
        !a_list.text.contains(&b_conversation),
        "B's conversation never appears in A"
    );
    let r = org_messaging(
        &stack,
        org,
        Method::GET,
        &format!("v1/conversations/{b_conversation}/messages"),
        &owner,
        None,
    )
    .await;
    assert_eq!(
        r.status, 404,
        "B's conversation is unknown in A's scope: {}",
        r.text
    );
    // The removed employee administers B but still cannot reach A.
    let r = org_messaging(
        &stack,
        org,
        Method::GET,
        "v1/conversations",
        &employee,
        None,
    )
    .await;
    assert!(r.status == 403 || r.status == 404, "{}", r.text);
    // Roles are independent per organization.
    let caps_a = meeting(
        &stack,
        Method::GET,
        &format!("/api/v2/me/workspaces/{org}/capabilities"),
        &owner,
        None,
    )
    .await;
    let caps_b = meeting(
        &stack,
        Method::GET,
        &format!("/api/v2/me/workspaces/{org_b}/capabilities"),
        &owner,
        None,
    )
    .await;
    assert_eq!(
        (caps_a.body["role"].as_str(), caps_b.body["role"].as_str()),
        (Some("OWNER"), Some("ADMIN"))
    );

    println!("ORG_LIFECYCLE STEP J-06 second organization {org_b}: no conversation crosses, roles OWNER/ADMIN");
    // ---- J-19: delivery evidence — signed outbox delivery, nothing dead-lettered, the
    // removal applied by the consumer exactly once.
    let r = signed(
        &stack,
        services::OPERATOR_CONSOLE,
        ServiceId::organizations(),
        stack.url(stack::ORGANIZATIONS),
        Method::GET,
        "/internal/v1/admin/event-delivery",
        None,
        vec![("x-cf-operator".into(), "cfmeet-certification".into())],
        None,
    )
    .await;
    status(&r, 200, "event delivery status");
    let meeting_dest = r.body["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["destination"] == "cfmeet.meeting-service")
        .expect("Meeting consumer")
        .clone();
    assert!(
        meeting_dest["delivered"].as_i64().unwrap() > 0,
        "{meeting_dest}"
    );
    assert_eq!(meeting_dest["dead"], 0, "{meeting_dest}");
    let db = stack.db(stack::MEETING_SERVICE).await.unwrap();
    let applied = db.query_one(Statement::from_string(sea_orm::DbBackend::Postgres, format!(
        "SELECT count(*) FROM organization_event_inbox WHERE organization_id = '{org}' AND event_type = 'membership.removed' AND outcome = 'APPLIED'"
    ))).await.unwrap().unwrap().try_get_by_index::<i64>(0).unwrap();
    assert_eq!(applied, 1, "the removal was applied exactly once");
    let access = db.query_one(Statement::from_string(sea_orm::DbBackend::Postgres, format!(
        "SELECT applied_state FROM organization_member_access WHERE organization_id = '{org}' AND user_id = '{}'", employee.id
    ))).await.unwrap().unwrap().try_get_by_index::<Option<String>>(0).unwrap();
    assert_eq!(access.as_deref(), Some("REMOVED"));

    println!(
        "ORG_LIFECYCLE STEP J-19 delivered={} dead=0, removal applied once",
        meeting_dest["delivered"]
    );
    println!("ORGANIZATION_LIFECYCLE_REAL_E2E: J-04 J-05 J-06 J-10 J-14 J-19: PASS");
    let _ = socket.close().await;
    stack.teardown().await;
}

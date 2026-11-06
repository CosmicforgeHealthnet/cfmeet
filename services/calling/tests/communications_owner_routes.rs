//! Sections 9–23 Calling owner interfaces (CL-1 … CL-5) used by the Communications Hub,
//! end to end through the real router, the real Platform Access and a scripted Hub
//! policy endpoint:
//!
//! * every route refuses a caller without its receiver-granted permission (403), a
//!   request without a delegated context (403), a scope other than the governed personal
//!   scope (404) and a body principal other than the delegated NATIVE_CF_USER (403);
//! * CL-1 runs the canonical creation path: the personal policy refusal is the same 409
//!   TARGET_UNAVAILABLE (and runs before replay), a foreign initiator endpoint is 404,
//!   the call rings, a replay returns the same call, Hub keys never meet customer
//!   `Idempotency-Key` values, and without a launch authority it fails closed (503);
//! * CL-2 / CL-3 return only the delegated user's calls, in the canonical history shape;
//! * CL-4 / CL-5 see and revoke only the delegated user's endpoints.

mod common;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{Method, StatusCode},
    routing::post,
    Json, Router,
};
use cf_communications_policy::{PolicyClient, Scope, CHECK_PATH};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, ServiceId, ServiceTokenSigner,
};
use common::{
    app::{AppWorld, Device},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

const LAUNCH: &str = "/internal/v1/communications/calls";
const PARTICIPATION: &str = "/internal/v1/communications/calls/participation";
const HISTORY: &str = "/internal/v1/communications/calls/history";
const VERIFY: &str = "/internal/v1/communications/endpoints/verify";
const REVOKE: &str = "/internal/v1/communications/endpoints/revoke";

/// The Hub's policy endpoint: denies DIRECT_CALL to the scripted targets.
#[derive(Clone, Default)]
struct FakeHub {
    denied: Arc<Mutex<HashSet<Uuid>>>,
    checks: Arc<Mutex<usize>>,
}

impl FakeHub {
    fn deny(&self, target: Uuid) {
        self.denied.lock().unwrap().insert(target);
    }
    fn allow(&self, target: Uuid) {
        self.denied.lock().unwrap().remove(&target);
    }
    fn checks(&self) -> usize {
        *self.checks.lock().unwrap()
    }
}

async fn policy_check(State(hub): State<FakeHub>, Json(check): Json<Value>) -> Json<Value> {
    *hub.checks.lock().unwrap() += 1;
    let target: Uuid = check["targetPrincipalId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let allowed = !hub.denied.lock().unwrap().contains(&target);
    Json(json!({
        "schemaVersion": 1,
        "allowed": allowed,
        "reason": if allowed { "ALLOWED" } else { "NOT_PERMITTED" },
        "policyVersion": 1,
    }))
}

/// A world whose own scope is the governed personal scope, its Calling deployment
/// reaching the scripted Hub; `launch` configures the world's credential as the launch
/// authority.
async fn world(launch: bool) -> Option<(AppWorld, FakeHub)> {
    let hub = FakeHub::default();
    let app = Router::new()
        .route(CHECK_PATH, post(policy_check))
        .with_state(hub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (service_auth, peers) =
        common::service_auth_fixture_targeting(&[(ServiceId::communications(), &url)]);
    let mut w = AppWorld::with_service_auth(service_auth, peers, common::t0()).await?;
    let ctx = &w.w.ctx;
    let scope = Scope {
        tenant_id: ctx.tenant_id().as_uuid(),
        application_id: ctx.application_id().as_uuid(),
        environment_id: ctx.environment_id().as_uuid(),
    };
    let client = PolicyClient::new(w.w.h.state.service_auth.client_arc(), scope);
    let mut state = w.w.h.state.clone().with_communications(Some(client));
    if launch {
        state.communications_launch_credential = Some(w.w.token.credential.id.as_uuid());
    }
    w.w.h.state = state;
    Some((w, hub))
}

/// The world's governed scope, acting for `principal` (a NATIVE_CF_USER).
fn delegated(w: &AppWorld, principal: Uuid) -> DelegatedPlatformContext {
    delegated_as(w, principal, DelegatedPrincipalKind::NativeCfUser)
}

fn delegated_as(
    w: &AppWorld,
    principal: Uuid,
    kind: DelegatedPrincipalKind,
) -> DelegatedPlatformContext {
    let ctx = &w.w.ctx;
    DelegatedPlatformContext::new(
        ctx.tenant_id().as_uuid(),
        ctx.application_id().as_uuid(),
        ctx.environment_id().as_uuid(),
        ApplicationEnvironmentKind::Production,
        ctx.service_account_id().as_uuid(),
    )
    .with_initiating_principal(principal, kind)
}

async fn signed(
    w: &AppWorld,
    signer: &ServiceTokenSigner,
    scope: Option<DelegatedPlatformContext>,
    path: &str,
    body: Value,
) -> TestResponse {
    let mut request = OutboundServiceRequest::new_operation(ServiceId::calling());
    if let Some(scope) = scope {
        request = request.with_delegated_context(scope);
    }
    let signed = signer.sign(&request).unwrap();
    let headers: Vec<(String, String)> = signed
        .headers()
        .iter()
        .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    w.h()
        .request_with(Method::POST, path, None, Some(body), &refs)
        .await
}

/// The Hub acting for `principal`.
async fn hub_as(w: &AppWorld, principal: Uuid, path: &str, body: Value) -> TestResponse {
    let peers = w.h().peers.clone();
    signed(
        w,
        &peers.communications,
        Some(delegated(w, principal)),
        path,
        body,
    )
    .await
}

fn code(r: &TestResponse) -> &str {
    r.json["error"]["code"].as_str().unwrap_or_default()
}

fn launch_body(key: &str, initiator: Uuid, from: &Device, targets: &[Uuid]) -> Value {
    json!({
        "idempotencyKey": key,
        "initiatorPrincipalId": initiator,
        "initiatorEndpointId": from.endpoint,
        "targetPrincipalIds": targets,
        "kind": "VOICE",
    })
}

/// One well-formed body per route, for `principal` (the endpoint and call may be random:
/// the guards answer first).
fn bodies(principal: Uuid, endpoint: Uuid) -> Vec<(&'static str, Value)> {
    vec![
        (
            LAUNCH,
            json!({"idempotencyKey": "k-1", "initiatorPrincipalId": principal,
                   "initiatorEndpointId": endpoint, "targetPrincipalIds": [Uuid::new_v4()],
                   "kind": "VIDEO"}),
        ),
        (
            PARTICIPATION,
            json!({"principalId": principal, "callIds": [Uuid::new_v4()]}),
        ),
        (HISTORY, json!({"principalId": principal, "limit": 10})),
        (
            VERIFY,
            json!({"principalId": principal, "endpointId": endpoint}),
        ),
        (
            REVOKE,
            json!({"principalId": principal, "endpointId": endpoint}),
        ),
    ]
}

#[tokio::test]
async fn owner_routes_refuse_wrong_permission_missing_context_and_ungoverned_scope() {
    let Some((w, hub)) = world(true).await else {
        return;
    };
    let me = w.w.initiator.id().as_uuid();
    let device = w.device(&w.w.initiator, "me-main").await;
    let peers = w.h().peers.clone();
    let foreign = DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        ApplicationEnvironmentKind::Production,
        w.w.ctx.service_account_id().as_uuid(),
    )
    .with_initiating_principal(me, DelegatedPrincipalKind::NativeCfUser);
    for (path, body) in bodies(me, device.endpoint) {
        // A trusted service without the receiver-granted permission.
        let r = signed(
            &w,
            &peers.messaging,
            Some(delegated(&w, me)),
            path,
            body.clone(),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
        // The Hub without a delegated context.
        let r = signed(&w, &peers.communications, None, path, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
        // The Hub in a scope that is not the governed personal scope.
        let r = signed(&w, &peers.communications, Some(foreign.clone()), path, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.text);
    }
    // Nothing reached the policy, nothing was created or revoked.
    assert_eq!(hub.checks(), 0);
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
    let r = hub_as(
        &w,
        me,
        VERIFY,
        json!({"principalId": me, "endpointId": device.endpoint}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "ACTIVE");
}

#[tokio::test]
async fn owner_routes_refuse_a_principal_other_than_the_delegated_user() {
    let Some((w, _hub)) = world(true).await else {
        return;
    };
    let me = w.w.initiator.id().as_uuid();
    let other = w.w.recipient.id().as_uuid();
    let theirs = w.device(&w.w.recipient, "other-main").await;
    let peers = w.h().peers.clone();
    for (path, body) in bodies(other, theirs.endpoint) {
        // The Hub acts for `me` but names another principal.
        let r = hub_as(&w, me, path, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
        assert_eq!(code(&r), "INTERNAL_PERMISSION_DENIED", "{path}");
        // The right principal, but not delegated as a NATIVE_CF_USER.
        let r = signed(
            &w,
            &peers.communications,
            Some(delegated_as(
                &w,
                other,
                DelegatedPrincipalKind::ExternalApplicationPrincipal,
            )),
            path,
            body,
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
    }
    // Unknown fields are refused with Calling's validation envelope.
    let r = hub_as(
        &w,
        me,
        HISTORY,
        json!({"principalId": me, "limit": 10, "principal": other}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    assert_eq!(code(&r), "VALIDATION_ERROR");
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
}

#[tokio::test]
async fn launch_runs_the_canonical_creation_path_and_replays_idempotently() {
    let Some((w, hub)) = world(true).await else {
        return;
    };
    let (me, them) = (&w.w.initiator, &w.w.recipient);
    let mine = w.device(me, "me-main").await;
    let theirs = w.device(them, "them-main").await;
    let (me_id, them_id) = (me.id().as_uuid(), them.id().as_uuid());

    // Personal policy denial: the customer route's 409 TARGET_UNAVAILABLE, nothing rings.
    hub.deny(them_id);
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-1", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(code(&r), "TARGET_UNAVAILABLE");
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
    hub.allow(them_id);

    // The initiator endpoint must be an ACTIVE endpoint of the initiator.
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-2", me_id, &theirs, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // Self and duplicate targets are the domain's validation.
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-3", me_id, &mine, &[me_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-4", me_id, &mine, &[them_id, them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body(&"k".repeat(201), me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // Created and ringing, exactly like POST /v1/calls.
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-5", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["schemaVersion"], 1);
    assert_eq!(r.json["replayed"], false);
    assert_eq!(r.json["kind"], "VOICE");
    assert_eq!(r.json["state"], "RINGING", "{}", r.text);
    let call: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    assert!(w.invitation_for(call, theirs.endpoint).await.is_some());

    // A replay answers the same call and creates nothing.
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-5", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["replayed"], true);
    assert_eq!(r.json["callId"], call.to_string());
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 1);
    // The same key for another request is a conflict, never another user's call.
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        json!({"idempotencyKey": "hub-5", "initiatorPrincipalId": me_id,
               "initiatorEndpointId": mine.endpoint, "targetPrincipalIds": [them_id],
               "kind": "VIDEO"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(code(&r), "IDEMPOTENCY_CONFLICT");

    // The policy check runs before replay: a later denial refuses the replay too.
    hub.deny(them_id);
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-5", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(code(&r), "TARGET_UNAVAILABLE");
    hub.allow(them_id);

    // Hub keys live in their own namespace: the same raw key through the customer route
    // is a different command, never a replay of the Hub launch.
    let end = w.command(call, "cancel", w.state(call).await.1).await;
    assert!(end.status.is_success(), "{}", end.text);
    let r = w.create_call("VOICE", me, &mine, &[them], "hub-5").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_ne!(r.json["call"]["callId"], call.to_string());
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 2);
}

#[tokio::test]
async fn launch_fails_closed_without_a_launch_authority() {
    let Some((w, hub)) = world(false).await else {
        return;
    };
    let (me, them) = (&w.w.initiator, &w.w.recipient);
    let mine = w.device(me, "me-main").await;
    w.device(them, "them-main").await;
    let (me_id, them_id) = (me.id().as_uuid(), them.id().as_uuid());
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-1", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(hub.checks(), 0);
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
}

#[tokio::test]
async fn launch_fails_closed_once_the_launch_credential_is_revoked() {
    let Some((w, hub)) = world(true).await else {
        return;
    };
    let (me, them) = (&w.w.initiator, &w.w.recipient);
    let mine = w.device(me, "me-main").await;
    w.device(them, "them-main").await;
    let (me_id, them_id) = (me.id().as_uuid(), them.id().as_uuid());
    // Revocation in Platform Access applies to the very next launch: the credential is
    // re-verified live, never cached as a standing grant.
    w.h()
        .platform
        .revoke_credential(w.w.token.credential.id)
        .await
        .unwrap();
    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-revoked", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(hub.checks(), 0);
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
}

#[tokio::test]
async fn participation_and_history_return_only_the_delegated_users_calls() {
    let Some((w, _hub)) = world(true).await else {
        return;
    };
    let (me, them) = (&w.w.initiator, &w.w.recipient);
    let third = w.principal("third-1").await;
    let mine = w.device(me, "me-main").await;
    let theirs = w.device(them, "them-main").await;
    w.device(&third, "third-main").await;
    let (me_id, them_id) = (me.id().as_uuid(), them.id().as_uuid());

    let r = hub_as(
        &w,
        me_id,
        LAUNCH,
        launch_body("hub-1", me_id, &mine, &[them_id]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let ours: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    // A call between two other principals of the same scope.
    let r = w
        .create_call(
            "VIDEO",
            them,
            &theirs,
            &[&third],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let foreign: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // Both calls were created at the same (manual) instant: pages follow (createdAt, id).

    // CL-2: the non-participant call (and an unknown id) are silently omitted.
    let r = hub_as(
        &w,
        me_id,
        PARTICIPATION,
        json!({"principalId": me_id, "callIds": [ours, foreign, Uuid::new_v4()]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["schemaVersion"], 1);
    let calls = r.json["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "{}", r.text);
    let entry = &calls[0];
    assert_eq!(entry["callId"], ours.to_string());
    assert_eq!(entry["kind"], "VOICE");
    assert_eq!(entry["direction"], "OUTGOING");
    assert_eq!(entry["terminal"], false);
    assert_eq!(entry["missed"], false);
    assert_eq!(entry["counterpartPrincipalIds"], json!([them_id]));
    assert!(entry.get("endedAt").is_none() && entry.get("durationMs").is_none());
    // The recipient sees it as an incoming, ringing call.
    let r = hub_as(
        &w,
        them_id,
        PARTICIPATION,
        json!({"principalId": them_id, "callIds": [ours, foreign]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let calls = r.json["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "{}", r.text);
    let incoming = calls
        .iter()
        .find(|c| c["callId"] == ours.to_string())
        .unwrap();
    assert_eq!(incoming["direction"], "INCOMING");
    assert_eq!(incoming["outcome"], "RINGING");
    // Bounds: 1-50 distinct ids.
    for ids in [
        json!([]),
        json!([ours, ours]),
        json!(vec![Uuid::new_v4(); 51]),
    ] {
        let r = hub_as(
            &w,
            me_id,
            PARTICIPATION,
            json!({"principalId": me_id, "callIds": ids}),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }

    // CL-3: the recipient's history pages through both calls with the keyset cursor.
    let r = hub_as(
        &w,
        them_id,
        HISTORY,
        json!({"principalId": them_id, "limit": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["data"].as_array().unwrap().len(), 1);
    let first = r.json["data"][0]["callId"].clone();
    let cursor = r.json["nextCursor"].as_str().unwrap().to_string();
    let r = hub_as(
        &w,
        them_id,
        HISTORY,
        json!({"principalId": them_id, "limit": 1, "cursor": cursor}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["data"].as_array().unwrap().len(), 1);
    let second = r.json["data"][0]["callId"].clone();
    let mut paged = [first, second];
    paged.sort_by_key(|v| v.as_str().unwrap().to_string());
    let mut both = [json!(ours.to_string()), json!(foreign.to_string())];
    both.sort_by_key(|v| v.as_str().unwrap().to_string());
    assert_eq!(paged, both);
    let r = hub_as(
        &w,
        them_id,
        HISTORY,
        json!({"principalId": them_id, "limit": 2}),
    )
    .await;
    assert!(r.json["nextCursor"].is_null(), "{}", r.text);
    // My history holds only my call; nothing is missed yet.
    let r = hub_as(
        &w,
        me_id,
        HISTORY,
        json!({"principalId": me_id, "limit": 100}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let data = r.json["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["callId"], ours.to_string());
    let r = hub_as(
        &w,
        me_id,
        HISTORY,
        json!({"principalId": me_id, "limit": 100, "missed": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["data"].as_array().unwrap().is_empty());
    for body in [
        json!({"principalId": me_id, "limit": 0}),
        json!({"principalId": me_id, "limit": 101}),
        json!({"principalId": me_id, "limit": 10, "cursor": "not-a-cursor"}),
    ] {
        let r = hub_as(&w, me_id, HISTORY, body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }
}

#[tokio::test]
async fn endpoint_verify_and_revoke_see_only_the_delegated_users_endpoints() {
    let Some((w, _hub)) = world(true).await else {
        return;
    };
    let (me, them) = (&w.w.initiator, &w.w.recipient);
    let mine = w.device(me, "me-main").await;
    let theirs = w.device(them, "them-main").await;
    let (me_id, them_id) = (me.id().as_uuid(), them.id().as_uuid());

    let r = hub_as(
        &w,
        me_id,
        VERIFY,
        json!({"principalId": me_id, "endpointId": mine.endpoint}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json,
        json!({"schemaVersion": 1, "owned": true, "status": "ACTIVE", "endpointKind": "MOBILE"})
    );
    // Another principal's endpoint and an unknown one look the same: not owned.
    for endpoint in [theirs.endpoint, Uuid::new_v4()] {
        let r = hub_as(
            &w,
            me_id,
            VERIFY,
            json!({"principalId": me_id, "endpointId": endpoint}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(
            r.json,
            json!({"schemaVersion": 1, "owned": false, "status": null, "endpointKind": null})
        );
        // Revoking it is 404 and changes nothing.
        let r = hub_as(
            &w,
            me_id,
            REVOKE,
            json!({"principalId": me_id, "endpointId": endpoint}),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    }
    let r = hub_as(
        &w,
        them_id,
        VERIFY,
        json!({"principalId": them_id, "endpointId": theirs.endpoint}),
    )
    .await;
    assert_eq!(r.json["status"], "ACTIVE", "{}", r.text);

    // My endpoint: the canonical revoke ends its live ring offers.
    let r = w
        .create_call("VOICE", them, &theirs, &[me], &Uuid::new_v4().to_string())
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(w.invitation_for(call, mine.endpoint).await.is_some());
    let r = hub_as(
        &w,
        me_id,
        REVOKE,
        json!({"principalId": me_id, "endpointId": mine.endpoint}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json,
        json!({"schemaVersion": 1, "revoked": true, "alreadyRevoked": false})
    );
    assert!(w.invitation_for(call, mine.endpoint).await.is_none());
    let r = hub_as(
        &w,
        me_id,
        REVOKE,
        json!({"principalId": me_id, "endpointId": mine.endpoint}),
    )
    .await;
    assert_eq!(
        r.json,
        json!({"schemaVersion": 1, "revoked": true, "alreadyRevoked": true})
    );
    let r = hub_as(
        &w,
        me_id,
        VERIFY,
        json!({"principalId": me_id, "endpointId": mine.endpoint}),
    )
    .await;
    assert_eq!(r.json["status"], "REVOKED", "{}", r.text);
}

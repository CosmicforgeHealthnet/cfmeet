//! ADR 0062 (CF Meet Platform "Build") — partner users join meetings with their CF Meet
//! user token, and partner meeting chat.
//!
//! The partner's server creates the meeting exactly as the API Gateway forwards it (a
//! machine caller of the environment, `common::platform`). Devices then join with
//! `Authorization: Bearer cfut_…`; a scripted Platform Access answers the live
//! `/internal/v1/user-tokens/verify` (through the production answer mapping), and a
//! scripted Platform Access / Messaging pair answers partner meeting chat. Proves: the
//! environment scope (other environments' and user meetings are 404), the grant (403
//! without one), roles from the grant, the profile display name, one participant per
//! (conference, principal) across re-joins, identity bans, refusals of suspended users and
//! revoked tokens (401) and an unreachable Platform Access (503), the `principal_id` of
//! participant envelopes, partner chat sync, and that with the flag off a user token is
//! refused exactly like any unknown bearer (guests and accounts unchanged).

mod common;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use backend::services::{
    platform::{
        authority::AuthorityUnavailable,
        events,
        partner_chat::{
            PartnerChat, PartnerChatGateway, PartnerScope, CONVERSATIONS_PATH,
            ENSURE_PRINCIPAL_PATH, SYSTEM_PRINCIPAL_REFERENCE,
        },
        user_token::{verification_of, UserTokenVerification, UserTokenVerifier},
        PlatformUsers,
    },
    unified_messaging::{GatewayError, Peer},
};
use common::{platform::*, *};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

const RW: &[&str] = &["meetings.read", "meetings.write"];

/// Scripted Platform Access `user-tokens/verify`: token -> (HTTP status, body).
#[derive(Default)]
struct FakeTokens {
    answers: Mutex<HashMap<String, (u16, Value)>>,
    unavailable: AtomicBool,
    calls: AtomicUsize,
}

#[async_trait]
impl UserTokenVerifier for FakeTokens {
    async fn verify(&self, token: &str) -> Result<UserTokenVerification, AuthorityUnavailable> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(AuthorityUnavailable("connect".into()));
        }
        let answer = self.answers.lock().unwrap().get(token).cloned();
        match answer {
            Some((status, body)) => verification_of(status, &body),
            None => verification_of(401, &json!({ "error": { "code": "USER_TOKEN_INVALID" } })),
        }
    }
}

/// A partner user of a machine's environment.
#[derive(Clone)]
struct PartnerUser {
    principal: Uuid,
    name: Option<String>,
}

impl PartnerUser {
    fn new(name: Option<&str>) -> Self {
        Self {
            principal: Uuid::new_v4(),
            name: name.map(str::to_string),
        }
    }
}

impl FakeTokens {
    /// Mints a user token of `m`'s environment for `user` with `mtg` grants; returns the
    /// token and its id.
    fn mint(&self, m: &Machine, user: &PartnerUser, mtg: Value) -> (String, Uuid) {
        self.mint_with(m, user, mtg, "ACTIVE")
    }

    fn mint_with(
        &self,
        m: &Machine,
        user: &PartnerUser,
        mtg: Value,
        status: &str,
    ) -> (String, Uuid) {
        let token_id = Uuid::new_v4();
        let token = format!("cfut_sbx_eyJhbGciOiJFZERTQSJ9.{}.sig", token_id.simple());
        let a = &m.actor;
        let body = json!({
            "tokenId": token_id, "tenantId": a.tenant_id, "applicationId": a.application_id,
            "environmentId": a.environment_id, "environmentKind": "SANDBOX",
            "credentialId": a.credential_id, "serviceAccountId": a.service_account_id,
            "user": { "id": user.principal, "externalId": "rider-4711",
                      "displayName": user.name, "imageUrl": null, "status": status },
            "grants": { "msg": ["read", "send"], "call": [], "mtg": mtg },
            "issuedAt": "2026-11-18T10:00:00Z", "expiresAt": "2099-01-01T00:00:00Z",
        });
        self.answers
            .lock()
            .unwrap()
            .insert(token.clone(), (200, body));
        (token, token_id)
    }

    fn revoke(&self, token: &str) {
        self.answers.lock().unwrap().insert(
            token.to_string(),
            (401, json!({ "error": { "code": "USER_TOKEN_REVOKED" } })),
        );
    }
}

fn grant(conference: Uuid, role: &str) -> Value {
    json!([{ "conferenceId": conference, "role": role }])
}

/// Scripted Platform Access (ensure-meeting-platform) and Messaging (meeting
/// conversations) in the partner scope.
struct FakeChat {
    calls: Mutex<Vec<(Peer, PartnerScope, Method, String, Value)>>,
    conversation: Uuid,
    system_principal: Uuid,
    fail: AtomicBool,
}

impl Default for FakeChat {
    fn default() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            conversation: Uuid::new_v4(),
            system_principal: Uuid::new_v4(),
            fail: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl PartnerChatGateway for FakeChat {
    async fn call(
        &self,
        peer: Peer,
        scope: &PartnerScope,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GatewayError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(GatewayError::Unavailable("connect"));
        }
        self.calls.lock().unwrap().push((
            peer,
            *scope,
            method.clone(),
            path.to_string(),
            body.cloned().unwrap_or(Value::Null),
        ));
        Ok(match (peer, path) {
            (Peer::PlatformAccess, ENSURE_PRINCIPAL_PATH) => json!({
                "principalId": self.system_principal, "tenantId": scope.tenant_id,
                "applicationId": scope.application_id, "environmentId": scope.environment_id,
                "kind": "BOT_OR_SERVICE",
            }),
            (Peer::Messaging, CONVERSATIONS_PATH) => json!({
                "created": true, "conversation": { "conversationId": self.conversation },
            }),
            _ => json!({ "applied": true }),
        })
    }
}

impl FakeChat {
    fn member_updates(&self, principal: Uuid) -> Vec<Value> {
        let suffix = format!("/members/{principal}");
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.3.ends_with(&suffix))
            .map(|c| c.4.clone())
            .collect()
    }
}

/// Enables partner-user joins (and optionally partner chat) on the harness app.
fn enable(h: &mut Harness, tokens: Arc<FakeTokens>, chat: Option<Arc<FakeChat>>) {
    let chat = chat.map(|g| Arc::new(PartnerChat::new(h.app.db.conn.clone(), g)));
    h.app.state = h.app.state.clone().with_platform_users(PlatformUsers {
        verifier: tokens,
        chat,
    });
    h.app.router = backend::routes::create_routes(h.app.state.clone())
        .route_layer(axum::middleware::from_fn(common::route_coverage::record));
}

macro_rules! harness {
    () => {
        match Harness::new().await {
            Some(h) => h,
            None => return,
        }
    };
}

/// The partner's server creates a LIVE instant meeting through the gateway.
async fn partner_meeting(h: &Harness, m: &Machine, body: Value) -> (Uuid, Uuid) {
    let (s, v) = h
        .machine(m, Method::POST, "/api/v2/instant-meetings", Some(body))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    (uuid(&v["meetingSpace"]["id"]), uuid(&v["conference"]["id"]))
}

async fn join_with(h: &Harness, conference: Uuid, token: &str, body: Value) -> (StatusCode, Value) {
    h.app.join(conference, Auth::Bearer(token), body).await
}

#[tokio::test]
async fn a_partner_user_joins_under_its_identity_and_a_rejoin_keeps_the_participant() {
    let mut h = harness!();
    let tokens = Arc::new(FakeTokens::default());
    enable(&mut h, tokens.clone(), None);
    let m = Machine::new(RW);
    h.register(&m);
    // INVITE_ONLY: the verified grant is the authorization (no invitation, no passcode).
    let (_, conference) = partner_meeting(
        &h,
        &m,
        json!({ "title": "Delivery handover", "accessPolicy": "INVITE_ONLY" }),
    )
    .await;
    let rider = PartnerUser::new(Some("Rider Ruth"));
    let (token, token_id) = tokens.mint(&m, &rider, grant(conference, "PARTICIPANT"));

    // The body's displayName is ignored: the profile name is authoritative.
    let (s, v) = join_with(&h, conference, &token, json!({ "displayName": "Mallory" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let participant = uuid(&v["participant"]["id"]);
    assert_eq!(v["participant"]["displayName"], "Rider Ruth", "{v}");
    assert_eq!(v["participant"]["role"], "PARTICIPANT", "{v}");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED", "{v}");
    assert_eq!(v["rtcStatus"], "ISSUED", "{v}");
    assert!(v["rtc"]["credential"].is_string(), "{v}");
    let first_pt = v["participantToken"].as_str().unwrap().to_string();
    assert!(first_pt.starts_with("pt_"), "{v}");
    assert!(
        !v.to_string().contains("rider-4711"),
        "never the externalId: {v}"
    );
    assert!(!v.to_string().contains(&token), "never the user token: {v}");
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM conference_participants WHERE id = '{participant}' \
                 AND identity_type = 'INVITED_EXTERNAL' AND user_id IS NULL \
                 AND external_ref = 'principal:{}'",
                rider.principal
            ))
            .await,
        1
    );
    let a = &m.actor;
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_platform_participants WHERE conference_id = '{conference}' \
                 AND principal_id = '{}' AND participant_id = '{participant}' AND token_id = '{token_id}' \
                 AND credential_id = '{}' AND service_account_id = '{}' AND environment_id = '{}' \
                 AND environment_kind = 'SANDBOX'",
                rider.principal, a.credential_id, a.service_account_id, a.environment_id
            ))
            .await,
        1
    );
    // The participant token is what the device uses next.
    let (s, me) = h
        .app
        .get(
            format!("/api/v2/conferences/{conference}/participants/me"),
            Auth::Participant(&first_pt),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{me}");
    // Only the join accepts a user token.
    let (s, v) = h
        .app
        .get(
            format!("/api/v2/conferences/{conference}/participants/me"),
            Auth::Bearer(&token),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "AUTHENTICATION_REQUIRED");

    // A re-join (another device, a fresh user token) keeps the same participant and
    // rotates the participant token.
    let (token2, token2_id) = tokens.mint(&m, &rider, grant(conference, "PARTICIPANT"));
    let (s, v) = join_with(&h, conference, &token2, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(uuid(&v["participant"]["id"]), participant);
    let second_pt = v["participantToken"].as_str().unwrap().to_string();
    assert_ne!(second_pt, first_pt);
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM conference_participants WHERE conference_id = '{conference}'"
            ))
            .await,
        1,
        "one logical participant per (conference, principal)"
    );
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_platform_participants WHERE conference_id = '{conference}' \
                 AND token_id = '{token2_id}'"
            ))
            .await,
        1
    );
    let (s, _) = h
        .app
        .get(
            format!("/api/v2/conferences/{conference}/participants/me"),
            Auth::Participant(&second_pt),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .app
        .get(
            format!("/api/v2/conferences/{conference}/participants/me"),
            Auth::Participant(&first_pt),
        )
        .await;
    assert_ne!(
        s,
        StatusCode::OK,
        "the previous participant token was replaced"
    );

    // A profile without a display name joins under a generic name.
    let anonymous = PartnerUser::new(None);
    let (token, _) = tokens.mint(&m, &anonymous, grant(conference, "VIEWER"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["displayName"], "Participant", "{v}");
    assert_eq!(v["participant"]["role"], "VIEWER", "{v}");
}

#[tokio::test]
async fn the_environment_scope_and_the_grant_decide_admission() {
    let mut h = harness!();
    let tokens = Arc::new(FakeTokens::default());
    enable(&mut h, tokens.clone(), None);
    let m = Machine::new(RW);
    h.register(&m);
    let (_, conference) = partner_meeting(&h, &m, json!({ "title": "Ops" })).await;
    let (_, other_conference) = partner_meeting(&h, &m, json!({ "title": "Other" })).await;

    // Another environment of the same application: the conference does not exist for it.
    let sibling = m.sibling_environment();
    let user = PartnerUser::new(Some("Sam"));
    let (foreign, _) = tokens.mint(&sibling, &user, grant(conference, "HOST"));
    let (s, v) = join_with(&h, conference, &foreign, json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "CONFERENCE_NOT_FOUND");

    // A CF Meet user's meeting does not exist for a partner user either.
    let owner = h.app.user("owner").await;
    let (_, user_conference) = h
        .app
        .live_conference(&owner, json!({ "title": "Mine", "accessPolicy": "PUBLIC" }))
        .await;
    let (token, _) = tokens.mint(&m, &user, grant(user_conference, "HOST"));
    let (s, v) = join_with(&h, user_conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    let (s, v) = join_with(&h, Uuid::new_v4(), &token, json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");

    // Same environment, but the token grants another conference: 403.
    let (token, _) = tokens.mint(&m, &user, grant(other_conference, "HOST"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "MEETING_ACCESS_DENIED");
    // No meeting grant at all, or only a non-grantable role (OWNER is never granted).
    let (token, _) = tokens.mint(&m, &user, json!([]));
    let (s, _) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (token, _) = tokens.mint(&m, &user, grant(conference, "OWNER"));
    let (s, _) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        h.app
            .scalar("SELECT count(*) FROM meeting_platform_participants")
            .await,
        0,
        "nothing was admitted"
    );

    // Roles come from the grant: a HOST moderates, a PRESENTER does not.
    let host = PartnerUser::new(Some("Dispatcher"));
    let (token, _) = tokens.mint(&m, &host, grant(conference, "HOST"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "HOST", "{v}");
    let host_pt = v["participantToken"].as_str().unwrap().to_string();
    let presenter = PartnerUser::new(Some("Driver"));
    let (token, _) = tokens.mint(&m, &presenter, grant(conference, "PRESENTER"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "PRESENTER", "{v}");
    let presenter_pt = v["participantToken"].as_str().unwrap().to_string();
    let rider = PartnerUser::new(Some("Rider"));
    let (rider_token, _) = tokens.mint(&m, &rider, grant(conference, "CO_HOST"));
    let (s, v) = join_with(&h, conference, &rider_token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "CO_HOST", "{v}");
    let rider_participant = uuid(&v["participant"]["id"]);
    let ban = format!("/api/v2/conferences/{conference}/participants/{rider_participant}/ban");
    let (s, _) = h
        .app
        .post(&ban, Auth::Participant(&presenter_pt), json!({}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a presenter cannot ban");

    // The host bans the rider: the ban is on the partner identity, so a fresh token on
    // another device is refused too.
    let (s, v) = h
        .app
        .post(
            &ban,
            Auth::Participant(&host_pt),
            json!({ "scope": "SPACE" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM conference_bans WHERE subject_type = 'EXTERNAL' \
                 AND subject_ref = 'principal:{}'",
                rider.principal
            ))
            .await,
        1
    );
    let (fresh, _) = tokens.mint(&m, &rider, grant(conference, "CO_HOST"));
    let (s, v) = join_with(&h, conference, &fresh, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "PARTICIPANT_BANNED");
}

#[tokio::test]
async fn suspended_users_revoked_tokens_and_an_unreachable_platform_access_are_refused() {
    let mut h = harness!();
    let tokens = Arc::new(FakeTokens::default());
    enable(&mut h, tokens.clone(), None);
    let m = Machine::new(RW);
    h.register(&m);
    let (_, conference) = partner_meeting(&h, &m, json!({ "title": "Ops" })).await;
    let user = PartnerUser::new(Some("Sam"));

    let (token, _) = tokens.mint(&m, &user, grant(conference, "PARTICIPANT"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // revoke-sessions / a revoked minting key: Platform Access now refuses the token.
    tokens.revoke(&token);
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "AUTHENTICATION_REQUIRED");
    // A user Platform Access reports as not ACTIVE.
    let (suspended, _) = tokens.mint_with(&m, &user, grant(conference, "PARTICIPANT"), "SUSPENDED");
    let (s, _) = join_with(&h, conference, &suspended, json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Unknown token, malformed token, a token next to a participant credential.
    let (s, _) = join_with(&h, conference, "cfut_sbx_unknown.e30.sig", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = join_with(&h, conference, "cfut_staging_x.e30.sig", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (fresh, _) = tokens.mint(&m, &user, grant(conference, "PARTICIPANT"));
    let (s, _) = h
        .app
        .call_with_headers(
            Method::POST,
            &format!("/api/v2/conferences/{conference}/join"),
            &[
                ("authorization", &format!("Bearer {fresh}")),
                ("x-participant-token", "pt_whatever"),
            ],
            "{}".into(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Platform Access unreachable: fail closed.
    tokens.unavailable.store(true, Ordering::SeqCst);
    let (s, v) = join_with(&h, conference, &fresh, json!({})).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "SERVICE_UNAVAILABLE");
    tokens.unavailable.store(false, Ordering::SeqCst);
    let (s, _) = join_with(&h, conference, &fresh, json!({})).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn with_the_flag_off_a_user_token_is_refused_like_any_unknown_bearer() {
    let h = harness!();
    let m = Machine::new(RW);
    h.register(&m);
    let (_, conference) = partner_meeting(&h, &m, json!({ "title": "Ops" })).await;
    let (s, v) = join_with(
        &h,
        conference,
        "cfut_sbx_eyJhbGciOiJFZERTQSJ9.e30.sig",
        json!({}),
    )
    .await;
    let (s2, v2) = join_with(&h, conference, "not-a-jwt", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(
        (s, &v["error"]["code"], &v["error"]["message"]),
        (s2, &v2["error"]["code"], &v2["error"]["message"])
    );
    // Guests and CF Meet accounts are unchanged.
    let (s, g) = h
        .app
        .join(
            conference,
            Auth::None,
            json!({ "displayName": "Guest Ada" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    let owner = h.app.user("owner").await;
    let (_, mine) = h
        .app
        .live_conference(&owner, json!({ "title": "Mine", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = h
        .app
        .join(mine, Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "OWNER");
}

#[tokio::test]
async fn participant_envelopes_of_partner_users_carry_the_principal() {
    let mut h = harness!();
    events::set_staging(true);
    let tokens = Arc::new(FakeTokens::default());
    enable(&mut h, tokens.clone(), None);
    let m = Machine::new(RW);
    h.register(&m);
    let (_, conference) =
        partner_meeting(&h, &m, json!({ "title": "Ops", "accessPolicy": "PUBLIC" })).await;
    let user = PartnerUser::new(Some("Sam"));
    let (token, _) = tokens.mint(&m, &user, grant(conference, "PARTICIPANT"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pt = v["participantToken"].as_str().unwrap().to_string();
    let (s, g) = h
        .app
        .join(conference, Auth::None, json!({ "displayName": "Guest" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    let guest_pt = g["participantToken"].as_str().unwrap().to_string();
    for credential in [&pt, &guest_pt] {
        let (s, v) = h
            .app
            .post(
                format!("/api/v2/conferences/{conference}/leave"),
                Auth::Participant(credential),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let left = |extra: &str| {
        format!(
            "SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT' AND conference_id = '{conference}' \
             AND event_type = 'meeting-service.participant.left' {extra}"
        )
    };
    assert_eq!(h.app.scalar(&left("")).await, 2);
    assert_eq!(
        h.app
            .scalar(&left(&format!(
                "AND payload->'payload'->>'principal_id' = '{}'",
                user.principal
            )))
            .await,
        1,
        "the partner user's envelope names its principal"
    );
    assert_eq!(
        h.app
            .scalar(&left("AND payload->'payload' ? 'principal_id'"))
            .await,
        1,
        "a guest's envelope has no principal"
    );
}

#[tokio::test]
async fn partner_meeting_chat_follows_admission_in_the_partner_scope() {
    let mut h = harness!();
    let tokens = Arc::new(FakeTokens::default());
    let gateway = Arc::new(FakeChat::default());
    enable(&mut h, tokens.clone(), Some(gateway.clone()));
    let chat = h
        .app
        .state
        .platform_users
        .as_ref()
        .and_then(|u| u.chat.clone())
        .unwrap();
    let m = Machine::new(RW);
    h.register(&m);
    let (_, conference) = partner_meeting(&h, &m, json!({ "title": "Ops" })).await;
    let host = PartnerUser::new(Some("Dispatcher"));
    let rider = PartnerUser::new(Some("Rider"));
    let (token, _) = tokens.mint(&m, &host, grant(conference, "HOST"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let host_pt = v["participantToken"].as_str().unwrap().to_string();
    let (token, _) = tokens.mint(&m, &rider, grant(conference, "PARTICIPANT"));
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let rider_participant = uuid(&v["participant"]["id"]);

    // The system principal and the MANAGED conversation, once, in the partner's scope.
    let a = &m.actor;
    {
        let calls = gateway.calls.lock().unwrap();
        for c in calls.iter() {
            assert_eq!(
                (
                    c.1.tenant_id,
                    c.1.application_id,
                    c.1.environment_id,
                    c.1.service_account_id
                ),
                (
                    a.tenant_id,
                    a.application_id,
                    a.environment_id,
                    a.service_account_id
                ),
                "{c:?}"
            );
        }
        let ensures: Vec<_> = calls
            .iter()
            .filter(|c| c.3 == ENSURE_PRINCIPAL_PATH)
            .collect();
        assert_eq!(ensures.len(), 1);
        assert_eq!(ensures[0].0, Peer::PlatformAccess);
        assert_eq!(
            ensures[0].4,
            json!({ "environmentId": a.environment_id, "kind": "BOT_OR_SERVICE",
                    "externalReference": SYSTEM_PRINCIPAL_REFERENCE })
        );
        let resolves: Vec<_> = calls.iter().filter(|c| c.3 == CONVERSATIONS_PATH).collect();
        assert_eq!(resolves.len(), 1);
        assert_eq!(resolves[0].4["securityProfile"], "MANAGED");
        assert_eq!(
            resolves[0].4["ownerPrincipalId"],
            json!(gateway.system_principal)
        );
        assert_eq!(
            resolves[0].4["meetingRef"],
            format!("conference:{conference}")
        );
    }
    assert_eq!(gateway.member_updates(host.principal)[0]["role"], "ADMIN");
    let rider_updates = gateway.member_updates(rider.principal);
    assert_eq!(rider_updates.len(), 1);
    assert_eq!(rider_updates[0]["state"], "ACTIVE");
    assert_eq!(rider_updates[0]["role"], "MEMBER");
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_platform_chat_links WHERE conference_id = '{conference}' \
                 AND conversation_id = '{}' AND owner_principal_id = '{}'",
                gateway.conversation, gateway.system_principal
            ))
            .await,
        1
    );

    // A ban reaches the conversation on the next pass; the pass is idempotent.
    let (s, v) = h
        .app
        .post(
            format!("/api/v2/conferences/{conference}/participants/{rider_participant}/ban"),
            Auth::Participant(&host_pt),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let report = chat.sync_once().await;
    assert_eq!(
        (report.members_synced, report.failures),
        (1, 0),
        "{report:?}"
    );
    assert_eq!(
        gateway.member_updates(rider.principal).last().unwrap()["state"],
        "BANNED"
    );
    assert_eq!(chat.sync_once().await.members_synced, 0);

    // Ending the meeting closes the conversation (once).
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/conferences/{conference}/end"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Provider termination completes the end (as the RTC webhook / reconciler would).
    h.app
        .db
        .conn
        .execute_unprepared(&format!(
            "UPDATE conference_sessions SET status = 'ENDED', ended_at = now(), version = version + 1 \
             WHERE id = '{conference}' AND status <> 'ENDED'"
        ))
        .await
        .unwrap();
    assert_eq!(chat.sync_once().await.lifecycles_closed, 1);
    let lifecycle: Vec<_> = gateway
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.3.ends_with("/lifecycle"))
        .map(|c| c.4.clone())
        .collect();
    assert_eq!(lifecycle.len(), 1);
    assert_eq!(lifecycle[0]["lifecycleState"], "CLOSED");
    assert_eq!(lifecycle[0]["reason"], "MEETING_ENDED");
    assert_eq!(chat.sync_once().await.lifecycles_closed, 0);
}

#[tokio::test]
async fn chat_unavailable_never_fails_a_join_and_the_pass_catches_up() {
    let mut h = harness!();
    let tokens = Arc::new(FakeTokens::default());
    let gateway = Arc::new(FakeChat::default());
    enable(&mut h, tokens.clone(), Some(gateway.clone()));
    let chat = h
        .app
        .state
        .platform_users
        .as_ref()
        .and_then(|u| u.chat.clone())
        .unwrap();
    let m = Machine::new(RW);
    h.register(&m);
    let (_, conference) = partner_meeting(&h, &m, json!({ "title": "Ops" })).await;
    let user = PartnerUser::new(Some("Sam"));
    let (token, _) = tokens.mint(&m, &user, grant(conference, "PARTICIPANT"));
    gateway.fail.store(true, Ordering::SeqCst);
    let (s, v) = join_with(&h, conference, &token, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(chat.sync_once().await.failures > 0);
    gateway.fail.store(false, Ordering::SeqCst);
    let report = chat.sync_once().await;
    assert_eq!(
        (
            report.conversations_linked,
            report.members_synced,
            report.failures
        ),
        (1, 1, 0),
        "{report:?}"
    );
    assert_eq!(gateway.member_updates(user.principal)[0]["state"], "ACTIVE");
}

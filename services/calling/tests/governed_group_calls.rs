//! C7 in a governed personal scope (found by the production verification journey,
//! `infra/calling/production/issue_acceptance.py`, run 43919948570).
//!
//! Once a third person is added to a personal call, every check that carries device
//! evidence (accepting over signaling, a media credential, media admission) re-checks
//! the Communications policy for each initiator → party pair. The Communications policy
//! denies a check whose credential belongs to neither the actor nor the target of that
//! pair (`services/communications/src/policy.rs`). Applying one device's evidence to
//! every pair therefore denied the added person's accept (409 TARGET_UNAVAILABLE in
//! production) and any other non-initiator's media credential. The evidence now applies
//! to the pairs its owner is part of; every pair is still checked, and evidence of
//! someone who is not a party is refused.

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{Method, StatusCode},
    routing::post,
    Json, Router,
};
use cf_communications_policy::{PolicyClient, Scope, CHECK_PATH};
use cf_service_auth::ServiceId;
use common::app::AppWorld;
use serde_json::{json, Value};
use uuid::Uuid;

/// The Hub's policy endpoint with the real credential-owner rule: a check whose
/// credential belongs to neither the actor nor the target is denied. It records every
/// (actor, target, credential owner) it decided.
/// One decided check: (actor, target, credential owner).
type Decided = (Uuid, Uuid, Option<Uuid>);

#[derive(Clone, Default)]
struct PolicyHub {
    checks: Arc<Mutex<Vec<Decided>>>,
    /// Principals whose device credentials are revoked (`credentials_not_before`).
    revoked: Arc<Mutex<Vec<Uuid>>>,
}

fn id(v: &Value) -> Option<Uuid> {
    v.as_str().map(|s| s.parse().unwrap())
}

async fn policy_check(State(hub): State<PolicyHub>, Json(check): Json<Value>) -> Json<Value> {
    let actor = id(&check["actorPrincipalId"]).unwrap();
    let target = id(&check["targetPrincipalId"]).unwrap();
    let owner = id(&check["credentialPrincipalId"]);
    hub.checks.lock().unwrap().push((actor, target, owner));
    let revoked = owner.is_some_and(|o| hub.revoked.lock().unwrap().contains(&o));
    let allowed = owner.is_none_or(|o| o == actor || o == target) && !revoked;
    Json(json!({
        "schemaVersion": 1,
        "allowed": allowed,
        "reason": if allowed { "ALLOWED" } else { "NOT_PERMITTED" },
        "policyVersion": 1,
    }))
}

/// A world whose own scope is the governed personal scope, its Calling deployment
/// reaching the scripted Hub.
async fn governed() -> Option<(AppWorld, PolicyHub)> {
    let hub = PolicyHub::default();
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
    w.w.h.state = w.w.h.state.clone().with_communications(Some(client));
    // Production runs Calling's independently versioned Communications migration too
    // (credential revocations read by every signaling request in a governed scope).
    {
        use calling_migration::MigratorTrait;
        calling_migration::CommunicationsMigrator::up(w.h().conn(), None)
            .await
            .unwrap();
    }
    Some((w, hub))
}

#[tokio::test]
async fn an_added_person_accepts_over_signaling_and_every_party_keeps_media_access() {
    let Some((a, hub)) = governed().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let carol = a.principal("governed-carol").await;
    let carol_d = a.device(&carol, "governed-carol").await;
    let alice = a.participant_of(call, caller.principal).await;
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/participants"),
            json!({ "invitingParticipantId": alice, "targetPrincipalIds": [carol.id().as_uuid()],
                    "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let invitation = a.invitation_for(call, carol_d.endpoint).await.unwrap();

    // Carol's own device acknowledges and accepts over signaling (device evidence).
    let carol_token = a.signaling_token(&carol_d).await;
    let ack = a.acknowledge(&carol_d, invitation).await;
    assert_eq!(ack.status, StatusCode::OK, "{}", ack.text);
    let version = ack.json["callVersion"].as_i64().unwrap();
    let accepted = a
        .as_endpoint(
            &carol_token,
            Method::POST,
            &format!("/v1/calling/invitations/{invitation}/accept"),
            Some(json!({ "expectedVersion": version })),
            &[],
        )
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.text);

    // Every party obtains a media credential over its own signaling credential: the
    // original callee too, although the call now has a pair it is not part of.
    for device in [&caller, &callee, &carol_d] {
        let session = a.session_on(call, device.endpoint).await.unwrap();
        let token = a.signaling_token(device).await;
        let r = a
            .as_endpoint(
                &token,
                Method::POST,
                &format!("/v1/calling/sessions/{session}/media-credentials"),
                Some(json!({})),
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }

    // Every pair was still checked, and evidence was only ever attached to a pair its
    // owner belongs to (no check was decided with a third party's credential).
    let checks = hub.checks.lock().unwrap().clone();
    let (a_id, b_id, c_id) = (caller.principal, callee.principal, carol.id().as_uuid());
    assert!(!checks
        .iter()
        .any(|&(x, y, o)| (x, y, o) == (a_id, b_id, Some(c_id))));
    assert!(checks.iter().any(|&(x, y, _)| (x, y) == (a_id, b_id)));
    assert!(checks
        .iter()
        .any(|&(x, y, o)| (x, y, o) == (a_id, c_id, Some(c_id))));
    assert!(checks
        .iter()
        .any(|&(x, y, o)| (x, y, o) == (a_id, b_id, Some(b_id))));
    for &(actor, target, owner) in &checks {
        if let Some(owner) = owner {
            assert!(owner == actor || owner == target, "{checks:?}");
        }
    }

    // The evidence is still enforced: once Carol's credentials are revoked her own
    // device is refused, while Bob's media access is unaffected.
    hub.revoked.lock().unwrap().push(c_id);
    for (device, expected) in [(&carol_d, StatusCode::CONFLICT), (&callee, StatusCode::OK)] {
        let session = a.session_on(call, device.endpoint).await.unwrap();
        let token = a.signaling_token(device).await;
        let r = a
            .as_endpoint(
                &token,
                Method::POST,
                &format!("/v1/calling/sessions/{session}/media-credentials"),
                Some(json!({})),
                &[],
            )
            .await;
        assert_eq!(r.status, expected, "{}", r.text);
    }
}

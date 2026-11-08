mod common;
use cf_communications_policy::{Action, Dispatch};
use cf_service_auth::{OutboundServiceRequest, ServiceId};
use common::*;
use communications::{
    error::Error,
    model::Privacy,
    store::{execute, one},
};
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test]
async fn notification_policy_uses_recipient_preferences_and_live_blocks() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    let request = check(Action::MessageNotification, &a, &b);
    assert!(
        h.state
            .check_policy(&h.scope, &request)
            .await
            .unwrap()
            .allowed
    );
    for preference in ["MUTED", "MENTIONS", "ALL"] {
        execute(&h.state.store.db,
            "UPDATE communication_contact_aliases SET notifications=$2,version=version+1 WHERE owner_profile_id=$1",
            vec![b.id.into(),preference.into()]).await.unwrap();
        let direct = h.state.check_policy(&h.scope, &request).await.unwrap();
        let group = h
            .state
            .check_policy(&h.scope, &check(Action::GroupNotification, &a, &b))
            .await
            .unwrap();
        assert_eq!(direct.allowed, preference == "ALL");
        assert_eq!(group.allowed, preference == "ALL");
        // B's preferences do not mute A's notifications about B.
        assert!(
            h.state
                .check_policy(&h.scope, &check(Action::MessageNotification, &b, &a))
                .await
                .unwrap()
                .allowed
        );
    }
    *h.blocked.write().unwrap() = true;
    assert!(
        !h.state
            .check_policy(&h.scope, &request)
            .await
            .unwrap()
            .allowed
    );
    h.stop_owners();
    assert!(h.state.check_policy(&h.scope, &request).await.is_err());
    h.close().await;
}

#[tokio::test]
async fn concurrent_lazy_activation_keeps_one_stable_account_and_principal_without_phone() {
    let h = Harness::new().await;
    let a = h.account();
    let s = h.delegated(&a);
    let mut concurrent = Vec::new();
    for _ in 0..16 {
        let (state, scope, account) = (h.state.clone(), s.clone(), a.clone());
        concurrent.push(tokio::spawn(async move {
            state.ensure_profile(&scope, &account).await
        }));
    }
    let mut created_ids = std::collections::BTreeSet::new();
    for task in concurrent {
        created_ids.insert(task.await.unwrap().unwrap().id);
    }
    assert_eq!(created_ids.len(), 1);
    let (x, y) = tokio::join!(
        h.state.ensure_profile(&s, &a),
        h.state.ensure_profile(&s, &a)
    );
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!(x.id, y.id);
    assert_eq!(x.principal_id, a.principal_id);
    assert_eq!(x.account_reference, a.account_reference);
    let v = h.state.me(&x).await.unwrap();
    assert_eq!(v["activationState"], "ACTIVE");
    assert_eq!(v["phoneVerification"]["required"], false);
    assert!(communications::crypto::valid_public_id(
        v["cfMeetId"].as_str().unwrap()
    ));
    let b = h.profile(&h.account()).await;
    assert_ne!(h.public_id(&x).await, h.public_id(&b).await);
    assert_eq!(x.display_name, b.display_name);
    h.close().await;
}
#[tokio::test]
async fn public_id_rotation_invalidates_old_identity_and_invitations_without_remapping() {
    let h = Harness::new().await;
    let a = h.account();
    let p = h.profile(&a).await;
    let old = h.public_id(&p).await;
    let invite = h
        .state
        .create_invite(
            &p,
            json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1}),
            Uuid::new_v4(),
            false,
        )
        .await
        .unwrap();
    let key = Uuid::new_v4();
    let r = h
        .state
        .rotate_public_id(&p, &a, json!({"expectedVersion":p.version}), key)
        .await
        .unwrap();
    assert_ne!(r["cfMeetId"], old);
    assert_eq!(
        r,
        h.state
            .rotate_public_id(&p, &a, json!({"expectedVersion":p.version}), key)
            .await
            .unwrap()
    );
    not_found(
        h.state
            .store
            .by_public_id(&h.state.store.db, p.scope(), &old)
            .await,
    );
    let p2 = h.profile(&a).await;
    assert_eq!(p2.principal_id, p.principal_id);
    assert_eq!(p2.id, p.id);
    let b = h.profile(&h.account()).await;
    let token = invite["payload"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap();
    not_found(
        h.state
            .resolve_invite(
                &h.scope,
                &b,
                json!({"token":token,"purpose":"CONNECT"}),
                Uuid::new_v4(),
            )
            .await,
    );
    assert!(matches!(
        h.state
            .rotate_public_id(
                &p2,
                &a,
                json!({"expectedVersion":p2.version}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::RateLimited)
    ));
    let mut stale = a;
    stale.authenticated_at = Some(chrono::Utc::now().timestamp() - 901);
    assert!(matches!(
        h.state
            .rotate_public_id(
                &p2,
                &stale,
                json!({"expectedVersion":p2.version}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::RecentAuthentication)
    ));
    h.close().await;
}
#[tokio::test]
async fn invitation_digest_purpose_expiry_revocation_usage_and_owner_are_enforced() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    let c = h.profile(&h.account()).await;
    let key = Uuid::new_v4();
    let input = json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1});
    let invite = h
        .state
        .create_invite(&a, input.clone(), key, false)
        .await
        .unwrap();
    let id = Uuid::parse_str(invite["inviteId"].as_str().unwrap()).unwrap();
    let token = invite["payload"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap();
    assert!(h
        .state
        .create_invite(&a, input, key, false)
        .await
        .unwrap()
        .get("payload")
        .is_none());
    let list = h.state.list_invites(&a, 10, Uuid::nil()).await.unwrap();
    assert!(!list.to_string().contains(token));
    assert!(!list.to_string().contains("token_digest"));
    not_found(h.state.revoke_invite(&b, id, Uuid::new_v4()).await);
    not_found(
        h.state
            .resolve_invite(
                &h.scope,
                &b,
                json!({"token":token,"purpose":"LOGIN"}),
                Uuid::new_v4(),
            )
            .await,
    );
    let key = Uuid::new_v4();
    let resolve = json!({"token":token,"purpose":"CONNECT"});
    let profile = h
        .state
        .resolve_invite(&h.scope, &b, resolve.clone(), key)
        .await
        .unwrap();
    assert_eq!(profile["relationship"], "NONE");
    assert!(profile.get("principalId").is_none());
    assert_eq!(
        profile,
        h.state
            .resolve_invite(&h.scope, &b, resolve.clone(), key)
            .await
            .unwrap()
    );
    not_found(
        h.state
            .resolve_invite(&h.scope, &b, resolve.clone(), Uuid::new_v4())
            .await,
    );
    not_found(
        h.state
            .resolve_invite(&h.scope, &c, resolve.clone(), Uuid::new_v4())
            .await,
    );
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &b, &a))
            .await
            .unwrap()
            .allowed
    );
    h.state.revoke_invite(&a, id, Uuid::new_v4()).await.unwrap();
    not_found(
        h.state
            .resolve_invite(&h.scope, &b, resolve, Uuid::new_v4())
            .await,
    );
    let expired = h
        .state
        .create_invite(
            &a,
            json!({"purpose":"CONNECT","expiresInSeconds":60,"maxUses":1}),
            Uuid::new_v4(),
            false,
        )
        .await
        .unwrap();
    let expired_id = Uuid::parse_str(expired["inviteId"].as_str().unwrap()).unwrap();
    execute(&h.state.store.db,"UPDATE communication_invites SET created_at=now()-interval '2 minutes',expires_at=now()-interval '1 minute',version=version+1 WHERE id=$1",vec![expired_id.into()]).await.unwrap();
    not_found(h.state.resolve_invite(&h.scope,&b,json!({"token":expired["payload"].as_str().unwrap().rsplit('/').next().unwrap(),"purpose":"CONNECT"}),Uuid::new_v4()).await);
    h.close().await;
}
/// J-02 (final closure): the CF Meet ID QR route `GET me/qr` through the signed dispatch
/// route the Meeting relay uses. It mints a one-time CONNECT invitation (10 minutes, one use)
/// whose payload is disclosed exactly once (an idempotent replay never repeats it, the
/// listing never shows it); the scanner resolves it once; a second scanner learns nothing;
/// an owner who disabled invitations gets no QR.
#[tokio::test]
async fn qr_route_mints_a_one_time_connect_payload_disclosed_once() {
    let h = Harness::new().await;
    let owner_account = h.account();
    let owner = h.profile(&owner_account).await;
    let scanner = h.profile(&h.account()).await;
    let late = h.profile(&h.account()).await;
    let path = "/internal/v1/communications/dispatch";
    let qr = |key: Uuid| {
        serde_json::to_value(Dispatch {
            method: "GET".into(),
            path: "me/qr".into(),
            query: Default::default(),
            body: None,
            idempotency_key: Some(key),
            account: owner_account.clone(),
        })
        .unwrap()
    };
    let key = Uuid::new_v4();
    let (s, v) = h
        .http(
            &h.account_signer,
            path,
            qr(key),
            h.delegated(&owner_account),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["purpose"].as_str(),
            v["maxUses"].as_i64(),
            v["secretReturned"].as_bool()
        ),
        (Some("CONNECT"), Some(1), Some(true))
    );
    let payload = v["payload"].as_str().expect("QR payload").to_string();
    let token = payload.rsplit('/').next().unwrap().to_string();
    assert!(payload.contains("/connect/"), "{payload}");
    let expires = chrono::DateTime::parse_from_rfc3339(v["expiresAt"].as_str().unwrap()).unwrap();
    let ttl = (expires.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    assert!((590..=600).contains(&ttl), "ten-minute QR, got {ttl}s");
    // Replay of the same request: same invitation, the secret is not disclosed again.
    let (s, replay) = h
        .http(
            &h.account_signer,
            path,
            qr(key),
            h.delegated(&owner_account),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(replay["inviteId"], v["inviteId"]);
    assert!(replay.get("payload").is_none(), "{replay}");
    let list = h.state.list_invites(&owner, 10, Uuid::nil()).await.unwrap();
    assert!(!list.to_string().contains(&token));
    // The scanner resolves it once; a later scanner learns nothing (one use).
    let resolve = json!({ "token": token, "purpose": "CONNECT" });
    let profile = h
        .state
        .resolve_invite(&h.scope, &scanner, resolve.clone(), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(profile["relationship"], "NONE");
    assert!(
        profile.get("principalId").is_none(),
        "no internal id leaks to the scanner"
    );
    not_found(
        h.state
            .resolve_invite(&h.scope, &late, resolve, Uuid::new_v4())
            .await,
    );
    // Invitations disabled by the owner: no QR is minted.
    h.state
        .set_privacy(
            &owner,
            json!({ "expectedVersion": 1, "privacy": Privacy {
                invitation_permissions: communications::model::InvitationPermission::Disabled,
                ..Privacy::default()
            } }),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let (s, v) = h
        .http(
            &h.account_signer,
            path,
            qr(Uuid::new_v4()),
            h.delegated(&owner_account),
        )
        .await;
    assert_eq!(s, 403, "{v}");
    h.close().await;
}
#[tokio::test]
async fn concurrent_acceptance_is_unique_and_never_grants_pending_or_foreign_requests() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    let foreign = h.profile(&h.account()).await;
    let body = json!({"cfMeetId":h.public_id(&b).await});
    let key = Uuid::new_v4();
    let r = h
        .state
        .send_request(&h.scope, &a, body.clone(), key)
        .await
        .unwrap();
    assert_eq!(
        r,
        h.state.send_request(&h.scope, &a, body, key).await.unwrap()
    );
    let id = Uuid::parse_str(r["requestId"].as_str().unwrap()).unwrap();
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectConversation, &a, &b))
            .await
            .unwrap()
            .allowed
    );
    not_found(
        h.state
            .decide_request(
                &h.scope,
                &foreign,
                id,
                "accept",
                json!({"expectedVersion":1}),
                Uuid::new_v4(),
            )
            .await,
    );
    not_found(
        h.state
            .decide_request(
                &h.scope,
                &a,
                id,
                "accept",
                json!({"expectedVersion":1}),
                Uuid::new_v4(),
            )
            .await,
    );
    let (x, y) = tokio::join!(
        h.state.decide_request(
            &h.scope,
            &b,
            id,
            "accept",
            json!({"expectedVersion":1}),
            Uuid::new_v4()
        ),
        h.state.decide_request(
            &h.scope,
            &b,
            id,
            "accept",
            json!({"expectedVersion":1}),
            Uuid::new_v4()
        )
    );
    assert!(x.is_ok() && y.is_ok());
    let count:Value=one(&h.state.store.db,"SELECT jsonb_build_object('pairs',count(*)) doc FROM communication_connections WHERE state='CONNECTED'",vec![]).await.unwrap().unwrap();
    assert_eq!(count["pairs"], 1);
    for action in [
        Action::DirectCall,
        Action::DirectConversation,
        Action::MessageSend,
    ] {
        assert!(
            h.state
                .check_policy(&h.scope, &check(action, &a, &b))
                .await
                .unwrap()
                .allowed
        );
    }
    h.close().await;
}
#[tokio::test]
async fn aliases_are_per_owner_and_removal_preserves_preferences_but_revokes_personal_permissions()
{
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    let ca = h.state.list_contacts(&a, 10, Uuid::nil()).await.unwrap();
    let cb = h.state.list_contacts(&b, 10, Uuid::nil()).await.unwrap();
    let aid = Uuid::parse_str(ca["data"][0]["contactId"].as_str().unwrap()).unwrap();
    let bid = Uuid::parse_str(cb["data"][0]["contactId"].as_str().unwrap()).unwrap();
    h.state.patch_contact(&a,aid,json!({"expectedVersion":1,"alias":"My saved name","favorite":true,"notifications":"MUTED"}),Uuid::new_v4()).await.unwrap();
    assert_eq!(
        h.state.contact_view(&a, aid).await.unwrap()["alias"],
        "My saved name"
    );
    assert_eq!(
        h.state.contact_view(&b, bid).await.unwrap()["alias"],
        Value::Null
    );
    not_found(
        h.state
            .patch_contact(
                &b,
                aid,
                json!({"expectedVersion":2,"alias":"unauthorized"}),
                Uuid::new_v4(),
            )
            .await,
    );
    h.state
        .remove_contact(&a, aid, Uuid::new_v4())
        .await
        .unwrap();
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::MessageSend, &a, &b))
            .await
            .unwrap()
            .allowed
    );
    assert_eq!(
        h.state.contact_view(&a, aid).await.unwrap()["alias"],
        "My saved name"
    );
    h.close().await;
}
#[tokio::test]
async fn blocking_and_domain_blocks_are_immediate_and_unblocking_never_reconnects() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    *h.blocked.write().unwrap() = true;
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a, &b))
            .await
            .unwrap()
            .allowed
    );
    *h.blocked.write().unwrap() = false;
    h.state
        .set_block(&b, &a, true, Uuid::new_v4())
        .await
        .unwrap();
    for action in [
        Action::MessageSend,
        Action::DirectCall,
        Action::ProfileDiscovery,
        Action::Presence,
    ] {
        assert!(
            !h.state
                .check_policy(&h.scope, &check(action, &a, &b))
                .await
                .unwrap()
                .allowed
        );
    }
    not_found(h.state.visible_profile(&h.scope, &a, &b, false).await);
    h.state
        .set_block(&b, &a, false, Uuid::new_v4())
        .await
        .unwrap();
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a, &b))
            .await
            .unwrap()
            .allowed
    );
    h.close().await;
}
#[tokio::test]
async fn privacy_is_versioned_private_discovery_is_neutral_and_capabilities_are_truthful() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    let privacy = Privacy {
        incoming_call_policy: communications::model::Audience::Nobody,
        public_id_discoverable: false,
        ..Privacy::default()
    };
    h.state
        .set_privacy(
            &b,
            json!({"expectedVersion":1,"privacy":privacy}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a, &b))
            .await
            .unwrap()
            .allowed
    );
    not_found(h.state.visible_profile(&h.scope, &a, &b, false).await);
    assert!(matches!(
        h.state
            .set_privacy(
                &b,
                json!({"expectedVersion":1,"privacy":privacy}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Version)
    ));
    let capabilities = h.state.capabilities(&h.scope, &a).await;
    assert_eq!(capabilities["messaging"]["available"], false);
    assert_eq!(capabilities["messaging"]["securityReady"], false);
    assert!(capabilities["messaging"].get("privateField").is_none());
    assert_eq!(capabilities["meetings"]["independent"], true);
    h.close().await;
}
#[tokio::test]
async fn signed_scope_identity_and_replay_cannot_be_forged_and_unknown_routes_stay_private() {
    let h = Harness::new().await;
    let account = h.account();
    let request = Dispatch {
        method: "GET".into(),
        path: "me".into(),
        query: Default::default(),
        body: None,
        idempotency_key: None,
        account: account.clone(),
    };
    let body = serde_json::to_value(request).unwrap();
    let path = "/internal/v1/communications/dispatch";
    assert_eq!(
        h.http_headers(path, body.clone(), Default::default())
            .await
            .0,
        401
    );
    let scope = h.delegated(&account);
    let headers = h
        .account_signer
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::communications())
                .with_delegated_context(scope.clone()),
        )
        .unwrap()
        .headers();
    assert_eq!(
        h.http_headers(path, body.clone(), headers.clone()).await.0,
        200
    );
    assert_eq!(h.http_headers(path, body.clone(), headers).await.0, 401);
    let mut forged = body.clone();
    forged["account"]["principalId"] = json!(Uuid::new_v4());
    assert_eq!(
        h.http(&h.account_signer, path, forged, scope.clone())
            .await
            .0,
        403
    );
    assert_eq!(h.http(&h.calling_signer, path, body, scope).await.0, 403);
    let p = h.profile(&account).await;
    let other_scope = cf_communications_policy::Scope {
        environment_id: Uuid::new_v4(),
        ..p.scope()
    };
    not_found(
        h.state
            .store
            .by_public_id(&h.state.store.db, other_scope, &h.public_id(&p).await)
            .await,
    );
    h.close().await;
}
#[tokio::test]
async fn suspension_logout_and_unavailable_owners_fail_closed_without_deleting_identities() {
    let h = Harness::new().await;
    let account = h.account();
    let a = h.profile(&account).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    {
        let mut accounts = h.accounts.write().unwrap();
        let p = accounts.get_mut(&a.principal_id).unwrap();
        p.active = false;
        p.version += 1;
    }
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a, &b))
            .await
            .unwrap()
            .allowed
    );
    assert!(matches!(
        h.state
            .ensure_profile(&h.delegated(&account), &account)
            .await,
        Err(Error::Forbidden)
    ));
    {
        let mut accounts = h.accounts.write().unwrap();
        let p = accounts.get_mut(&a.principal_id).unwrap();
        p.active = true;
        p.version += 1;
        p.cutoff = Some(chrono::Utc::now());
    }
    assert!(matches!(
        h.state
            .ensure_profile(&h.delegated(&account), &account)
            .await,
        Err(Error::RecentAuthentication)
    ));
    assert_eq!(
        h.state
            .store
            .principal(&h.state.store.db, a.scope(), a.principal_id)
            .await
            .unwrap()
            .id,
        a.id
    );
    h.close().await;
}
#[tokio::test]
async fn audit_and_authority_constraints_reject_mutation_and_cross_pair_aliases() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    let c = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    let pair = h
        .state
        .store
        .pair(&h.state.store.db, a.scope(), a.id, b.id, false)
        .await
        .unwrap()
        .unwrap();
    assert!(execute(
        &h.state.store.db,
        "UPDATE communication_profiles SET principal_id=$2,version=version+1 WHERE id=$1",
        vec![a.id.into(), Uuid::new_v4().into()]
    )
    .await
    .is_err());
    assert!(execute(&h.state.store.db,"INSERT INTO communication_contact_aliases(id,owner_profile_id,connection_id) VALUES($1,$2,$3)",vec![Uuid::new_v4().into(),c.id.into(),pair.id.into()]).await.is_err());
    assert!(
        execute(&h.state.store.db, "DELETE FROM communication_audit", vec![])
            .await
            .is_err()
    );
    h.close().await;
}

#[tokio::test]
async fn account_events_are_idempotent_monotonic_and_payload_bound() {
    let h = Harness::new().await;
    let a = h.account();
    let p = h.profile(&a).await;
    let event = json!({"eventId":Uuid::new_v4(),"accountReference":a.account_reference,"sourceVersion":2,"kind":"STATUS_CHANGED","state":"SUSPENDED","occurredAt":chrono::Utc::now()});
    let path = "/internal/v1/communications/account-events";
    let (status, result) = h
        .http(&h.account_signer, path, event.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 200);
    assert_eq!(result["applied"], true);
    let updated = h
        .state
        .store
        .principal(&h.state.store.db, p.scope(), p.principal_id)
        .await
        .unwrap();
    assert!(
        updated.credentials_not_before.is_some(),
        "suspension must revoke old credentials"
    );
    let projected: Value = one(&h.state.store.db,
        "SELECT payload doc FROM communication_outbox WHERE event_type='communications.account.changed' LIMIT 1",
        vec![]).await.unwrap().unwrap();
    assert!(projected["credentialsNotBefore"].is_i64());
    let (_, duplicate) = h
        .http(&h.account_signer, path, event.clone(), h.scope.clone())
        .await;
    assert_eq!(duplicate["duplicate"], true);
    let mut changed = event.clone();
    changed["state"] = json!("ACTIVE");
    assert_eq!(
        h.http(&h.account_signer, path, changed, h.scope.clone())
            .await
            .0,
        409
    );
    let mut stale = event;
    stale["eventId"] = json!(Uuid::new_v4());
    stale["sourceVersion"] = json!(1);
    stale["state"] = json!("ACTIVE");
    let (_, result) = h
        .http(&h.account_signer, path, stale, h.scope.clone())
        .await;
    assert_eq!(result["applied"], false);
    assert_eq!(
        h.state
            .store
            .principal(&h.state.store.db, p.scope(), p.principal_id)
            .await
            .unwrap()
            .activation_state,
        communications::model::Activation::Suspended
    );
    h.close().await;
}
#[tokio::test]
async fn durable_outbox_leases_deliver_concurrently_and_dead_events_replay_by_scoped_operator() {
    let h = Harness::new().await;
    let _a = h.profile(&h.account()).await;
    let (a, b) = tokio::join!(h.state.deliver_due(), h.state.deliver_due());
    assert_eq!(a.unwrap() + b.unwrap(), 2);
    let event: Value = one(
        &h.state.store.db,
        "SELECT jsonb_build_object('eventId',event_id) doc FROM communication_outbox LIMIT 1",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    let id = Uuid::parse_str(event["eventId"].as_str().unwrap()).unwrap();
    execute(
        &h.state.store.db,
        "UPDATE communication_outbox SET state='DEAD' WHERE event_id=$1",
        vec![id.into()],
    )
    .await
    .unwrap();
    let (_, result) = h
        .http(
            &h.operator,
            "/internal/v1/communications/replay",
            json!({"eventId":id}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(result["requeued"], 2);
    assert_eq!(h.state.deliver_due().await.unwrap(), 2);
    assert_eq!(h.state.deliver_due().await.unwrap(), 0);
    h.close().await;
}
#[tokio::test]
async fn dependency_and_database_outages_never_return_personal_authorization() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    h.stop_owners();
    assert!(h
        .state
        .check_policy(&h.scope, &check(Action::DirectCall, &a, &b))
        .await
        .is_err());
    h.state.store.db.clone().close().await.unwrap();
    assert!(h
        .state
        .check_policy(&h.scope, &check(Action::MessageSend, &a, &b))
        .await
        .is_err());
    h.close().await;
}
#[tokio::test]
async fn request_attempts_are_durable_rate_limited_and_self_requests_are_rejected() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    assert!(matches!(
        h.state
            .send_request(
                &h.scope,
                &a,
                json!({"cfMeetId":h.public_id(&a).await}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Validation)
    ));
    for _ in 0..59 {
        not_found(
            h.state
                .send_request(
                    &h.scope,
                    &a,
                    json!({"cfMeetId":"CF-NOT-A-PERSON"}),
                    Uuid::new_v4(),
                )
                .await,
        );
    }
    assert!(matches!(
        h.state
            .send_request(
                &h.scope,
                &a,
                json!({"cfMeetId":"CF-NOT-A-PERSON"}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::RateLimited)
    ));
    h.close().await;
}

#[tokio::test]
async fn existing_native_mapping_is_authoritative_and_another_valid_principal_cannot_impersonate_it(
) {
    let h = Harness::new().await;
    let a = h.account();
    let b = h.account();
    let p = h.profile(&a).await;
    assert_eq!(p.principal_id, a.principal_id);
    let mut forged = a.clone();
    forged.principal_id = b.principal_id;
    assert!(matches!(
        h.state.ensure_profile(&h.delegated(&forged), &forged).await,
        Err(Error::Forbidden)
    ));
    assert_eq!(h.profile(&a).await.id, p.id);
    h.close().await;
}
#[tokio::test]
async fn retention_expires_pending_pairs_and_preserves_identity_and_undelivered_events() {
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    let request = h
        .state
        .send_request(
            &h.scope,
            &a,
            json!({"cfMeetId":h.public_id(&b).await}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let id = Uuid::parse_str(request["requestId"].as_str().unwrap()).unwrap();
    execute(&h.state.store.db,"UPDATE communication_contact_requests SET expires_at=now()-interval '1 second',version=version+1 WHERE id=$1",vec![id.into()]).await.unwrap();
    h.state.maintain().await.unwrap();
    h.state.maintain().await.unwrap();
    let result:Value=one(&h.state.store.db,"SELECT jsonb_build_object('request',r.state,'connection',c.state) doc FROM communication_contact_requests r JOIN communication_connections c ON c.id=r.connection_id WHERE r.id=$1",vec![id.into()]).await.unwrap().unwrap();
    assert_eq!(result["request"], "EXPIRED");
    assert_eq!(result["connection"], "NONE");
    assert_eq!(
        h.state
            .store
            .principal(&h.state.store.db, a.scope(), a.principal_id)
            .await
            .unwrap()
            .id,
        a.id
    );
    let result:Value=one(&h.state.store.db,"SELECT jsonb_build_object('count',count(*)) doc FROM communication_outbox WHERE state='PENDING'",vec![]).await.unwrap().unwrap();
    assert!(result["count"].as_i64().unwrap() > 0);
    h.close().await;
}

#[tokio::test]
async fn concurrent_invitation_creation_cannot_survive_disabling_and_reenabling_invites() {
    let h = Harness::new().await;
    let reader = h.profile(&h.account()).await;
    for _ in 0..4 {
        let owner = h.profile(&h.account()).await;
        let disabled = Privacy {
            invitation_permissions: communications::model::InvitationPermission::Disabled,
            ..Privacy::default()
        };
        let (created, changed) = tokio::join!(
            h.state.create_invite(
                &owner,
                json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1}),
                Uuid::new_v4(),
                false
            ),
            h.state.set_privacy(
                &owner,
                json!({"expectedVersion":1,"privacy":disabled}),
                Uuid::new_v4()
            )
        );
        changed.unwrap();
        h.state
            .set_privacy(
                &owner,
                json!({"expectedVersion":2,"privacy":Privacy::default()}),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        match created {
            Ok(invite)=>not_found(h.state.resolve_invite(&h.scope,&reader,json!({"purpose":"CONNECT","token":invite["payload"].as_str().unwrap().rsplit('/').next().unwrap()}),Uuid::new_v4()).await),
            Err(Error::Forbidden)=>{},
            Err(error)=>panic!("unexpected creation result: {error}"),
        }
    }
    h.close().await;
}

/// Blueprint Section 11: the Account learns only which recipients turned meeting
/// notifications off; only the Meeting Service with `communications-preferences.read` may ask.
#[tokio::test]
async fn meeting_notification_opt_outs_are_listed_for_the_account_only() {
    let h = Harness::new().await;
    let quiet = h.account();
    let loud = h.account();
    let quiet_profile = h.profile(&quiet).await;
    let loud_profile = h.profile(&loud).await;
    let without_profile = format!("cfmeet-user:{}", Uuid::new_v4());
    execute(&h.state.store.db,
        "INSERT INTO communication_notification_preferences(profile_id,meeting_invitations) VALUES($1,false),($2,true)",
        vec![quiet_profile.id.into(), loud_profile.id.into()]).await.unwrap();
    let path = "/internal/v1/communications/notification-preferences/resolve";
    let body = json!({"category":"MEETINGS","accountReferences":[quiet.account_reference,loud.account_reference,without_profile]});
    let (status, result) = h
        .http(&h.account_signer, path, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["schemaVersion"], 1);
    assert_eq!(result["optedOut"], json!([quiet.account_reference]));
    // Any other caller is refused (Calling holds communications-policy.check only).
    let (status, _) = h
        .http(&h.calling_signer, path, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 403);
    // Bounded, canonical input only.
    for bad in [
        json!({"category":"CALLS","accountReferences":[quiet.account_reference]}),
        json!({"category":"MEETINGS","accountReferences":[]}),
        json!({"category":"MEETINGS","accountReferences":["someone@example.com"]}),
        json!({"category":"MEETINGS","accountReferences":[quiet.account_reference,quiet.account_reference]}),
        json!({"category":"MEETINGS","accountReferences":[quiet.account_reference],"extra":true}),
    ] {
        let (status, _) = h.http(&h.account_signer, path, bad, h.scope.clone()).await;
        assert_eq!(status, 400);
    }
    let many: Vec<String> = (0..101)
        .map(|_| format!("cfmeet-user:{}", Uuid::new_v4()))
        .collect();
    let (status, _) = h
        .http(
            &h.account_signer,
            path,
            json!({"category":"MEETINGS","accountReferences":many}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, 400);
    h.close().await;
}

/// Section 14 (ACC-010): an account that never opened the Hub still has its Messaging
/// and Calling credentials revoked. The Hub asks Account for the principal and fans the
/// revocation out durably, exactly once per Account event.
#[tokio::test]
async fn account_revocations_without_a_profile_fan_out_to_owners_by_principal() {
    let h = Harness::new().await;
    let a = h.account();
    let path = "/internal/v1/communications/account-events";
    let at = chrono::Utc::now();
    let event = json!({"eventId":Uuid::new_v4(),"accountReference":a.account_reference,"sourceVersion":3,"kind":"SESSION_REVOKED","state":"ACCOUNT_CREATED","occurredAt":at});
    let (status, result) = h
        .http(&h.account_signer, path, event.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (result["applied"].clone(), result["fanOut"].clone()),
        (json!(true), json!("PRINCIPAL"))
    );
    let rows: Value = one(&h.state.store.db,
        "SELECT jsonb_agg(jsonb_build_object('destination',destination,'aggregate',aggregate_id,'payload',payload) ORDER BY destination) doc FROM communication_outbox WHERE event_type='communications.account.changed'",
        vec![]).await.unwrap().unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (row, destination) in rows.iter().zip(["CALLING", "MESSAGING"]) {
        assert_eq!(row["destination"], destination);
        assert_eq!(row["aggregate"], json!(a.principal_id));
        assert_eq!(row["payload"]["principalId"], json!(a.principal_id));
        assert_eq!(
            row["payload"]["credentialsNotBefore"],
            json!(at.timestamp())
        );
    }
    // No profile was created as a side effect.
    let profiles: Value = one(
        &h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_profiles",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(profiles["n"], 0);
    // A retry of the same event is a duplicate and enqueues nothing more.
    let (_, duplicate) = h
        .http(&h.account_signer, path, event, h.scope.clone())
        .await;
    assert_eq!(duplicate["duplicate"], true);
    // Suspension and deactivation revoke too; a plain reactivation does not.
    for (state, fans_out) in [("SUSPENDED", true), ("ACCOUNT_CREATED", false)] {
        let e = json!({"eventId":Uuid::new_v4(),"accountReference":a.account_reference,"sourceVersion":4,"kind":"STATUS_CHANGED","state":state,"occurredAt":chrono::Utc::now()});
        let (_, r) = h.http(&h.account_signer, path, e, h.scope.clone()).await;
        assert_eq!(r["applied"], fans_out, "{state}");
    }
    let count: Value = one(&h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_outbox WHERE event_type='communications.account.changed'",
        vec![]).await.unwrap().unwrap();
    assert_eq!(count["n"], 4);
    // An account Account has never mapped to a principal has nothing to revoke.
    let unknown = json!({"eventId":Uuid::new_v4(),"accountReference":format!("cfmeet-user:{}",Uuid::new_v4()),"sourceVersion":1,"kind":"SESSION_REVOKED","state":"ACCOUNT_CREATED","occurredAt":chrono::Utc::now()});
    let (status, r) = h
        .http(&h.account_signer, path, unknown, h.scope.clone())
        .await;
    assert_eq!((status.as_u16(), r["applied"].clone()), (200, json!(false)));
    // The fan-out is delivered to both owners through the normal leased outbox.
    assert_eq!(h.state.deliver_due().await.unwrap(), 4);
    // Account down: the event is not acknowledged, so Account's durable outbox retries.
    let b = h.account();
    h.stop_owners();
    let e = json!({"eventId":Uuid::new_v4(),"accountReference":b.account_reference,"sourceVersion":2,"kind":"SESSION_REVOKED","state":"ACCOUNT_CREATED","occurredAt":chrono::Utc::now()});
    let (status, _) = h.http(&h.account_signer, path, e, h.scope.clone()).await;
    assert_ne!(status, 200);
    h.close().await;
}

/// Section 14 (ACC-008): ACCOUNT_SESSION_ENDED carries exactly one well-formed session
/// reference, and no other kind may carry one.
#[tokio::test]
async fn account_session_events_are_shape_checked() {
    let h = Harness::new().await;
    let a = h.account();
    let _p = h.profile(&a).await;
    let path = "/internal/v1/communications/account-events";
    let reference = "ab".repeat(32);
    for (kind, session, ok) in [
        ("ACCOUNT_SESSION_ENDED", Some(reference.clone()), true),
        ("ACCOUNT_SESSION_ENDED", None, false),
        ("ACCOUNT_SESSION_ENDED", Some("AB".repeat(32)), false),
        ("SESSION_REVOKED", Some(reference.clone()), false),
    ] {
        let mut e = json!({"eventId":Uuid::new_v4(),"accountReference":a.account_reference,"sourceVersion":1,"kind":kind,"state":"ACCOUNT_CREATED","occurredAt":chrono::Utc::now()});
        if let Some(s) = session {
            e["accountSessionRef"] = json!(s);
        }
        let (status, r) = h.http(&h.account_signer, path, e, h.scope.clone()).await;
        assert_eq!(status.as_u16() == 200, ok, "{kind} {r}");
        if ok {
            assert_eq!(r["devicesRevoked"], 0);
        }
    }
    // A session end never touches the account epoch or activation.
    let p = h.profile(&a).await;
    assert!(p.credentials_not_before.is_none());
    h.close().await;
}

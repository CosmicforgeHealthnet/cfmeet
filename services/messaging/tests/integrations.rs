//! Part V H §§97–104 / I §116 — the internal integration surface, through the real
//! router and PostgreSQL with REAL service-token verification:
//!
//! * `/internal/v1/delegated/**` (`messaging-application.delegate`): the first-party
//!   Meeting Service acts as its own application's backend (devices with real OpenMLS
//!   clients, client tokens, logout) — refused without / with a forged / an EXTERNAL /
//!   a suspended delegated scope, for other peers, rogue keys and customer credentials.
//! * `/internal/v1/meeting-conversations` (`messaging-meeting.sync`): idempotent meeting
//!   conversation resolution, version-fenced membership / ban / role / lifecycle, legacy
//!   import with `LEGACY_MANAGED` provenance (never into E2EE).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use messaging::identity::PrincipalStatus;
use serde_json::{json, Value};
use uuid::Uuid;

fn delegated(c: &Customer) -> cf_service_auth::DelegatedPlatformContext {
    c.context.scope.delegated()
}

impl Harness {
    async fn as_meeting(
        &self,
        c: &Customer,
        method: Method,
        path: &str,
        body: Option<Value>,
        acting: Option<Uuid>,
    ) -> Response {
        let peers = peers();
        let a = acting.map(|p| p.to_string());
        let extra: Vec<(&str, &str)> = a
            .as_deref()
            .map(|p| vec![("cf-acting-principal", p)])
            .unwrap_or_default();
        self.call_internal(
            &peers.meeting,
            method,
            path,
            body,
            Some(delegated(c)),
            &extra,
        )
        .await
    }

    /// Lets MEETING conversations be MANAGED (meeting chat is server-readable today).
    async fn meeting_policy(&self, c: &Customer, extra: Value) {
        let mut meeting =
            json!({ "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" });
        meeting
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.set_policy(
            c,
            json!({ "contexts": { "MEETING": meeting,
                "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } }),
        )
            .await;
    }

    async fn resolve_meeting(&self, c: &Customer, owner: Uuid, meeting_ref: &str) -> Response {
        self.as_meeting(
            c,
            Method::POST,
            "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": meeting_ref, "ownerPrincipalId": owner, "securityProfile": "MANAGED" })),
            None,
        )
        .await
    }

    async fn sync(&self, c: &Customer, conv: &str, principal: Uuid, body: Value) -> Response {
        self.as_meeting(
            c,
            Method::PUT,
            &format!("/internal/v1/meeting-conversations/{conv}/members/{principal}"),
            Some(body),
            None,
        )
        .await
    }
}

#[tokio::test]
async fn delegated_first_party_backend_manages_devices_and_client_tokens() {
    // A runtime with client-token signing keys (realtime enabled).
    let Some(rt) = common::realtime::Rt::start().await else {
        return;
    };
    let h = &rt.h;
    let c = h.first_party(true);
    let user = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let base = "/internal/v1/delegated/v1/messaging";
    // Device registration with proof of possession by a real OpenMLS client device.
    let ch = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/devices/registration-challenges"),
            Some(json!({})),
            Some(user),
        )
        .await;
    assert_eq!(ch.status, StatusCode::CREATED, "{}", ch.text);
    let device_id = uuid(&ch.json["deviceId"]);
    let nonce: [u8; 32] = hex::decode(ch.json["nonce"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let device = cf_messaging_crypto::device::Device::generate(device_id).unwrap();
    let kp = device
        .registration_key_package(uuid(&ch.json["challengeId"]), &nonce)
        .unwrap();
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/devices"),
            Some(json!({ "challengeId": ch.json["challengeId"], "keyPackage": b64(&kp) })),
            Some(user),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // A short-lived device-bound client token (no service credential leaves the backend).
    let t = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/client-tokens"),
            Some(json!({ "deviceId": device_id })),
            Some(user),
        )
        .await;
    assert_eq!(t.status, StatusCode::CREATED, "{}", t.text);
    assert_eq!(t.json["deviceId"], json!(device_id));
    assert!(t.json["token"].as_str().unwrap().split('.').count() == 3);
    // Another principal's device cannot be used.
    let other = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/client-tokens"),
            Some(json!({ "deviceId": device_id })),
            Some(other),
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.text);
    // Logout everywhere revokes every session of the principal.
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/client-sessions/revoke-all"),
            None,
            Some(user),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["revokedSessions"], 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_client_sessions WHERE status = 'ACTIVE'")
            .await,
        0
    );
    // Delegation never confers moderation / export authority.
    let r = h
        .as_meeting(&c, Method::GET, &format!("{base}/reports"), None, None)
        .await;
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE", "{}", r.text);
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/exports"),
            Some(json!({ "conversationId": Uuid::new_v4() })),
            None,
        )
        .await;
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE", "{}", r.text);
    // A suspended principal is refused.
    h.set_principal(&c, user, PrincipalStatus::Inactive);
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("{base}/client-tokens"),
            Some(json!({ "deviceId": device_id })),
            Some(user),
        )
        .await;
    assert_eq!(r.code(), "PRINCIPAL_NOT_AUTHORIZED", "{}", r.text);
}

#[tokio::test]
async fn delegation_is_refused_without_a_valid_first_party_scope() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let user = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let path = "/internal/v1/delegated/v1/messaging/devices/registration-challenges";
    let peers = peers();
    let acting = user.to_string();
    let hdr = [("cf-acting-principal", acting.as_str())];
    // No delegated scope.
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            None,
            &hdr,
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_REQUIRED", "{}", r.text);
    // A forged scope (unknown chain).
    let forged = cf_service_auth::DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        c.context.scope.environment_id,
        cf_service_auth::ApplicationEnvironmentKind::Sandbox,
        Uuid::new_v4(),
    );
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(forged),
            &hdr,
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.text);
    // An EXTERNAL tenant's scope cannot be acted for through a peer service.
    let external = h.first_party(false);
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(delegated(&external)),
            &hdr,
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.text);
    // Peers without the permission, rogue keys, untrusted services, customer credentials.
    for signer in [&peers.privacy_relay, &peers.sms_gateway] {
        let r = h
            .call_internal(
                signer,
                Method::POST,
                path,
                Some(json!({})),
                Some(delegated(&c)),
                &hdr,
            )
            .await;
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED", "{}", r.text);
    }
    for signer in [&peers.rogue_meeting, &h.untrusted] {
        let r = h
            .call_internal(
                signer,
                Method::POST,
                path,
                Some(json!({})),
                Some(delegated(&c)),
                &hdr,
            )
            .await;
        assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED", "{}", r.text);
    }
    let r = h
        .send(
            Method::POST,
            Some(&c.token),
            Some(user),
            path,
            Some(json!({})),
            &[],
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED", "{}", r.text);
    // A suspended environment (authority SAFETY_ONLY) and Platform Access unavailable.
    h.authority
        .delegated
        .lock()
        .unwrap()
        .get_mut(&c.context.scope.environment_id)
        .unwrap()
        .2 = false;
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(delegated(&c)),
            &hdr,
        )
        .await;
    assert_eq!(r.code(), "AUTHORITY_SUSPENDED", "{}", r.text);
    *h.authority.down.lock().unwrap() = true;
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(delegated(&c)),
            &hdr,
        )
        .await;
    assert_eq!(r.code(), "SERVICE_UNAVAILABLE", "{}", r.text);
}

#[tokio::test]
async fn meeting_conversations_resolve_idempotently_and_follow_meeting_membership() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    h.meeting_policy(&c, json!({})).await;
    let host = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let guest = h.principal_of_kind(&c, "TEMPORARY_EXTERNAL_PARTICIPANT");
    let meeting_ref = format!("conference:{}", Uuid::new_v4());

    // Concurrent / retried resolutions converge on ONE conversation.
    let rs =
        futures_util::future::join_all((0..5).map(|_| h.resolve_meeting(&c, host, &meeting_ref)))
            .await;
    let ids: std::collections::BTreeSet<String> = rs
        .iter()
        .map(|r| {
            r.json["conversation"]["conversationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{}", r.text))
                .to_string()
        })
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    assert_eq!(rs.iter().filter(|r| r.json["created"] == true).count(), 1);
    let conv = rs[0].json["conversation"].clone();
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    assert_eq!(conv["contextClass"], "MEETING");
    assert_eq!(conv["securityProfile"], "MANAGED");
    assert_eq!(conv["encryption"]["serverCanReadContent"], true);

    // Admission (version 10), a stale duplicate (version 9) and a role change.
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "ACTIVE", "sourceVersion": 10 }),
        )
        .await;
    assert_eq!(
        (r.status, &r.json["applied"]),
        (StatusCode::OK, &json!(true)),
        "{}",
        r.text
    );
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "REMOVED", "sourceVersion": 9 }),
        )
        .await;
    assert_eq!(r.json["reason"], "STALE_VERSION", "{}", r.text);
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "ACTIVE", "role": "ADMIN", "sourceVersion": 11 }),
        )
        .await;
    assert_eq!(r.json["member"]["role"], "ADMIN", "{}", r.text);
    // The admitted guest sends through the delegated backend.
    let send = |text: &'static str, from: Uuid| {
        let h = &h;
        let c = &c;
        let cid = cid.clone();
        async move {
            h.as_meeting(
                c,
                Method::POST,
                &format!("/internal/v1/delegated/v1/conversations/{cid}/messages"),
                Some(json!({ "clientMessageId": Uuid::new_v4(), "text": text, "notify": false })),
                Some(from),
            )
            .await
        }
    };
    let m = send("hello from the guest", guest).await;
    assert_eq!(m.status, StatusCode::CREATED, "{}", m.text);
    assert_eq!(m.json["securityProvenance"], "NATIVE");

    // Removal: the guest can no longer send or read; a replayed older admission is stale.
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "REMOVED", "sourceVersion": 12 }),
        )
        .await;
    assert_eq!(r.json["member"]["state"], "REMOVED", "{}", r.text);
    assert_eq!(
        send("still here?", guest).await.status,
        StatusCode::NOT_FOUND
    );
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "ACTIVE", "sourceVersion": 11 }),
        )
        .await;
    assert_eq!(r.json["applied"], false, "replay must not restore access");
    assert_eq!(send("replayed", guest).await.status, StatusCode::NOT_FOUND);

    // A ban keeps the principal out of the public membership API as well.
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "BANNED", "sourceVersion": 13 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .post(
            &c,
            None,
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": guest }),
        )
        .await;
    assert_eq!(r.reason(), "PRINCIPAL_BANNED", "{}", r.text);
    // Re-admission by the meeting authority (newer version) lifts the ban.
    let r = h
        .sync(
            &c,
            &cid,
            guest,
            json!({ "state": "ACTIVE", "sourceVersion": 14 }),
        )
        .await;
    assert_eq!(r.json["member"]["state"], "ACTIVE", "{}", r.text);
    // An inactive principal is never admitted.
    let gone = h.principal_of_kind(&c, "TEMPORARY_EXTERNAL_PARTICIPANT");
    h.set_principal(&c, gone, PrincipalStatus::Inactive);
    let r = h
        .sync(
            &c,
            &cid,
            gone,
            json!({ "state": "ACTIVE", "sourceVersion": 1 }),
        )
        .await;
    assert_eq!(r.code(), "PRINCIPAL_NOT_AUTHORIZED", "{}", r.text);

    // The meeting ends: CLOSED, sends refused; a stale lifecycle update is ignored.
    let r = h
        .as_meeting(&c, Method::POST, &format!("/internal/v1/meeting-conversations/{cid}/lifecycle"),
            Some(json!({ "lifecycleState": "CLOSED", "sourceVersion": 20, "reason": "MEETING_ENDED" })), None)
        .await;
    assert_eq!(r.json["applied"], true, "{}", r.text);
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("/internal/v1/meeting-conversations/{cid}/lifecycle"),
            Some(json!({ "lifecycleState": "ACTIVE", "sourceVersion": 19 })),
            None,
        )
        .await;
    assert_eq!(r.json["applied"], false, "{}", r.text);
    let r = send("after the end", host).await;
    assert_eq!(r.code(), "CONVERSATION_NOT_ACTIVE", "{}", r.text);

    // Only meeting conversations of the caller's own environment are reachable.
    let other_conv = h
        .create_conversation(&c, host, json!({ "contextClass": "GROUP" }))
        .await;
    let ocid = other_conv.json["conversationId"].as_str().unwrap();
    let r = h
        .sync(
            &c,
            ocid,
            guest,
            json!({ "state": "ACTIVE", "sourceVersion": 1 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let foreign = h.first_party(true);
    let r = h
        .sync(
            &foreign,
            &cid,
            guest,
            json!({ "state": "REMOVED", "sourceVersion": 99 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // The fences are monotonic in the database itself.
    assert!(h
        .sql("UPDATE messaging_sync_fences SET source_version = 0")
        .await
        .is_err());
    // Audit of every applied sync.
    assert!(
        h.scalar("SELECT count(*) FROM messaging_audit WHERE action = 'MEETING_MEMBER_SYNCED'")
            .await
            >= 5
    );
}

#[tokio::test]
async fn meeting_resolution_never_downgrades_and_respects_regulated_disclosure() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let host = h.principal_of_kind(&c, "NATIVE_CF_USER");
    // Default policy: MEETING is PRIVATE_E2EE-only; a MANAGED request is refused, never
    // silently converted.
    let r = h.resolve_meeting(&c, host, "conference:a").await;
    assert_eq!(r.code(), "SECURITY_PROFILE_NOT_ALLOWED", "{}", r.text);
    // E2EE requested without the host's device: E2EE_NOT_AVAILABLE (nothing created).
    let r = h
        .as_meeting(&c, Method::POST, "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conference:b", "ownerPrincipalId": host, "securityProfile": "PRIVATE_E2EE" })), None)
        .await;
    assert_eq!(r.code(), "E2EE_NOT_AVAILABLE", "{}", r.text);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_conversations")
            .await,
        0
    );
    // A regulated MEETING context requires the explicit MANAGED disclosure.
    h.meeting_policy(&c, json!({ "regulated": true })).await;
    let r = h.resolve_meeting(&c, host, "conference:c").await;
    assert_eq!(r.reason(), "MANAGED_DISCLOSURE_REQUIRED", "{}", r.text);
    let r = h
        .as_meeting(&c, Method::POST, "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conference:c", "ownerPrincipalId": host, "securityProfile": "MANAGED", "managedDisclosureAcknowledged": true })), None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // A meeting reference already held by a MANAGED conversation cannot become E2EE.
    let r = h
        .as_meeting(&c, Method::POST, "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conference:c", "ownerPrincipalId": host, "securityProfile": "PRIVATE_E2EE" })), None)
        .await;
    assert!(r.status.is_client_error(), "{}", r.text);
    // Malformed references never reach the database.
    for bad in ["", "has space", &"x".repeat(300)] {
        let r = h.resolve_meeting(&c, host, bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
}

#[tokio::test]
async fn legacy_chat_is_imported_as_legacy_managed_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    h.meeting_policy(&c, json!({})).await;
    let host = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let guest = h.principal_of_kind(&c, "TEMPORARY_EXTERNAL_PARTICIPANT");
    let r = h.resolve_meeting(&c, host, "conference:legacy").await;
    let cid = r.json["conversation"]["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    h.sync(
        &c,
        &cid,
        guest,
        json!({ "state": "ACTIVE", "sourceVersion": 1 }),
    )
    .await;
    let outsider = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let batch = json!({
        "legacySource": "meeting.conference-messages",
        "messages": [
            { "legacyMessageId": "m-1", "senderPrincipalId": host, "text": "welcome everyone", "sentAt": "2026-10-01T09:00:00Z", "presentation": "ANNOUNCEMENT" },
            { "legacyMessageId": "m-2", "senderPrincipalId": guest, "text": "hi!", "sentAt": "2026-10-01T09:00:05Z" },
            { "legacyMessageId": "m-3", "senderPrincipalId": guest, "sentAt": "2026-10-01T09:00:07Z" },
            { "legacyMessageId": "m-4", "senderPrincipalId": outsider, "text": "never a member", "sentAt": "2026-10-01T09:01:00Z" },
        ],
    });
    let path = format!("/internal/v1/meeting-conversations/{cid}/legacy-imports");
    let r = h
        .as_meeting(&c, Method::POST, &path, Some(batch.clone()), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["imported"].as_i64(), r.json["duplicates"].as_i64()),
        (Some(3), Some(0)),
        "{}",
        r.text
    );
    assert_eq!(r.json["refused"][0]["reason"], "SENDER_NOT_A_MEMBER");
    // Idempotent: a retried batch imports nothing twice.
    let again = h
        .as_meeting(&c, Method::POST, &path, Some(batch), None)
        .await;
    assert_eq!(
        (
            again.json["imported"].as_i64(),
            again.json["duplicates"].as_i64()
        ),
        (Some(0), Some(3))
    );
    // Both answers map each legacy id to the SAME canonical message.
    let ids = |v: &Value| -> Vec<(String, String)> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["legacyMessageId"].as_str().unwrap().to_string(),
                    i["messageId"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(ids(&r.json).len(), 3);
    assert_eq!(ids(&r.json), ids(&again.json));
    assert!(again.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["status"] == "DUPLICATE"));
    // The imported history is truthfully labelled and keeps its original time.
    let list = h
        .get(&c, Some(host), &format!("/v1/conversations/{cid}/messages"))
        .await;
    let msgs = list.json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "{}", list.text);
    assert!(msgs
        .iter()
        .all(|m| m["securityProvenance"] == "LEGACY_MANAGED"));
    assert_eq!(msgs[0]["originalCreatedAt"], "2026-10-01T09:00:00.000000Z");
    assert_eq!(msgs[0]["managed"]["presentation"], "ANNOUNCEMENT");
    assert_eq!(msgs[1]["managed"]["text"], "hi!");
    assert_eq!(msgs[2]["lifecycle"], "DELETED");
    assert_eq!(msgs[2]["managed"]["text"], Value::Null);
    // History pagination newest first.
    let page = h
        .get(
            &c,
            Some(host),
            &format!("/v1/conversations/{cid}/messages?beforeSequence=3&limit=1"),
        )
        .await;
    assert_eq!(page.json["messages"][0]["sequence"], 2, "{}", page.text);
    assert_eq!(page.json["nextBeforeSequence"], 2);
    // Imports are durable events without push notifications.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.message.created' AND payload->>'security_provenance' = 'LEGACY_MANAGED' AND (payload->>'notify')::boolean = false").await,
        3
    );
    // The database refuses legacy provenance on anything but MANAGED application messages.
    assert!(h
        .sql("UPDATE messaging_messages SET security_provenance = 'NATIVE', legacy_source = NULL, legacy_message_id = NULL, original_created_at = NULL")
        .await
        .is_err());

    // Never into an E2EE conversation.
    h.meeting_policy(&c, json!({})).await;
    let mut owner = h.member(&c, 1).await;
    let r = h
        .as_meeting(&c, Method::POST, "/internal/v1/meeting-conversations",
            Some(json!({ "meetingRef": "conference:e2ee", "ownerPrincipalId": owner.principal, "securityProfile": "PRIVATE_E2EE" })), None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let e2ee = r.json["conversation"]["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h
        .as_meeting(&c, Method::POST, &format!("/internal/v1/meeting-conversations/{e2ee}/legacy-imports"),
            Some(json!({ "legacySource": "meeting.conference-messages", "messages": [
                { "legacyMessageId": "x", "senderPrincipalId": owner.principal, "text": "plaintext", "sentAt": "2026-10-01T09:00:00Z" }] })), None)
        .await;
    assert_eq!(r.reason(), "LEGACY_IMPORT_REQUIRES_MANAGED", "{}", r.text);
    let _ = &mut owner;
}

#[tokio::test]
async fn the_signed_initiating_principal_is_the_acting_principal() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    let user = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let other = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let path = "/internal/v1/delegated/v1/messaging/devices/registration-challenges";
    let peers = peers();
    let signed = delegated(&c)
        .with_initiating_principal(user, cf_service_auth::DelegatedPrincipalKind::NativeCfUser);
    // Signed principal alone: acts for it.
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(signed.clone()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // The same principal repeated in the header is accepted.
    let same = user.to_string();
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(signed.clone()),
            &[("cf-acting-principal", same.as_str())],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // An unsigned header can never re-target a signed request.
    let forged = other.to_string();
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            path,
            Some(json!({})),
            Some(signed),
            &[("cf-acting-principal", forged.as_str())],
        )
        .await;
    assert_eq!(r.code(), "PRINCIPAL_NOT_AUTHORIZED", "{}", r.text);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_device_challenges WHERE principal_id = '{other}'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn the_meeting_authority_removes_messages_without_reading_them_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    h.meeting_policy(&c, json!({})).await;
    let host = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let guest = h.principal_of_kind(&c, "TEMPORARY_EXTERNAL_PARTICIPANT");
    let r = h.resolve_meeting(&c, host, "conference:moderation").await;
    let cid = r.json["conversation"]["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    h.sync(
        &c,
        &cid,
        guest,
        json!({ "state": "ACTIVE", "sourceVersion": 1 }),
    )
    .await;
    // The guest posts through the delegated customer surface (signed acting principal).
    let sent = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("/internal/v1/delegated/v1/conversations/{cid}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "spam spam" })),
            Some(guest),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let mid = sent.json["messageId"].as_str().unwrap().to_string();
    let path = format!("/internal/v1/meeting-conversations/{cid}/messages/{mid}/moderation");
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &path,
            Some(json!({ "action": "REMOVE" })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["lifecycle"].as_str(), r.json["changed"].as_bool()),
        (Some("MODERATED"), Some(true))
    );
    assert!(!r.text.contains("spam"), "content is never returned");
    // Idempotent.
    let again = h
        .as_meeting(
            &c,
            Method::POST,
            &path,
            Some(json!({ "action": "REMOVE" })),
            None,
        )
        .await;
    assert_eq!(again.json["changed"], false, "{}", again.text);
    // Members see a tombstone without text.
    let list = h
        .get(&c, Some(host), &format!("/v1/conversations/{cid}/messages"))
        .await;
    let m = &list.json["messages"][0];
    assert_eq!(m["lifecycle"], "MODERATED", "{}", list.text);
    assert!(m["managed"]["text"].is_null());
    // Unknown action / foreign message / other signers.
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &path,
            Some(json!({ "action": "READ" })),
            None,
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!(
                "/internal/v1/meeting-conversations/{cid}/messages/{}/moderation",
                Uuid::new_v4()
            ),
            Some(json!({ "action": "REMOVE" })),
            None,
        )
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
    let peers = peers();
    let r = h
        .call_internal(
            &peers.privacy_relay,
            Method::POST,
            &path,
            Some(json!({ "action": "REMOVE" })),
            Some(delegated(&c)),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
}

#[tokio::test]
async fn former_participants_keep_authorship_without_ever_gaining_access() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(true);
    h.meeting_policy(&c, json!({})).await;
    let host = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let left = h.principal_of_kind(&c, "TEMPORARY_EXTERNAL_PARTICIPANT");
    let r = h.resolve_meeting(&c, host, "conference:former").await;
    let cid = r.json["conversation"]["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    // The participant left the meeting before the conversation existed.
    let r = h
        .sync(
            &c,
            &cid,
            left,
            json!({ "state": "LEFT", "sourceVersion": 1 }),
        )
        .await;
    assert_eq!(r.json["applied"], true, "{}", r.text);
    assert_eq!(r.json["member"]["state"], "LEFT", "{}", r.text);
    let r = h
        .as_meeting(
            &c,
            Method::POST,
            &format!("/internal/v1/meeting-conversations/{cid}/legacy-imports"),
            Some(json!({ "legacySource": "meeting.conference-messages", "messages": [
                { "legacyMessageId": "x-1", "senderPrincipalId": left, "text": "bye", "sentAt": "2026-10-01T09:00:00Z" } ] })),
            None,
        )
        .await;
    assert_eq!(r.json["imported"], 1, "{}", r.text);
    // A former member reads nothing and cannot post.
    let read = h
        .get(&c, Some(left), &format!("/v1/conversations/{cid}/messages"))
        .await;
    assert!(read.status.is_client_error(), "{}", read.text);
    // No membership event announced a member who was never there.
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_outbox_events WHERE aggregate_id = '{cid}' \
             AND event_type IN ('messaging.member.added','messaging.member.removed') AND payload::text LIKE '%{left}%'"
        ))
        .await,
        0
    );
}

async fn relay_create(h: &Harness, c: &Customer, alice: Uuid) -> Response {
    h.create_conversation(
        c,
        alice,
        json!({ "contextClass": "GROUP", "securityProfile": "MANAGED",
                "context": { "namespace": "privacy-relay", "reference": "relay-context:7f3a" } }),
    )
    .await
}

async fn relay_apply(h: &Harness, c: &Customer, state: &str, v: i64) -> Response {
    let body = json!({ "namespace": "privacy-relay", "reference": "relay-context:7f3a", "state": state, "sourceVersion": v });
    h.call_internal(
        &peers().privacy_relay,
        Method::POST,
        "/internal/v1/context-authorities",
        Some(body),
        Some(delegated(c)),
        &[],
    )
    .await
}

async fn lifecycle_code(h: &Harness, cid: &str) -> i64 {
    h.scalar(&format!(
        "SELECT CASE lifecycle_state WHEN 'ACTIVE' THEN 1 WHEN 'LOCKED' THEN 2 WHEN 'CLOSED' THEN 3 ELSE 0 END::bigint FROM messaging_conversations WHERE id = '{cid}'"
    ))
    .await
}

#[tokio::test]
async fn privacy_relay_context_authority_gates_and_withdraws_conversations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // An EXTERNAL tenant's protected-communication context (opaque id only).
    let c = h.first_party(false);
    h.meeting_policy(&c, json!({})).await;
    let alice = h.principal(&c);
    let peers = peers();
    // Unknown to Privacy Relay → no conversation can claim the context.
    let r = relay_create(&h, &c, alice).await;
    assert_eq!(r.reason(), "CONTEXT_AUTHORITY_UNKNOWN", "{}", r.text);
    let r = relay_apply(&h, &c, "ACTIVE", 1).await;
    assert_eq!(r.json["applied"], true, "{}", r.text);
    let r = relay_create(&h, &c, alice).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let cid = r.json["conversationId"].as_str().unwrap().to_string();
    // SUSPENDED locks; a delayed older ACTIVE never unlocks it.
    assert_eq!(
        relay_apply(&h, &c, "SUSPENDED", 2).await.json["conversationsAffected"],
        1
    );
    assert_eq!(lifecycle_code(&h, &cid).await, 2);
    let stale = relay_apply(&h, &c, "ACTIVE", 1).await;
    assert_eq!(
        (
            stale.json["applied"].as_bool(),
            stale.json["reason"].as_str()
        ),
        (Some(false), Some("STALE_VERSION"))
    );
    assert_eq!(lifecycle_code(&h, &cid).await, 2);
    // A newer ACTIVE unlocks what the authority locked.
    relay_apply(&h, &c, "ACTIVE", 3).await;
    assert_eq!(lifecycle_code(&h, &cid).await, 1);
    // REVOKED closes it and it cannot be reopened or re-created while revoked.
    assert_eq!(
        relay_apply(&h, &c, "REVOKED", 4).await.json["applied"],
        true
    );
    assert_eq!(lifecycle_code(&h, &cid).await, 3);
    let v = h
        .scalar(&format!(
            "SELECT version::bigint FROM messaging_conversations WHERE id = '{cid}'"
        ))
        .await;
    let r = h
        .patch(
            &c,
            Some(alice),
            &format!("/v1/conversations/{cid}"),
            json!({ "expectedVersion": v, "lifecycleState": "ACTIVE" }),
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.text);
    assert_eq!(lifecycle_code(&h, &cid).await, 3);
    assert_eq!(
        relay_create(&h, &c, alice).await.reason(),
        "CONTEXT_AUTHORITY_WITHDRAWN"
    );
    // Only Privacy Relay speaks for its namespace; other namespaces are not controlled.
    let r = h
        .call_internal(&peers.meeting, Method::POST, "/internal/v1/context-authorities",
            Some(json!({ "namespace": "privacy-relay", "reference": "relay-context:7f3a", "state": "ACTIVE", "sourceVersion": 9 })),
            Some(delegated(&c)), &[])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = h
        .call_internal(&peers.privacy_relay, Method::POST, "/internal/v1/context-authorities",
            Some(json!({ "namespace": "meeting", "reference": "x", "state": "REVOKED", "sourceVersion": 9 })),
            Some(delegated(&c)), &[])
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    // No delegated scope → refused; nothing in the record names a contact value.
    let r = h
        .call_internal(&peers.privacy_relay, Method::POST, "/internal/v1/context-authorities",
            Some(json!({ "namespace": "privacy-relay", "reference": "relay-context:7f3a", "state": "REVOKED", "sourceVersion": 10 })),
            None, &[])
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_REQUIRED");
    // A suspended tenant still receives revocations, never grants.
    h.authority.delegated.lock().unwrap().insert(
        c.context.scope.environment_id,
        (c.context.scope, false, false),
    );
    assert_eq!(
        relay_apply(&h, &c, "REVOKED", 11).await.json["applied"],
        true
    );
    assert_eq!(
        relay_apply(&h, &c, "ACTIVE", 12).await.code(),
        "AUTHORITY_SUSPENDED"
    );
}

//! X4 `checkCommunicationRestrictions` (contract `communications/notification-owner/v1`):
//! `POST /internal/v1/communications/restrictions/check` for the Notification Service only
//! (`communications-restrictions.read`), in the signed first-party scope. Per recipient:
//! blocked (either direction), privacy-denied (the Hub's policy rule for the action) and
//! operator hold, from the Hub's own state.

mod common;
use axum::http::{HeaderMap, StatusCode};
use cf_communications_policy::Action;
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::*;
use communications::model::{Audience, Privacy, RequestPermission};
use serde_json::{json, Value};
use uuid::Uuid;

const ROUTE: &str = "/internal/v1/communications/restrictions/check";

fn unscoped(signer: &ServiceTokenSigner) -> HeaderMap {
    signer
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::communications(),
        ))
        .unwrap()
        .headers()
}

async fn restrictions(h: &Harness, body: Value) -> Vec<Value> {
    let (status, v) = h.http(&h.notifications, ROUTE, body, h.scope.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["results"], "{v}");
    v["results"].as_array().unwrap().clone()
}

/// `[blocked, privacyDenied, operatorHold]` of one result row (and its exact shape).
#[track_caller]
fn flags(row: &Value, reference: &str) -> [bool; 3] {
    assert_eq!(row["recipientRef"], reference, "{row}");
    assert_eq!(row.as_object().unwrap().len(), 4, "{row}");
    [
        row["blocked"].as_bool().unwrap(),
        row["privacyDenied"].as_bool().unwrap(),
        row["operatorHold"].as_bool().unwrap(),
    ]
}

#[tokio::test]
async fn only_the_notification_service_may_check_restrictions() {
    let h = Harness::new().await;
    let a = h.account();
    let b = h.account();
    h.profile(&a).await;
    h.profile(&b).await;
    let body = json!({"actorRef":a.account_reference,"recipientRefs":[b.account_reference],"action":"MESSAGE"});

    // No service token → 401.
    let (status, v) = h.http_headers(ROUTE, body.clone(), HeaderMap::new()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "AUTHENTICATION_REQUIRED");
    // Trusted peers without `communications-restrictions.read` → 403.
    for peer in [&h.account_signer, &h.calling_signer, &h.operator] {
        let (status, v) = h.http(peer, ROUTE, body.clone(), h.scope.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["error"]["code"], "PERMISSION_DENIED");
    }
    // The Notification Service must sign the first-party scope, like every scoped route.
    let (status, _) = h
        .http_headers(ROUTE, body.clone(), unscoped(&h.notifications))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The Notification Service holding the permission → 200, never cached.
    let (status, headers) = h
        .http_full(&h.notifications, ROUTE, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    // A replayed token is refused.
    let signed = h
        .notifications
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::communications())
                .with_delegated_context(h.scope.clone()),
        )
        .unwrap()
        .headers();
    let (status, _) = h.http_headers(ROUTE, body.clone(), signed.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.http_headers(ROUTE, body, signed).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    h.close().await;
}

#[tokio::test]
async fn batches_are_bounded_canonical_and_unique() {
    let h = Harness::new().await;
    let a = h.account();
    h.profile(&a).await;
    let r = || format!("cfmeet-user:{}", Uuid::new_v4());
    let one = r();
    let id = Uuid::new_v4();
    for bad in [
        json!({"actorRef":null,"recipientRefs":[],"action":"MESSAGE"}),
        json!({"actorRef":null,"recipientRefs":(0..101).map(|_| r()).collect::<Vec<_>>(),"action":"MESSAGE"}),
        json!({"actorRef":null,"recipientRefs":[one,one],"action":"MESSAGE"}),
        json!({"actorRef":null,"recipientRefs":["someone@example.com"],"action":"MESSAGE"}),
        json!({"actorRef":null,"recipientRefs":[format!("cfmeet-user:{}", id.simple())],"action":"MESSAGE"}),
        json!({"actorRef":null,"recipientRefs":[format!("cfmeet-user:{}", id.to_string().to_uppercase())],"action":"MESSAGE"}),
        json!({"actorRef":"cfmeet-user:nope","recipientRefs":[r()],"action":"MESSAGE"}),
        json!({"actorRef":a.account_reference,"recipientRefs":[r()],"action":"PRESENCE"}),
        json!({"actorRef":a.account_reference,"recipientRefs":[r()]}),
        json!({"actorRef":a.account_reference,"action":"MESSAGE"}),
        json!({"actorRef":a.account_reference,"recipientRefs":[r()],"action":"MESSAGE","extra":true}),
        json!({"actorRef":a.account_reference,"recipientRefs":"cfmeet-user:x","action":"MESSAGE"}),
    ] {
        let (status, v) = h
            .http(&h.notifications, ROUTE, bad.clone(), h.scope.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{bad}");
    }
    // Exactly 100 recipients (none with a profile) answer in request order; `actorRef`
    // may be omitted.
    let many: Vec<String> = (0..100).map(|_| r()).collect();
    let rows = restrictions(
        &h,
        json!({"recipientRefs":many,"action":"CONTACT_CONNECTED"}),
    )
    .await;
    assert_eq!(rows.len(), 100);
    for (row, reference) in rows.iter().zip(&many) {
        assert_eq!(flags(row, reference), [false, false, false]);
    }
    h.close().await;
}

#[tokio::test]
async fn blocks_privacy_and_operator_holds_follow_the_hub_rules() {
    let h = Harness::new().await;
    let actor = h.account();
    let contact = h.account(); // connected, default privacy
    let stranger = h.account(); // not connected
    let blocker = h.account(); // blocked the actor
    let blocked = h.account(); // blocked by the actor
    let held = h.account(); // connected, under an operator hold
    let ap = h.profile(&actor).await;
    let cp = h.profile(&contact).await;
    let sp = h.profile(&stranger).await;
    let bp = h.profile(&blocker).await;
    let dp = h.profile(&blocked).await;
    let hp = h.profile(&held).await;
    let unknown = format!("cfmeet-user:{}", Uuid::new_v4()); // no Hub profile
    h.connected(&ap, &cp).await;
    h.connected(&ap, &hp).await;
    h.connected(&ap, &bp).await;
    h.state
        .set_block(&bp, &ap, true, Uuid::new_v4())
        .await
        .unwrap();
    h.state
        .set_block(&ap, &dp, true, Uuid::new_v4())
        .await
        .unwrap();
    let (status, v) = h
        .http(
            &h.operator,
            "/internal/v1/communications/admin/suspend",
            json!({"cfMeetId":h.public_id(&hp).await,"active":true,"reason":"ABUSE"}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let refs = [
        contact.account_reference.clone(),
        stranger.account_reference.clone(),
        blocker.account_reference.clone(),
        blocked.account_reference.clone(),
        held.account_reference.clone(),
        unknown.clone(),
    ];
    let check = |action: &str, actor: Option<&str>| json!({"actorRef":actor,"recipientRefs":refs,"action":action});
    let table = |rows: Vec<Value>| -> Vec<[bool; 3]> {
        rows.iter()
            .zip(&refs)
            .map(|(row, reference)| flags(row, reference))
            .collect()
    };
    const F: bool = false;
    const T: bool = true;
    let me = Some(actor.account_reference.as_str());

    // CALL: direct calls need a connection and CONNECTIONS incoming-call policy.
    assert_eq!(
        table(restrictions(&h, check("CALL", me)).await),
        vec![
            [F, F, F], // contact
            [F, T, F], // stranger: not connected
            [T, T, F], // blocker: blocked (and the block revoked the connection)
            [T, T, F], // blocked by the actor: either direction
            [F, F, T], // held
            [F, T, F], // unknown: no profile, not connected
        ]
    );
    // MESSAGE: the direct-message notification rule (connection + direct-chat policy).
    assert_eq!(
        table(restrictions(&h, check("MESSAGE", me)).await),
        vec![
            [F, F, F],
            [F, T, F],
            [T, T, F],
            [T, T, F],
            [F, F, T],
            [F, T, F]
        ]
    );
    // CONTACT_REQUEST: anyone may ask while contact requests are open to EVERYONE.
    assert_eq!(
        table(restrictions(&h, check("CONTACT_REQUEST", me)).await),
        vec![
            [F, F, F],
            [F, F, F],
            [T, F, F],
            [T, F, F],
            [F, F, T],
            [F, F, F]
        ]
    );
    // GROUP_INVITATION: connection + open contact requests.
    assert_eq!(
        table(restrictions(&h, check("GROUP_INVITATION", me)).await),
        vec![
            [F, F, F],
            [F, T, F],
            [T, T, F],
            [T, T, F],
            [F, F, T],
            [F, T, F]
        ]
    );
    // CONTACT_CONNECTED and OTHER carry no privacy rule; blocks and holds still apply.
    for action in ["CONTACT_CONNECTED", "OTHER"] {
        assert_eq!(
            table(restrictions(&h, check(action, me)).await),
            vec![
                [F, F, F],
                [F, F, F],
                [T, F, F],
                [T, F, F],
                [F, F, T],
                [F, F, F]
            ],
            "{action}"
        );
    }
    // Without an actor only the recipient's operator hold remains.
    for action in ["CALL", "MESSAGE", "CONTACT_REQUEST", "GROUP_INVITATION"] {
        assert_eq!(
            table(restrictions(&h, check(action, None)).await),
            vec![
                [F, F, F],
                [F, F, F],
                [F, F, F],
                [F, F, F],
                [F, F, T],
                [F, F, F]
            ],
            "{action}"
        );
    }
    // An actor without a Hub profile: nothing is blocked, the pair is not connected.
    let ghost = format!("cfmeet-user:{}", Uuid::new_v4());
    let rows = table(restrictions(&h, check("CALL", Some(&ghost))).await);
    assert_eq!(rows[0], [F, T, F]);
    assert_eq!(rows[2], [F, T, F]);
    assert_eq!(rows[4], [F, T, T]);

    // The recipient's privacy settings decide privacyDenied.
    h.state
        .set_privacy(
            &cp,
            json!({"expectedVersion":1,"privacy":Privacy {
                incoming_call_policy: Audience::Nobody,
                ..Privacy::default()
            }}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    h.state
        .set_privacy(
            &sp,
            json!({"expectedVersion":1,"privacy":Privacy {
                contact_request_permissions: RequestPermission::Nobody,
                ..Privacy::default()
            }}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let call = table(restrictions(&h, check("CALL", me)).await);
    assert_eq!(call[0], [F, T, F], "contact refuses calls");
    let message = table(restrictions(&h, check("MESSAGE", me)).await);
    assert_eq!(message[0], [F, F, F], "direct chat is still allowed");
    let request = table(restrictions(&h, check("CONTACT_REQUEST", me)).await);
    assert_eq!(request[1], [F, T, F], "stranger refuses contact requests");
    assert_eq!(request[0], [F, F, F]);

    // The actor named among the recipients is never restricted against itself.
    let rows = restrictions(
        &h,
        json!({"actorRef":actor.account_reference,"recipientRefs":[actor.account_reference],"action":"CALL"}),
    )
    .await;
    assert_eq!(flags(&rows[0], &actor.account_reference), [F, F, F]);

    // Releasing the hold clears operatorHold.
    let (status, _) = h
        .http(
            &h.operator,
            "/internal/v1/communications/admin/suspend",
            json!({"cfMeetId":h.public_id(&hp).await,"active":false,"reason":"ABUSE"}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let call = table(restrictions(&h, check("CALL", me)).await);
    assert_eq!(call[4], [F, F, F]);

    // Parity with `policy/check` for the same pairs (owners report no domain block and
    // active accounts): allowed ⇔ no restriction.
    for (row, target) in call.iter().zip([&cp, &sp, &bp, &dp, &hp]) {
        let decision = h
            .state
            .check_policy(&h.scope, &common::check(Action::DirectCall, &ap, target))
            .await
            .unwrap();
        assert_eq!(decision.allowed, row.iter().all(|f| !f), "{row:?}");
    }

    // Read-only: no audit, outbox or change-feed row is written by the check.
    let count = |table: &'static str| {
        let h = &h;
        async move {
            communications::store::one::<_, Value>(
                &h.state.store.db,
                &format!("SELECT jsonb_build_object('n',count(*)) doc FROM {table}"),
                vec![],
            )
            .await
            .unwrap()
            .unwrap()["n"]
                .as_i64()
                .unwrap()
        }
    };
    let before = (
        count("communication_audit").await,
        count("communication_outbox").await,
        count("communication_changes").await,
    );
    restrictions(&h, check("MESSAGE", me)).await;
    let after = (
        count("communication_audit").await,
        count("communication_outbox").await,
        count("communication_changes").await,
    );
    assert_eq!(before, after);
    h.close().await;
}

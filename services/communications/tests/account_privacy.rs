//! S2: a deleted account's Hub data is erased (idempotently) and its data download reads
//! it back first.
mod common;
use common::*;
use communications::store::one;
use serde_json::{json, Value};
use uuid::Uuid;

async fn count(h: &Harness, sql: &str, id: Uuid) -> i64 {
    let v: Option<Value> = one(
        &h.state.store.db,
        &format!("SELECT to_jsonb(count(*)) doc FROM ({sql}) x"),
        vec![id.into()],
    )
    .await
    .unwrap();
    v.unwrap().as_i64().unwrap()
}

const ERASE: &str = "/internal/v1/communications/account-erasure";
const EXPORT: &str = "/internal/v1/communications/account-export";

#[tokio::test]
async fn account_erasure_removes_hub_data_and_export_reads_it_back() {
    let h = Harness::new().await;
    let a = h.account();
    let b = h.account();
    let pa = h.profile(&a).await;
    let pb = h.profile(&b).await;
    h.connected(&pa, &pb).await;
    let body = json!({"accountReference": a.account_reference});

    // The download first: the account's own Hub data.
    let (status, export) = h
        .http(&h.account_signer, EXPORT, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 200, "{export}");
    assert_eq!(export["schemaVersion"], 1);
    assert_eq!(
        export["data"]["cfMeetIds"].as_array().unwrap().len(),
        1,
        "{export}"
    );
    assert_eq!(
        export["data"]["contacts"].as_array().unwrap().len(),
        1,
        "{export}"
    );

    // Only the account authority's grant reaches it.
    let (status, _) = h
        .http(&h.calling_signer, ERASE, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 403);

    let (status, erased) = h
        .http(&h.account_signer, ERASE, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, 200, "{erased}");
    assert_eq!(erased["profile"], true);
    assert_eq!(erased["erased"]["cfMeetIds"], 1);
    assert_eq!(erased["erased"]["contacts"], 1);
    assert_eq!(
        count(
            &h,
            "SELECT 1 FROM communication_public_ids WHERE profile_id=$1",
            pa.id
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &h,
            "SELECT 1 FROM communication_contact_aliases WHERE owner_profile_id=$1",
            pa.id
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &h,
            "SELECT 1 FROM communication_connections WHERE (left_profile_id=$1 OR right_profile_id=$1) AND state='CONNECTED'",
            pa.id
        )
        .await,
        0
    );
    let profile: Value = one(
        &h.state.store.db,
        "SELECT to_jsonb(p) doc FROM communication_profiles p WHERE id=$1",
        vec![pa.id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(profile["activation_state"], "DEACTIVATED");
    assert_eq!(
        profile["display_name"],
        communications::privacy::ERASED_DISPLAY_NAME
    );
    assert!(profile["credentials_not_before"].is_string());
    // Calling and Messaging hear about it through the durable outbox.
    let fanned: Value = one(
        &h.state.store.db,
        "SELECT to_jsonb(count(*)) doc FROM communication_outbox WHERE event_type='communications.account.changed' AND payload->>'state'='DEACTIVATED'",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(fanned.as_i64().unwrap() >= 1);
    // The other person keeps their own profile and CF Meet ID.
    assert_eq!(
        count(
            &h,
            "SELECT 1 FROM communication_public_ids WHERE profile_id=$1",
            pb.id
        )
        .await,
        1
    );

    // Idempotent: nothing is left to erase.
    let (status, again) = h
        .http(&h.account_signer, ERASE, body, h.scope.clone())
        .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["erased"]["cfMeetIds"], 0);
    assert_eq!(again["erased"]["contacts"], 0);

    // An account that never opened the Hub has nothing to erase.
    let c = h.account();
    let (status, none) = h
        .http(
            &h.account_signer,
            ERASE,
            json!({"accountReference": c.account_reference}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(none["profile"], false);
    // Malformed references are refused.
    let (status, _) = h
        .http(
            &h.account_signer,
            ERASE,
            json!({"accountReference": "someone"}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, 400);
    h.close().await;
}

//! Contact names for the CF Meet client: the private Meeting-only lookup names only the
//! caller's CONNECTED, unblocked contacts, with the name the Hub contact list shows.

mod common;
use common::*;
use communications::store::execute;
use serde_json::{json, Value};
use uuid::Uuid;

const ROUTE: &str = "/internal/v1/communications/contacts/names";

fn named(body: &Value) -> Vec<(String, String, Option<String>)> {
    body["principals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["principalId"].as_str().unwrap().to_owned(),
                p["displayName"].as_str().unwrap().to_owned(),
                p.get("alias").and_then(Value::as_str).map(str::to_owned),
            )
        })
        .collect()
}

#[tokio::test]
async fn only_connected_unblocked_contacts_are_named() {
    let h = Harness::new().await;
    let me = h.account();
    let (friend, stranger, pending, blocked) = (h.account(), h.account(), h.account(), h.account());
    let mp = h.profile(&me).await;
    let fp = h.profile(&friend).await;
    let sp = h.profile(&stranger).await;
    let pp = h.profile(&pending).await;
    let bp = h.profile(&blocked).await;
    h.connected(&mp, &fp).await;
    h.connected(&bp, &mp).await; // connected from the other side
    h.state
        .send_request(
            &h.scope,
            &mp,
            json!({"cfMeetId": h.public_id(&pp).await}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let ids = json!([
        fp.principal_id,
        sp.principal_id,
        pp.principal_id,
        bp.principal_id,
        mp.principal_id,
        Uuid::new_v4()
    ]);
    let ask = |ids: Value| json!({"account": me, "principalIds": ids});

    // Connected contacts are named (either side of the connection); a stranger, a pending
    // request, the caller itself and an unknown id are left out without an error.
    let (status, body) = h
        .http(&h.account_signer, ROUTE, ask(ids.clone()), h.delegated(&me))
        .await;
    assert_eq!(status.as_u16(), 200, "{body}");
    assert_eq!(body["schemaVersion"], json!(1));
    let mut want = vec![
        (fp.principal_id.to_string(), fp.display_name.clone(), None),
        (bp.principal_id.to_string(), bp.display_name.clone(), None),
    ];
    want.sort();
    assert_eq!(named(&body), want);

    // The caller's private alias is returned with the contact's own display name.
    execute(
        &h.state.store.db,
        "UPDATE communication_contact_aliases SET alias='Best friend',version=version+1 \
          WHERE owner_profile_id=$1",
        vec![mp.id.into()],
    )
    .await
    .unwrap();
    let (_, body) = h
        .http(
            &h.account_signer,
            ROUTE,
            ask(json!([fp.principal_id])),
            h.delegated(&me),
        )
        .await;
    assert_eq!(
        named(&body),
        vec![(
            fp.principal_id.to_string(),
            fp.display_name.clone(),
            Some("Best friend".into())
        )]
    );

    // A block in either direction removes the name.
    h.state
        .set_block(&bp, &mp, true, Uuid::new_v4())
        .await
        .unwrap();
    let (_, body) = h
        .http(&h.account_signer, ROUTE, ask(ids.clone()), h.delegated(&me))
        .await;
    assert!(
        !named(&body)
            .iter()
            .any(|n| n.0 == bp.principal_id.to_string()),
        "{body}"
    );
    h.state
        .set_block(&bp, &mp, false, Uuid::new_v4())
        .await
        .unwrap();

    // The stranger sees nobody: the lookup is the caller's own contact list only.
    let (_, body) = h
        .http(
            &h.account_signer,
            ROUTE,
            json!({"account": stranger, "principalIds": [mp.principal_id, fp.principal_id]}),
            h.delegated(&stranger),
        )
        .await;
    assert_eq!(named(&body), vec![]);

    // Empty and oversized requests; unknown fields are refused.
    let (status, body) = h
        .http(&h.account_signer, ROUTE, ask(json!([])), h.delegated(&me))
        .await;
    assert_eq!(
        (status.as_u16(), body["principals"].clone()),
        (200, json!([]))
    );
    let many: Vec<Uuid> = (0..257).map(|_| Uuid::new_v4()).collect();
    let (status, _) = h
        .http(&h.account_signer, ROUTE, ask(json!(many)), h.delegated(&me))
        .await;
    assert_eq!(status.as_u16(), 400);
    let (status, _) = h
        .http(
            &h.account_signer,
            ROUTE,
            json!({"account": me, "principalIds": [], "extra": 1}),
            h.delegated(&me),
        )
        .await;
    assert_eq!(status.as_u16(), 400);

    // Only the Meeting Service may call it.
    let (status, _) = h
        .http(&h.calling_signer, ROUTE, ask(ids), h.delegated(&me))
        .await;
    assert_eq!(status.as_u16(), 403);
    h.close().await;
}

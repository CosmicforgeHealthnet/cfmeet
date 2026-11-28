//! Status audiences (Messaging only): the eligibility lookup lists exactly the actor's
//! MUTUAL, unblocked, active contacts, with the time eligibility last (re)started.

mod common;
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const ROUTE: &str = "/internal/v1/communications/contacts/eligible";

fn listed(body: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = body["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["principalId"].as_str().unwrap().to_owned(),
                c["eligibleSince"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    v.sort();
    v
}

fn ids(body: &Value) -> Vec<String> {
    listed(body).into_iter().map(|(p, _)| p).collect()
}

async fn alias_of(h: &Harness, owner: &communications::model::Profile) -> Uuid {
    h.state
        .store
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM communication_contact_aliases WHERE owner_profile_id=$1 ORDER BY created_at DESC LIMIT 1",
            vec![owner.id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<Uuid>("", "id")
        .unwrap()
}

#[tokio::test]
async fn only_mutual_unblocked_contacts_are_eligible() {
    let h = Harness::new().await;
    let me = h.account();
    let (friend, stranger, pending, blocked) = (h.account(), h.account(), h.account(), h.account());
    let mp = h.profile(&me).await;
    let fp = h.profile(&friend).await;
    let _sp = h.profile(&stranger).await;
    let pp = h.profile(&pending).await;
    let bp = h.profile(&blocked).await;
    h.connected(&mp, &fp).await;
    h.connected(&bp, &mp).await;
    // One-way: a request the other side never accepted.
    h.state
        .send_request(
            &h.scope,
            &mp,
            json!({"cfMeetId": h.public_id(&pp).await}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let ask = |filter: Option<Value>| match filter {
        Some(f) => json!({"actorPrincipalId": mp.principal_id, "principalIds": f}),
        None => json!({"actorPrincipalId": mp.principal_id}),
    };
    let (status, body) = h
        .http(&h.messaging_signer, ROUTE, ask(None), h.scope.clone())
        .await;
    assert_eq!(status.as_u16(), 200, "{body}");
    assert_eq!(body["schemaVersion"], json!(1));
    let mut want = vec![fp.principal_id.to_string(), bp.principal_id.to_string()];
    want.sort();
    assert_eq!(ids(&body), want);
    let before = listed(&body);

    // The pending side (either direction) is not eligible.
    let (_, body) = h
        .http(
            &h.messaging_signer,
            ROUTE,
            json!({"actorPrincipalId": pp.principal_id}),
            h.scope.clone(),
        )
        .await;
    assert!(ids(&body).is_empty(), "{body}");

    // A filter narrows; the actor itself and unknown ids are left out.
    let (_, body) = h
        .http(
            &h.messaging_signer,
            ROUTE,
            ask(Some(json!([
                fp.principal_id,
                mp.principal_id,
                Uuid::new_v4()
            ]))),
            h.scope.clone(),
        )
        .await;
    assert_eq!(ids(&body), vec![fp.principal_id.to_string()]);

    // A block in either direction removes eligibility; unblocking restores the contact
    // with a LATER eligibleSince (older statuses stay hidden).
    for (blocker, target) in [(&bp, &mp), (&mp, &bp)] {
        h.state
            .set_block(blocker, target, true, Uuid::new_v4())
            .await
            .unwrap();
        let (_, body) = h
            .http(&h.messaging_signer, ROUTE, ask(None), h.scope.clone())
            .await;
        assert_eq!(ids(&body), vec![fp.principal_id.to_string()], "{body}");
        h.state
            .set_block(blocker, target, false, Uuid::new_v4())
            .await
            .unwrap();
    }
    let (_, body) = h
        .http(&h.messaging_signer, ROUTE, ask(None), h.scope.clone())
        .await;
    let after = listed(&body);
    let since = |l: &[(String, String)], p: Uuid| {
        l.iter()
            .find(|(id, _)| *id == p.to_string())
            .map(|(_, s)| chrono::DateTime::parse_from_rfc3339(s).unwrap())
            .unwrap()
    };
    assert!(since(&after, bp.principal_id) > since(&before, bp.principal_id));
    assert_eq!(
        since(&after, fp.principal_id),
        since(&before, fp.principal_id)
    );

    // Removing the contact ends eligibility for both sides; re-adding starts it again later.
    let alias = alias_of(&h, &mp).await;
    let _ = h.state.remove_contact(&mp, alias, Uuid::new_v4()).await;
    let (_, body) = h
        .http(
            &h.messaging_signer,
            ROUTE,
            ask(Some(json!([fp.principal_id, bp.principal_id]))),
            h.scope.clone(),
        )
        .await;
    assert_eq!(ids(&body).len(), 1, "{body}");
    h.close().await;
}

#[tokio::test]
async fn only_messaging_may_ask() {
    let h = Harness::new().await;
    let me = h.account();
    let mp = h.profile(&me).await;
    let body = json!({"actorPrincipalId": mp.principal_id});
    // Calling holds the permission but is not the status owner.
    let (status, _) = h
        .http(&h.calling_signer, ROUTE, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status.as_u16(), 403);
    // The Meeting Service lacks the permission.
    let (status, _) = h
        .http(&h.account_signer, ROUTE, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status.as_u16(), 403);
    // Too many filter ids.
    let many: Vec<Uuid> = (0..257).map(|_| Uuid::new_v4()).collect();
    let (status, _) = h
        .http(
            &h.messaging_signer,
            ROUTE,
            json!({"actorPrincipalId": mp.principal_id, "principalIds": many}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status.as_u16(), 400);
    // An unknown actor has no contacts.
    let (status, body) = h
        .http(
            &h.messaging_signer,
            ROUTE,
            json!({"actorPrincipalId": Uuid::new_v4()}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(
        (status.as_u16(), body["contacts"].clone()),
        (200, json!([]))
    );
    h.close().await;
}

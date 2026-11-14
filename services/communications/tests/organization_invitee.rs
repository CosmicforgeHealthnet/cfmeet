//! Organization invitations by CF Meet ID: the private Meeting-only translation of a CF Meet
//! ID into the owning account follows exactly the consumer-discovery rules.

mod common;
use common::*;
use communications::store::execute;
use serde_json::json;
use uuid::Uuid;

const ROUTE: &str = "/internal/v1/communications/organization-invitations/resolve";

#[tokio::test]
async fn organization_invitee_resolution_follows_discovery_rules() {
    let h = Harness::new().await;
    let inviter = h.account();
    let invitee = h.account();
    let ip = h.profile(&inviter).await;
    let tp = h.profile(&invitee).await;
    let target_id = h.public_id(&tp).await;
    let req = |id: &str| json!({"account": inviter, "cfMeetId": id});

    // A discoverable, visible, active account resolves to its account reference.
    let (status, body) = h
        .http(
            &h.account_signer,
            ROUTE,
            req(&target_id),
            h.delegated(&inviter),
        )
        .await;
    assert_eq!(status.as_u16(), 200, "{body}");
    assert_eq!(body["accountReference"], json!(invitee.account_reference));
    assert!(body.get("self").is_none());

    // Surrounding whitespace is tolerated; unknown and malformed ids are 404.
    let (status, _) = h
        .http(
            &h.account_signer,
            ROUTE,
            req(&format!(" {target_id} ")),
            h.delegated(&inviter),
        )
        .await;
    assert_eq!(status.as_u16(), 200);
    for bad in ["cfm-does-not-exist", "", "x".repeat(300).as_str()] {
        let (status, body) = h
            .http(&h.account_signer, ROUTE, req(bad), h.delegated(&inviter))
            .await;
        assert_eq!(status.as_u16(), 404, "{bad}: {body}");
    }

    // The inviter's own id is flagged (the caller refuses self-invitations).
    let own = h.public_id(&ip).await;
    let (status, body) = h
        .http(&h.account_signer, ROUTE, req(&own), h.delegated(&inviter))
        .await;
    assert_eq!((status.as_u16(), body["self"].clone()), (200, json!(true)));

    // A target that is not discoverable is indistinguishable from an unknown id.
    execute(
        &h.state.store.db,
        "UPDATE communication_privacy SET public_id_discoverable=false,version=version+1 WHERE profile_id=$1",
        vec![tp.id.into()],
    )
    .await
    .unwrap();
    let (status, body) = h
        .http(
            &h.account_signer,
            ROUTE,
            req(&target_id),
            h.delegated(&inviter),
        )
        .await;
    assert_eq!(status.as_u16(), 404, "{body}");
    assert!(body.get("accountReference").is_none());
    execute(
        &h.state.store.db,
        "UPDATE communication_privacy SET public_id_discoverable=true,version=version+1 WHERE profile_id=$1",
        vec![tp.id.into()],
    )
    .await
    .unwrap();

    // A block in either direction hides the target.
    h.state
        .set_block(&tp, &ip, true, Uuid::new_v4())
        .await
        .unwrap();
    let (status, _) = h
        .http(
            &h.account_signer,
            ROUTE,
            req(&target_id),
            h.delegated(&inviter),
        )
        .await;
    assert_eq!(status.as_u16(), 404);
    h.state
        .set_block(&tp, &ip, false, Uuid::new_v4())
        .await
        .unwrap();

    // Only the Meeting Service may call it.
    let (status, _) = h
        .http(
            &h.calling_signer,
            ROUTE,
            req(&target_id),
            h.delegated(&inviter),
        )
        .await;
    assert!(matches!(status.as_u16(), 403), "{status}");
    h.close().await;
}

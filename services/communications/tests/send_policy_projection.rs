mod common;
use cf_communications_policy::Dispatch;
use common::*;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn block_confirmation_requires_the_matching_receiver_ack_including_idempotent_retry() {
    let mut h = Harness::new().await;
    let generation = Uuid::new_v4();
    let config = Arc::get_mut(&mut h.state.config).unwrap();
    config.synchronous_policy_events = true;
    config.send_policy_generation = Some(generation);
    let account = h.account();
    let actor = h.profile(&account).await;
    let target = h.profile(&h.account()).await;
    let public = h.public_id(&target).await;
    h.world.write().unwrap().policy_events_down = true;
    let key = Uuid::new_v4();
    let request = serde_json::to_value(Dispatch {
        method: "PUT".into(),
        path: format!("profiles/{public}/block"),
        query: Default::default(),
        body: None,
        idempotency_key: Some(key),
        account: account.clone(),
    })
    .unwrap();
    let path = "/internal/v1/communications/dispatch";
    let call = || {
        h.http(
            &h.account_signer,
            path,
            request.clone(),
            h.delegated(&account),
        )
    };
    assert_eq!(
        call().await.0,
        503,
        "an unacknowledged block is not confirmed"
    );
    assert!(
        h.state
            .store
            .blocked(&h.state.store.db, actor.id, target.id)
            .await
            .unwrap(),
        "the committed block and outbox survive the failed ACK"
    );
    {
        let mut world = h.world.write().unwrap();
        world.policy_events_down = false;
        world.policy_ack_generation = Some(Uuid::new_v4());
    }
    assert_eq!(
        call().await.0,
        503,
        "another generation cannot acknowledge this rollout"
    );
    h.world.write().unwrap().policy_ack_generation = Some(generation);
    assert_eq!(
        call().await.0,
        200,
        "same idempotency key completes only after delivery"
    );
    let events = h.world.read().unwrap().policy_events.clone();
    let blocks: Vec<_> = events
        .iter()
        .filter(|v| v["type"] == "communications.block.changed")
        .collect();
    assert_eq!(
        blocks.last().unwrap()["data"]["projectionGeneration"],
        json!(generation)
    );
    assert!(!blocks.is_empty());
    let count:serde_json::Value=communications::store::one(&h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_outbox WHERE destination='MESSAGING' AND event_type='communications.block.changed'",vec![]).await.unwrap().unwrap();
    assert_eq!(count["n"], 1, "retry does not create another mutation");
    h.close().await;
}

#[tokio::test]
async fn native_application_rule_projects_without_lazy_hub_profiles() {
    let mut h = Harness::new().await;
    let generation = Uuid::new_v4();
    let config = Arc::get_mut(&mut h.state.config).unwrap();
    config.synchronous_policy_events = true;
    config.send_policy_generation = Some(generation);
    h.world.write().unwrap().policy_ack_generation = Some(generation);
    let target = Uuid::new_v4();
    let body = json!({"eventId":Uuid::new_v4(),"actorPrincipalId":target,"targetPrincipalId":target,
        "sourceVersion":1,"blocked":true,"ruleId":Uuid::new_v4(),"applicationBlock":true,"projectionGeneration":generation});
    let (status, _) = h
        .http(
            &h.calling_signer,
            "/internal/v1/communications/block-events",
            body.clone(),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, 200);
    let (status, receipt) = h
        .http(
            &h.calling_signer,
            "/internal/v1/communications/block-events",
            body,
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(receipt["duplicate"], true);
    let events = h.world.read().unwrap().policy_events.clone();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["data"]["actorPrincipalId"],
        serde_json::Value::Null
    );
    assert_eq!(events[0]["data"]["targetPrincipalId"], json!(target));
    h.close().await;
}

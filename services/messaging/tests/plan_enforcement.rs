//! CF Meet user plan limits (ADR 0068 A6, brief §4): with `MESSAGING_PLAN_ENFORCEMENT`
//! on, an upload intent in CF Meet's own environment is capped by the uploader's plan
//! `attachmentBytes` (the existing `QUOTA_EXCEEDED`, reason `PLAN_ATTACHMENT_LIMIT`); the
//! plan never raises the policy's limit; an unavailable plan read, a non-account uploader,
//! another environment or enforcement off keep today's limits (fail open).

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use cf_object_storage::MemoryObjectStore;
use common::{flows::*, *};
use messaging::{
    conversations::policy::{register_baseline, ApplicationBaseline},
    plans::{PlanEnforcement, PlanLimits, PlanLimitsSource, PlanUnavailable},
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The Developer Platform's plan read, scripted per account (absent: unavailable).
#[derive(Default)]
struct Plans {
    by_account: Mutex<HashMap<Uuid, i64>>,
    calls: Mutex<Vec<(String, Uuid)>>,
}

#[async_trait]
impl PlanLimitsSource for Plans {
    async fn limits(&self, kind: &str, id: Uuid) -> Result<PlanLimits, PlanUnavailable> {
        self.calls.lock().unwrap().push((kind.to_string(), id));
        let cap = *self
            .by_account
            .lock()
            .unwrap()
            .get(&id)
            .ok_or(PlanUnavailable)?;
        Ok(serde_json::from_value(json!({
            "planCode": "FREE",
            "limits": { "attachmentBytes": cap, "storageBytes": null, "recording": false },
        }))
        .unwrap())
    }
}

fn body(size: i64) -> Value {
    json!({ "kind": "IMAGE", "size": size, "sha256": "ab".repeat(32), "mediaType": "image/png", "fileName": "p.png" })
}

#[tokio::test]
async fn the_uploaders_plan_caps_attachment_size_in_cf_meets_environment() {
    let plans = Arc::new(Plans::default());
    let store = Arc::new(MemoryObjectStore::new());
    let (s2, p2) = (store.clone(), plans.clone());
    let Some(h) = Harness::build(move |s| {
        s.storage = s2;
        s.plans = Some(Arc::new(PlanEnforcement::new(p2, Duration::from_secs(60))));
    })
    .await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED"], "defaultProfile": "MANAGED" } } }),
    )
    .await;
    let s = c.context.scope;
    register_baseline(
        h.conn(),
        s.tenant_id,
        s.application_id,
        s.environment_id,
        ApplicationBaseline::CfMeetFirstParty,
        h.now(),
    )
    .await
    .unwrap();
    // A CF Meet account on a plan capping attachments at 10 000 bytes, another account
    // whose plan read is unavailable, and a principal that is no CF Meet account.
    let capped = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let unknown = h.principal_of_kind(&c, "NATIVE_CF_USER");
    let other = h.principal(&c);
    let (account, account2) = (Uuid::new_v4(), Uuid::new_v4());
    {
        let mut native = h.principals.native.lock().unwrap();
        native.insert((s.environment_id, capped), format!("cfmeet-user:{account}"));
        native.insert(
            (s.environment_id, unknown),
            format!("cfmeet-user:{account2}"),
        );
    }
    plans.by_account.lock().unwrap().insert(account, 10_000);
    let conv = h
        .create_conversation(
            &c,
            capped,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await
        .json;
    let cid = uuid(&conv["conversationId"]);
    for p in [unknown, other] {
        let r = h
            .post(
                &c,
                Some(capped),
                &format!("/v1/conversations/{cid}/members"),
                json!({ "principalId": p }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let intent = |who: Uuid, size: i64| {
        let path = format!("/v1/conversations/{cid}/attachments");
        let h = &h;
        let c = &c;
        async move { h.post(c, Some(who), &path, body(size)).await }
    };

    let r = intent(capped, 10_001).await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::FORBIDDEN,
            "QUOTA_EXCEEDED",
            "PLAN_ATTACHMENT_LIMIT"
        ),
        "{}",
        r.text
    );
    let r = intent(capped, 10_000).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Fail open: the plan read is unavailable, or the uploader is no CF Meet account.
    let r = intent(unknown, 50_000).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = intent(other, 50_000).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Cached: one read per account.
    let calls = plans.calls.lock().unwrap().clone();
    assert_eq!(calls.iter().filter(|(_, id)| *id == account).count(), 1);
    assert!(calls.iter().all(|(kind, _)| kind == "USER"));

    // Another environment (no CF Meet baseline) is never capped by a plan.
    let partner = h.customer(ALL_SCOPES);
    h.set_policy(
        &partner,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED"], "defaultProfile": "MANAGED" } } }),
    )
    .await;
    let p = h.principal_of_kind(&partner, "NATIVE_CF_USER");
    h.principals.native.lock().unwrap().insert(
        (partner.context.scope.environment_id, p),
        format!("cfmeet-user:{account}"),
    );
    let pconv = h
        .create_conversation(
            &partner,
            p,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await
        .json;
    let r = h
        .post(
            &partner,
            Some(p),
            &format!(
                "/v1/conversations/{}/attachments",
                uuid(&pconv["conversationId"])
            ),
            body(50_000),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

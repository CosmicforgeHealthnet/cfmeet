//! INV-002: the HTTP contract of every webhook-migration operator route
//! (`/internal/v1/operator/webhook-migration/**`), route by route through the real router.
//!
//! The migration's semantics (history preservation, secrets, drift, shadow parity, the
//! takeover's de-duplication, rollback) are proven in `tests/webhook_migration.rs`; this
//! file pins what each route answers: 401 without a token, 403 for a peer without
//! `developer-platform.administer`, 400 OPERATOR_REQUIRED without a named operator, 503
//! when the capability or Calling is not configured, 400 for malformed input, 409 for a
//! command out of phase — and the success path of each route over a stopped, empty
//! Calling, in cutover order, with its audit row.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, Method, StatusCode};
use cf_webhooks::{
    endpoint::EndpointPolicy,
    secret_box::{SealingKey, SecretBox},
};
use common::*;
use developer_platform::webhooks::{
    migration::port::{CallingMigrationPort, Entity, ExportPage, SourceError},
    DeliveryMode, WebhookSettings,
};
use serde_json::{json, Value};
use uuid::Uuid;

const STATUS: &str = "/internal/v1/operator/webhook-migration/status";
const INVENTORY: &str = "/internal/v1/operator/webhook-migration/inventory";
const IMPORT: &str = "/internal/v1/operator/webhook-migration/import";
const RECONCILE: &str = "/internal/v1/operator/webhook-migration/reconcile";
const SHADOW_REPORT: &str = "/internal/v1/operator/webhook-migration/shadow-report";
const CUTOVER_CHECK: &str = "/internal/v1/operator/webhook-migration/cutover-check";
const ROTATIONS: &str = "/internal/v1/operator/webhook-migration/rotation-acknowledgements";
const TAKEOVER: &str = "/internal/v1/operator/webhook-migration/takeover";
const ROLLBACK_EXPORT: &str = "/internal/v1/operator/webhook-migration/rollback-export";
const ROLLBACK: &str = "/internal/v1/operator/webhook-migration/rollback";

/// Every operator route with a request that is valid for it.
fn routes() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (Method::GET, INVENTORY, None),
        (Method::POST, IMPORT, Some(json!({}))),
        (Method::GET, RECONCILE, None),
        (Method::GET, SHADOW_REPORT, None),
        (Method::GET, CUTOVER_CHECK, None),
        (
            Method::POST,
            ROTATIONS,
            Some(json!({ "webhookIds": [Uuid::new_v4()] })),
        ),
        (Method::POST, TAKEOVER, None),
        (Method::POST, ROLLBACK_EXPORT, None),
        (Method::POST, ROLLBACK, Some(json!({}))),
    ]
}

fn ring(kid: &str, seed: u8) -> Arc<SecretBox> {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(11));
    }
    Arc::new(SecretBox::new(SealingKey::from_bytes(kid, &k).unwrap(), None).unwrap())
}

fn settings(mode: DeliveryMode) -> WebhookSettings {
    WebhookSettings {
        enabled: true,
        key_ring: Some(ring("dp-routes-1", 17)),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        mode,
        request_timeout: std::time::Duration::from_secs(1),
        max_attempts: 3,
        auto_disable_after: 50,
        mutations_frozen: true,
        transfer_key: Some(ring("transfer-routes-1", 23)),
    }
}

/// A Calling with no webhook history whose delivery switch the test controls.
#[derive(Clone)]
struct EmptyCalling {
    delivering: Arc<Mutex<bool>>,
    rolled_back: Arc<Mutex<usize>>,
}

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[async_trait::async_trait]
impl CallingMigrationPort for EmptyCalling {
    async fn inventory(&self) -> Result<Value, SourceError> {
        let delivering = *self.delivering.lock().unwrap();
        Ok(json!({
            "generatedAt": "2026-11-09T10:00:00.000000Z",
            "flags": { "mutationsFrozen": !delivering, "deliveryEnabled": delivering,
                       "authority": "active", "signingKeyRingConfigured": true,
                       "transferKeyId": "transfer-routes-1" },
            "subscriptions": { "total": 0, "byStatus": {}, "unopenableSecrets": 0, "unopenableDeletedSecrets": 0 },
            "events": { "total": 0, "byProducer": {} },
            "deliveries": { "total": 0, "byStatus": {}, "open": 0, "inFlight": 0 },
            "attempts": { "total": 0 },
        }))
    }

    async fn digest(&self) -> Result<Value, SourceError> {
        let mut out = json!({ "algorithm": "sha256-canonical-lines-v1" });
        for e in Entity::ALL {
            out[e.as_str()] = json!({ "count": 0, "digest": EMPTY_SHA256 });
        }
        Ok(out)
    }

    async fn page(
        &self,
        _: Entity,
        _: Option<&str>,
        _: u32,
        _: bool,
    ) -> Result<ExportPage, SourceError> {
        Ok(ExportPage {
            data: vec![],
            next_cursor: None,
        })
    }

    async fn rollback_subscriptions(&self, items: Vec<Value>) -> Result<Value, SourceError> {
        *self.rolled_back.lock().unwrap() += items.len();
        Ok(json!({ "applied": items.len(), "unchanged": 0, "conflicts": [] }))
    }

    async fn settle_deliveries(&self, ids: &[Uuid]) -> Result<Value, SourceError> {
        Ok(json!({ "requested": ids.len(), "settled": ids.len() }))
    }
}

async fn world(mode: DeliveryMode, delivering: bool) -> Option<(Control, EmptyCalling, Webhooks)> {
    let calling = EmptyCalling {
        delivering: Arc::new(Mutex::new(delivering)),
        rolled_back: Arc::default(),
    };
    let port: Arc<dyn CallingMigrationPort> = Arc::new(calling.clone());
    let (c, w) =
        Control::with_webhooks_configured(settings(mode), move |s| s.with_calling_export(port))
            .await?;
    Some((c, calling, w))
}

async fn anonymous_operator(
    c: &Control,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Response {
    let signed = c
        .operator
        .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
            cf_service_auth::ServiceId::developer_platform(),
        ))
        .unwrap();
    let mut headers = HeaderMap::new();
    signed.apply_to(&mut headers);
    c.raw(method, path.to_string(), headers, body).await
}

async fn audits(c: &Control, action: &str) -> i64 {
    c.count(&format!(
        "SELECT count(*) FROM devplat_webhook_migration_audit WHERE action = '{action}'"
    ))
    .await
}

async fn all_audits(c: &Control) -> i64 {
    c.count("SELECT count(*) FROM devplat_webhook_migration_audit")
        .await
}

#[tokio::test]
async fn every_migration_route_requires_the_operator_permission_and_a_named_operator() {
    let Some((c, _, w)) = world(DeliveryMode::Active, false).await else {
        return;
    };
    // Trusted peers without developer-platform.administer: the gateway, and Calling
    // (which only publishes platform events here).
    let gateway = c.peers.gateway.clone();
    let calling = w.producers.calling.clone();
    for (method, path, body) in routes() {
        let r = c
            .raw(
                method.clone(),
                path.to_string(),
                HeaderMap::new(),
                body.clone(),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
        // A signature the trust bundle does not list under the gateway's name.
        let rogue = c.peers.rogue_gateway.clone();
        let r = c.internal(&rogue, method.clone(), path, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}: {}", r.json);
        for peer in [&gateway, &calling] {
            let r = c.internal(peer, method.clone(), path, body.clone()).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.json);
            assert_eq!(r.json["error"]["code"], "INTERNAL_PERMISSION_DENIED");
        }
        let r = anonymous_operator(&c, method.clone(), path, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "OPERATOR_REQUIRED");
    }
    assert_eq!(all_audits(&c).await, 0, "refused commands audit nothing");
    let r = c.operator(Method::GET, STATUS, None).await;
    assert!(
        r.json["phase"].is_null(),
        "no migration started: {}",
        r.json
    );
}

#[tokio::test]
async fn the_routes_fail_closed_without_central_webhooks_or_calling() {
    // Central webhooks not configured in this deployment.
    let Some(c) = Control::new().await else {
        return;
    };
    for (method, path, body) in routes() {
        let r = c.operator(method, path, body).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE", "{path}");
    }
    // Webhooks configured, but no Calling export (CF_CALLING_BASE_URL unset).
    let Some((c, _)) = Control::with_webhooks(settings(DeliveryMode::Active)).await else {
        return;
    };
    for (method, path) in [
        (Method::GET, INVENTORY),
        (Method::POST, IMPORT),
        (Method::GET, RECONCILE),
        (Method::GET, SHADOW_REPORT),
        (Method::GET, CUTOVER_CHECK),
        (Method::POST, TAKEOVER),
    ] {
        let r = c.operator(method, path, None).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE", "{path}");
    }
}

#[tokio::test]
async fn malformed_requests_and_out_of_phase_commands_are_refused() {
    let Some((c, _, _)) = world(DeliveryMode::Active, false).await else {
        return;
    };
    for query in ["pageSize=0", "pageSize=501", "pageSize=ten", "limit=5"] {
        let r = c
            .operator(Method::GET, &format!("{INVENTORY}?{query}"), None)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{query}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    for body in [
        json!({ "pageSize": "big" }),
        json!({ "pageSize": 0 }),
        json!({ "pageSize": 501 }),
        json!({ "resume": true }),
    ] {
        let r = c.operator(Method::POST, IMPORT, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    for query in [
        "since=yesterday",
        "since=2026-11-01",
        "until=2026-11-01T00:00:00Z",
    ] {
        let r = c
            .operator(Method::GET, &format!("{SHADOW_REPORT}?{query}"), None)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{query}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    let too_many: Vec<Uuid> = (0..501).map(|_| Uuid::new_v4()).collect();
    for body in [
        json!({}),
        json!({ "webhookIds": [] }),
        json!({ "webhookIds": ["wh_1"] }),
        json!({ "webhookIds": too_many }),
        json!({ "webhookIds": [Uuid::new_v4()], "all": true }),
    ] {
        let r = c.operator(Method::POST, ROTATIONS, Some(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    for body in [json!({ "acceptOrphans": "yes" }), json!({ "force": true })] {
        let r = c.operator(Method::POST, ROLLBACK, Some(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    // Out of phase: no completed import yet, nothing cut over.
    let r = c.operator(Method::POST, TAKEOVER, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
    let r = c.operator(Method::POST, ROLLBACK, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
    // Shadow mode never takes over.
    let Some((shadow, _, _)) = world(DeliveryMode::Shadow, false).await else {
        return;
    };
    shadow.operator(Method::POST, IMPORT, Some(json!({}))).await;
    let r = shadow.operator(Method::POST, TAKEOVER, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
}

#[tokio::test]
async fn each_route_succeeds_in_cutover_order_over_a_stopped_empty_calling() {
    let Some((c, calling, _)) = world(DeliveryMode::Active, true).await else {
        return;
    };
    // Dry run: writes nothing.
    let r = c
        .operator(Method::GET, &format!("{INVENTORY}?pageSize=50"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["dryRun"], true);
    for e in ["subscriptions", "events", "deliveries", "attempts"] {
        assert_eq!(r.json["wouldImport"][e]["seen"], 0, "{e}: {}", r.json);
    }
    assert_eq!(all_audits(&c).await, 0);

    // Import while Calling still delivers: completes, but the takeover is refused.
    let r = c
        .operator(Method::POST, IMPORT, Some(json!({ "pageSize": 10 })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);
    let r = c.operator(Method::GET, CUTOVER_CHECK, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["ready"], false, "Calling is not frozen: {}", r.json);
    assert_eq!(r.json["info"]["callingDeliveryEnabled"], true);
    let r = c.operator(Method::POST, TAKEOVER, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);

    // Calling stops; the final import is taken after that.
    *calling.delivering.lock().unwrap() = false;
    let r = c.operator(Method::POST, IMPORT, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);

    let r = c.operator(Method::GET, RECONCILE, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["clean"], true, "{}", r.json);

    let r = c
        .operator(
            Method::GET,
            &format!("{SHADOW_REPORT}?since=2026-01-01T00:00:00Z"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["parity"], true, "{}", r.json);
    assert_eq!(r.json["eventsComparedBothSides"], 0);
    let r = c.operator(Method::GET, SHADOW_REPORT, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(r.json["since"].is_null());

    // Acknowledging a subscription that needs no rotation acknowledges nothing (audited).
    let before = audits(&c, "webhook_migration.rotation_acknowledged").await;
    let r = c
        .operator(
            Method::POST,
            ROTATIONS,
            Some(json!({ "webhookIds": [Uuid::new_v4()] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json, json!({ "acknowledged": 0 }));
    assert_eq!(
        audits(&c, "webhook_migration.rotation_acknowledged").await,
        before + 1
    );

    let r = c.operator(Method::GET, CUTOVER_CHECK, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["ready"], true, "{}", r.json);
    let checks = r.json["checks"].as_array().unwrap();
    assert!(checks.iter().all(|c| c["ok"] == true), "{}", r.json);

    let r = c.operator(Method::POST, TAKEOVER, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["result"]["released"], 0, "{}", r.json);
    let r = c.operator(Method::GET, STATUS, None).await;
    assert_eq!(r.json["phase"], "CUT_OVER", "{}", r.json);

    // Nothing changed here since Calling last had it.
    let r = c.operator(Method::POST, ROLLBACK_EXPORT, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["candidates"], 0, "{}", r.json);
    assert_eq!(*calling.rolled_back.lock().unwrap(), 0);

    // The rollback is refused while Calling delivers again, then completes.
    *calling.delivering.lock().unwrap() = true;
    let r = c.operator(Method::POST, ROLLBACK, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    *calling.delivering.lock().unwrap() = false;
    let r = c
        .operator(
            Method::POST,
            ROLLBACK,
            Some(json!({ "acceptOrphans": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["phase"], "ROLLED_BACK", "{}", r.json);
    assert_eq!(r.json["completed"], true, "{}", r.json);
    let r = c.operator(Method::GET, STATUS, None).await;
    assert_eq!(r.json["phase"], "ROLLED_BACK", "{}", r.json);
    assert!(
        all_audits(&c).await >= 4,
        "state-changing commands are audited"
    );
}

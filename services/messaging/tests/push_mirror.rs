//! Roadmap N9 (D-1b copy path): Messaging's push-registration facade towards the
//! Notification Service (`push_mirror`), against real PostgreSQL and the real router.
//! The service's I13 is a fake with the same idempotency rules (its own behaviour is
//! tested in services/notifications/tests/push_facade.rs).
mod common;

use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use common::{flows::*, *};
use messaging::{
    identity::{AuthorityUnavailable, OwnerScope, PrincipalDirectory, PrincipalStatus},
    push_mirror::{
        interval_from_env, mirror_devices, reconcile, PushMirror, PushRegistrationSink,
        SyncFailure, SyncResult,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The service's I13, reduced to its idempotency rules; records every item it received.
#[derive(Default)]
struct FakeService {
    endpoints: Mutex<HashMap<Uuid, (String, String)>>,
    received: Mutex<Vec<Value>>,
    down: Mutex<bool>,
}

#[async_trait]
impl PushRegistrationSink for FakeService {
    async fn sync(&self, items: &[Value]) -> Result<Vec<SyncResult>, SyncFailure> {
        if *self.down.lock().unwrap() {
            return Err(SyncFailure::Unavailable);
        }
        let mut out = Vec::new();
        for i in items {
            self.received.lock().unwrap().push(i.clone());
            let device: Uuid = i["deviceId"].as_str().unwrap().parse().unwrap();
            let mut e = self.endpoints.lock().unwrap();
            let outcome = if i["enabled"] == true {
                let v = (
                    i["accountRef"].as_str().unwrap().to_string(),
                    i["handle"].as_str().unwrap().to_string(),
                );
                match e.insert(device, v.clone()) {
                    None => "CREATED",
                    Some(old) if old == v => "UNCHANGED",
                    Some(_) => "UPDATED",
                }
            } else {
                assert!(
                    i.get("handle").is_none(),
                    "a removal never carries the handle"
                );
                if e.remove(&device).is_some() {
                    "REMOVED"
                } else {
                    "ABSENT"
                }
            };
            out.push(SyncResult {
                device_id: device,
                outcome: outcome.into(),
                push_target_ref: None,
                reason: None,
            });
        }
        Ok(out)
    }
}

/// Principals of the personal scope with a CF Meet account (others have none).
struct Accounts {
    inner: Arc<FakePrincipals>,
    references: HashMap<Uuid, String>,
}

#[async_trait]
impl PrincipalDirectory for Accounts {
    async fn resolve(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        self.inner.resolve(scope, principal_id).await
    }

    async fn native_user_references(
        &self,
        _scope: &OwnerScope,
        principals: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, AuthorityUnavailable> {
        Ok(principals
            .iter()
            .filter_map(|p| self.references.get(p).map(|r| (*p, r.clone())))
            .collect())
    }
}

struct World {
    c: Customer,
    /// A CF Meet account with a push-registered device.
    alice: Member,
    alice_account: String,
    /// A principal of the same scope without a CF Meet account (not first-party).
    robot: Member,
    service: Arc<FakeService>,
    mirror: Arc<PushMirror>,
}

async fn register(h: &Harness, c: &Customer, m: &Member, handle: &str) {
    let r = h
        .put(
            c,
            Some(m.principal),
            &format!("/v1/messaging/devices/{}/push-registration", m.device_id()),
            json!({ "platform": "LOCAL_TEST", "handle": handle }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

async fn world(h: &mut Harness) -> World {
    let c = h.customer(ALL_SCOPES);
    let alice = h.member(&c, 0).await;
    let robot = h.member(&c, 0).await;
    let alice_account = format!("cfmeet-user:{}", Uuid::new_v4());
    h.state.principals = Arc::new(Accounts {
        inner: h.principals.clone(),
        references: HashMap::from([(alice.principal, alice_account.clone())]),
    });
    // A realtime client session of the scope: where a copy pass (no request context)
    // reads the authority references it asks Platform Access with.
    let s = c.context.scope;
    h.sql(&format!(
        "INSERT INTO messaging_client_sessions (id, tenant_id, application_id, environment_id, environment_kind, \
           service_account_id, principal_id, device_id, credential_id, scopes, status, created_at, expires_at) \
         VALUES ('{}', '{}', '{}', '{}', 'SANDBOX', '{}', '{}', '{}', '{}', 'messaging.realtime', 'ACTIVE', now(), now() + interval '1 hour')",
        Uuid::new_v4(), s.tenant_id, s.application_id, s.environment_id, s.service_account_id,
        alice.principal, alice.device_id(), c.context.credential_id
    ))
    .await
    .unwrap();
    // Another environment (a platform tenant): never read by the mirror.
    let other = h.customer(ALL_SCOPES);
    let stranger = h.member(&other, 0).await;
    register(h, &other, &stranger, "stranger-handle-0001").await;
    let service = Arc::new(FakeService::default());
    let mirror = Arc::new(PushMirror {
        scope: cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
        sink: service.clone(),
    });
    World {
        c,
        alice,
        alice_account,
        robot,
        service,
        mirror,
    }
}

async fn wait_for(service: &FakeService, n: usize) {
    for _ in 0..100 {
        if service.received.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "the facade sent {} item(s), expected {n}",
        service.received.lock().unwrap().len()
    );
}

#[test]
fn the_switch_defaults_to_off() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    };
    assert_eq!(interval_from_env(env(&[])), Ok(None));
    assert_eq!(
        interval_from_env(env(&[("MESSAGING_PUSH_REGISTRATION_MIRROR", "off")])),
        Ok(None)
    );
    assert_eq!(
        interval_from_env(env(&[("MESSAGING_PUSH_REGISTRATION_MIRROR", "on")])),
        Ok(Some(Duration::from_secs(60)))
    );
    assert!(interval_from_env(env(&[("MESSAGING_PUSH_REGISTRATION_MIRROR", "yes")])).is_err());
    assert!(interval_from_env(env(&[
        ("MESSAGING_PUSH_REGISTRATION_MIRROR", "on"),
        ("MESSAGING_PUSH_REGISTRATION_MIRROR_INTERVAL_SECONDS", "1")
    ]))
    .is_err());
}

#[tokio::test]
async fn switched_off_nothing_is_mirrored_and_registration_is_unchanged() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let w = world(&mut h).await;
    assert!(h.state.push_mirror.is_none());
    register(&h, &w.c, &w.alice, "alice-handle-0001").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(w.service.received.lock().unwrap().is_empty());
    assert_eq!(
        h.scalar("SELECT count(*)::bigint FROM messaging_push_registrations WHERE enabled AND push_handle = 'alice-handle-0001'")
            .await,
        1
    );
}

#[tokio::test]
async fn the_copy_mirrors_first_party_registrations_idempotently() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let w = world(&mut h).await;
    // Registrations made before the switch (the D-1b copy case).
    register(&h, &w.c, &w.alice, "alice-handle-0001").await;
    register(&h, &w.c, &w.robot, "robot-handle-0001").await;
    let first = reconcile(&h.state, &w.mirror, None).await.unwrap();
    assert_eq!(first.scanned, 2, "only the personal scope is read");
    assert_eq!((first.enabled, first.removals), (1, 1));
    assert_eq!(first.count("CREATED"), 1);
    assert_eq!(first.count("ABSENT"), 1);
    let received = w.service.received.lock().unwrap().clone();
    let alice = received
        .iter()
        .find(|i| i["deviceId"] == json!(w.alice.device_id()))
        .unwrap();
    assert_eq!(alice["accountRef"], w.alice_account);
    assert_eq!(alice["principalId"], json!(w.alice.principal));
    assert_eq!(alice["platform"], "LOCAL_TEST");
    assert_eq!(alice["handle"], "alice-handle-0001");
    assert_eq!(
        alice["environmentId"],
        json!(w.c.context.scope.environment_id)
    );
    // Not a CF Meet account: sent only as a removal, without its handle.
    let robot = received
        .iter()
        .find(|i| i["deviceId"] == json!(w.robot.device_id()))
        .unwrap();
    assert_eq!(robot["enabled"], false);
    assert!(robot.get("handle").is_none() && robot.get("accountRef").is_none());
    // Replaying the copy is idempotent (verification: counts stay equal).
    let again = reconcile(&h.state, &w.mirror, None).await.unwrap();
    assert_eq!(again.count("UNCHANGED"), 1);
    assert_eq!(again.count("CREATED"), 0);
    assert_eq!(w.service.endpoints.lock().unwrap().len(), 1);
    // An incremental pass reads only what changed since.
    let later = reconcile(
        &h.state,
        &w.mirror,
        Some(chrono::Utc::now() + chrono::Duration::minutes(1)),
    )
    .await
    .unwrap();
    assert_eq!(later.scanned, 0);
}

#[tokio::test]
async fn the_facade_follows_register_unregister_and_device_revocation() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let w = world(&mut h).await;
    h.state.push_mirror = Some(w.mirror.clone());
    register(&h, &w.c, &w.alice, "alice-handle-0001").await;
    wait_for(&w.service, 1).await;
    assert_eq!(
        w.service.endpoints.lock().unwrap()[&w.alice.device_id()].1,
        "alice-handle-0001"
    );
    // A changed handle.
    register(&h, &w.c, &w.alice, "alice-handle-0002").await;
    wait_for(&w.service, 2).await;
    assert_eq!(
        w.service.endpoints.lock().unwrap()[&w.alice.device_id()].1,
        "alice-handle-0002"
    );
    // Unregister: removed at the service.
    let r = h
        .delete(
            &w.c,
            Some(w.alice.principal),
            &format!(
                "/v1/messaging/devices/{}/push-registration",
                w.alice.device_id()
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    wait_for(&w.service, 3).await;
    assert!(w.service.endpoints.lock().unwrap().is_empty());
    // Re-registered, then the device is revoked: removed again.
    register(&h, &w.c, &w.alice, "alice-handle-0003").await;
    wait_for(&w.service, 4).await;
    let r = h
        .post(
            &w.c,
            Some(w.alice.principal),
            &format!("/v1/messaging/devices/{}/revoke", w.alice.device_id()),
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    wait_for(&w.service, 5).await;
    assert!(w.service.endpoints.lock().unwrap().is_empty());
    // Messaging's own registration rows behave exactly as before.
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*)::bigint FROM messaging_push_registrations WHERE device_id = '{}' AND NOT enabled AND disabled_reason = 'DEVICE_REVOKED'",
            w.alice.device_id()
        ))
        .await,
        1
    );
}

/// Captures every log line written while the guard lives (current-thread runtime).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_failed_pass_is_retried_and_never_logs_a_handle() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let w = world(&mut h).await;
    register(&h, &w.c, &w.alice, "alice-SECRET-handle").await;
    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    *w.service.down.lock().unwrap() = true;
    assert!(reconcile(&h.state, &w.mirror, None).await.is_err());
    *w.service.down.lock().unwrap() = false;
    let r = mirror_devices(&h.state, &w.mirror, &[w.alice.device_id()], None)
        .await
        .unwrap();
    assert_eq!(r.count("CREATED"), 1);
    drop(guard);
    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).to_string();
    assert!(
        text.contains("push-registration mirror batch not accepted"),
        "{text}"
    );
    assert!(!text.contains("SECRET"), "a handle reached the logs");
}

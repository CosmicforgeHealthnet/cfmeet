//! ADR 0058 Amendment A2: automatic organization service bindings, against an in-memory
//! Platform Access (`FakePlatformAccess`, the contract of
//! `/internal/v1/organization-scopes/{org}/primary/service-bindings/{service}`).
//!
//! Certified: with the flag on an organization is READY only after its Calling AND
//! Messaging bindings were created, read back and verified server-side, and scope
//! resolution then carries both; Platform Access unavailable backs off, then FAILS and an
//! operator retry recovers without a duplicate identity; a conflict and an unverified
//! read-back FAIL at once; a lost lease after Platform Access succeeded resumes
//! idempotently; archive stops resolving at once and the revocation reaches Platform
//! Access with retries; operator rotation; reconcile backfill, renewal and drift; an
//! operator binding is never replaced; suspension refuses resolution and reactivation
//! restores it. With the flag off nothing here runs (tests/provisioning.rs).

mod common;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use common::{s, Harness};
use organizations::provisioning::{self, BindingFacts, PlatformScopes, PortError, ScopeFacts};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{json, Value};
use uuid::Uuid;

type Key = (Uuid, String);

/// In-memory Platform Access with organization scopes and service bindings.
#[derive(Default)]
struct FakePlatformAccess {
    bindings_on: AtomicBool,
    scopes: Mutex<HashMap<Uuid, ScopeFacts>>,
    bindings: Mutex<HashMap<Key, BindingFacts>>,
    /// Service accounts created per (organization, service): must stay 1.
    creations: Mutex<HashMap<Key, usize>>,
    ensure_calls: Mutex<HashMap<Key, usize>>,
    binding_unavailable: AtomicBool,
    revoke_unavailable: AtomicBool,
    refuse_binding: Mutex<Option<(u16, String)>>,
    /// Read-back answers an unverified binding for these keys.
    tamper: Mutex<Vec<Key>>,
    /// The next ensure of these keys renews the credential (close to expiry).
    renew: Mutex<Vec<Key>>,
    /// Steals the workflow lease after the PUT of this key succeeded (a crash / lost
    /// lease after Platform Access already did its part).
    steal_lease_after: Mutex<Option<(DatabaseConnection, Key)>>,
}

impl FakePlatformAccess {
    fn on() -> Self {
        let pa = Self::default();
        pa.bindings_on.store(true, Ordering::SeqCst);
        pa
    }

    fn key(org: Uuid, service: &str) -> Key {
        (org, service.to_string())
    }

    fn creations(&self, org: Uuid, service: &str) -> usize {
        *self
            .creations
            .lock()
            .unwrap()
            .get(&Self::key(org, service))
            .unwrap_or(&0)
    }

    fn binding(&self, org: Uuid, service: &str) -> Option<BindingFacts> {
        self.bindings
            .lock()
            .unwrap()
            .get(&Self::key(org, service))
            .cloned()
    }

    fn scope(&self, org: Uuid) -> Option<ScopeFacts> {
        self.scopes.lock().unwrap().get(&org).cloned()
    }

    fn new_binding(scope: &ScopeFacts, service: &str) -> BindingFacts {
        let scopes: Vec<String> = provisioning::binding_scopes(service)
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        BindingFacts {
            organization_ref: scope.organization_ref,
            capability: "primary".into(),
            service: service.into(),
            tenant_id: scope.tenant_id,
            application_id: scope.application_id,
            environment_id: scope.environment_id,
            service_account_id: Uuid::new_v4(),
            credential_id: Uuid::new_v4(),
            required_scopes: scopes.clone(),
            scopes,
            service_account_status: "ACTIVE".into(),
            credential_status: "ACTIVE".into(),
            credential_expires_at: Some(Utc::now() + Duration::days(365)),
            authenticates: true,
            revision: 1,
            verified: true,
            active: true,
            revoked: false,
        }
    }
}

#[async_trait]
impl PlatformScopes for FakePlatformAccess {
    async fn provision(&self, org: Uuid, _capability: &str) -> Result<ScopeFacts, PortError> {
        let (t, a, e) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        Ok(self
            .scopes
            .lock()
            .unwrap()
            .entry(org)
            .or_insert_with(|| ScopeFacts {
                organization_ref: org,
                capability: "primary".into(),
                tenant_id: t,
                application_id: a,
                environment_id: e,
                tenant_classification: "FIRST_PARTY".into(),
                tenant_status: "ACTIVE".into(),
                tenant_revision: 1,
                environment_kind: "PRODUCTION".into(),
                verified: true,
                active: true,
            })
            .clone())
    }

    async fn read(&self, org: Uuid, _capability: &str) -> Result<Option<ScopeFacts>, PortError> {
        Ok(self.scope(org))
    }

    async fn set_status(
        &self,
        org: Uuid,
        _capability: &str,
        suspended: bool,
    ) -> Result<ScopeFacts, PortError> {
        let mut scopes = self.scopes.lock().unwrap();
        let f = scopes.get_mut(&org).ok_or(PortError::Refused {
            status: 404,
            code: "NOT_FOUND".into(),
        })?;
        f.tenant_status = if suspended { "SUSPENDED" } else { "ACTIVE" }.into();
        f.active = !suspended;
        Ok(f.clone())
    }

    fn bindings_enabled(&self) -> bool {
        self.bindings_on.load(Ordering::SeqCst)
    }

    async fn ensure_binding(
        &self,
        org: Uuid,
        _capability: &str,
        service: &str,
    ) -> Result<BindingFacts, PortError> {
        let key = Self::key(org, service);
        *self
            .ensure_calls
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default() += 1;
        if self.binding_unavailable.load(Ordering::SeqCst) {
            return Err(PortError::Unavailable("TRANSPORT".into()));
        }
        if let Some((status, code)) = self.refuse_binding.lock().unwrap().clone() {
            return Err(PortError::Refused { status, code });
        }
        let scope = self.scope(org).ok_or(PortError::Refused {
            status: 404,
            code: "NOT_FOUND".into(),
        })?;
        if scope.tenant_status != "ACTIVE" {
            return Err(PortError::Refused {
                status: 409,
                code: "CONFLICT".into(),
            });
        }
        let facts = {
            let mut bindings = self.bindings.lock().unwrap();
            let renew = {
                let mut r = self.renew.lock().unwrap();
                let hit = r.contains(&key);
                r.retain(|k| *k != key);
                hit
            };
            match bindings.get_mut(&key) {
                Some(b) if b.revoked => {
                    return Err(PortError::Refused {
                        status: 409,
                        code: "CONFLICT".into(),
                    })
                }
                Some(b) => {
                    if renew {
                        b.credential_id = Uuid::new_v4();
                        b.revision += 1;
                    }
                    b.clone()
                }
                None => {
                    *self
                        .creations
                        .lock()
                        .unwrap()
                        .entry(key.clone())
                        .or_default() += 1;
                    let b = Self::new_binding(&scope, service);
                    bindings.insert(key.clone(), b.clone());
                    b
                }
            }
        };
        let hook = {
            let mut steal = self.steal_lease_after.lock().unwrap();
            match steal.as_ref() {
                Some((_, k)) if *k == key => steal.take(),
                _ => None,
            }
        };
        if let Some((db, _)) = hook {
            db.execute_unprepared(&format!(
                "UPDATE organization_provisioning SET lease_id = gen_random_uuid() WHERE organization_id = '{org}'"
            ))
            .await
            .unwrap();
        }
        Ok(facts)
    }

    async fn read_binding(
        &self,
        org: Uuid,
        _capability: &str,
        service: &str,
    ) -> Result<Option<BindingFacts>, PortError> {
        if self.binding_unavailable.load(Ordering::SeqCst) {
            return Err(PortError::Unavailable("TRANSPORT".into()));
        }
        let key = Self::key(org, service);
        let mut b = self.bindings.lock().unwrap().get(&key).cloned();
        if self.tamper.lock().unwrap().contains(&key) {
            if let Some(b) = b.as_mut() {
                b.scopes.push("numbers.admin".into());
                b.verified = false;
            }
        }
        Ok(b)
    }

    async fn rotate_binding(
        &self,
        org: Uuid,
        _capability: &str,
        service: &str,
    ) -> Result<BindingFacts, PortError> {
        let mut bindings = self.bindings.lock().unwrap();
        let b = bindings
            .get_mut(&Self::key(org, service))
            .ok_or(PortError::Refused {
                status: 404,
                code: "NOT_FOUND".into(),
            })?;
        b.credential_id = Uuid::new_v4();
        b.revision += 1;
        Ok(b.clone())
    }

    async fn revoke_binding(
        &self,
        org: Uuid,
        _capability: &str,
        service: &str,
    ) -> Result<Option<BindingFacts>, PortError> {
        if self.revoke_unavailable.load(Ordering::SeqCst) {
            return Err(PortError::Unavailable("TRANSPORT".into()));
        }
        let mut bindings = self.bindings.lock().unwrap();
        Ok(bindings.get_mut(&Self::key(org, service)).map(|b| {
            b.revoked = true;
            b.active = false;
            b.authenticates = false;
            b.credential_status = "REVOKED".into();
            b.service_account_status = "REVOKED".into();
            b.revision += 1;
            b.clone()
        }))
    }
}

async fn run(h: &Harness, pa: &FakePlatformAccess) -> provisioning::ProvisioningReport {
    provisioning::run_once(&h.db.conn, pa).await.unwrap()
}

async fn read(h: &Harness, org: &str) -> Value {
    h.by(&h.peers.messaging)
        .get(&format!("/internal/v1/organizations/{org}/provisioning"))
        .await
        .expect(200)
        .clone()
}

async fn resolve(h: &Harness, org: &str) -> common::Response {
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/scopes/resolve",
            json!({ "organizationId": org }),
        )
        .await
}

async fn retry(h: &Harness, org: &str) -> common::Response {
    h.by(&h.peers.operator)
        .post(
            &format!("/internal/v1/admin/organizations/{org}/provisioning/retry"),
            json!({}),
        )
        .await
}

fn binding_path(org: &str, service: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/scopes/primary/service-bindings/{service}")
}

async fn get_binding(h: &Harness, org: &str, service: &str) -> common::Response {
    h.by(&h.peers.operator)
        .get(&binding_path(org, service))
        .await
}

async fn due_now(h: &Harness) {
    h.exec("UPDATE organization_provisioning SET next_attempt_at = now(), lease_until = CASE WHEN lease_id IS NULL THEN NULL ELSE now() - interval '1 second' END WHERE state IN ('PENDING', 'IN_PROGRESS')").await.unwrap();
    h.exec("UPDATE organization_platform_scopes SET next_sync_at = now() WHERE next_sync_at IS NOT NULL").await.unwrap();
    h.exec("UPDATE organization_scope_service_bindings SET revocation_next_at = now() WHERE revocation_next_at IS NOT NULL").await.unwrap();
}

async fn checks_due(h: &Harness) {
    h.exec("UPDATE organization_platform_scopes SET bindings_next_check_at = now()")
        .await
        .unwrap();
}

fn uuid(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn ready_only_after_both_bindings_are_verified_and_resolve_carries_them() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::on();
    let org = s(&h.create_org(Uuid::new_v4(), "bind-auto-a").await["id"]);
    let id = uuid(&json!(org));

    // Platform Access refuses the bindings for now: the scope is verified, the
    // organization is NOT ready and resolution refuses.
    pa.binding_unavailable.store(true, Ordering::SeqCst);
    let r = run(&h, &pa).await;
    assert_eq!((r.ready, r.retried), (0, 1), "{r:?}");
    let p = read(&h, &org).await;
    assert_eq!(p["provisioningState"], "IN_PROGRESS");
    assert_eq!(p["step"], "SCOPE_VERIFIED");
    assert_eq!(p["lastErrorClass"], "PLATFORM_ACCESS_UNAVAILABLE");
    assert_eq!(p["scopes"][0]["mappingState"], "READY");
    resolve(&h, &org)
        .await
        .expect_error(409, "PROVISIONING_PENDING");

    // Recovery: both bindings created, read back, verified, recorded; then READY.
    pa.binding_unavailable.store(false, Ordering::SeqCst);
    due_now(&h).await;
    let r = run(&h, &pa).await;
    assert_eq!(r.ready, 1, "{r:?}");
    let p = read(&h, &org).await;
    assert_eq!(p["provisioningState"], "READY");
    assert_eq!(p["step"], "VERIFIED");
    let v = resolve(&h, &org).await.expect(200).clone();
    for service in ["calling", "messaging"] {
        let b = pa.binding(id, service).unwrap();
        assert_eq!(
            uuid(&v["serviceBindings"][service]["serviceAccountId"]),
            b.service_account_id
        );
        assert_eq!(
            uuid(&v["serviceBindings"][service]["credentialId"]),
            b.credential_id
        );
        assert_eq!(pa.creations(id, service), 1);
        let g = get_binding(&h, &org, service).await.expect(200).clone();
        assert_eq!(g["source"], "AUTOMATIC");
        assert_eq!(g["version"], 1);
        assert!(g["verifiedAt"].is_string());
        assert!(g["credentialExpiresAt"].is_string());
        assert_eq!(g["boundBy"], json!(null));
    }
    // Ids only, audited once each; one provisioned event; the scope's mapping event once.
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.service_binding_provisioned' AND organization_id = '{org}' AND actor_kind = 'SYSTEM'")).await, 2);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = 'organization.provisioned' AND organization_id = '{org}'")).await, 1);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = 'scope.mapping_changed' AND organization_id = '{org}'")).await, 1);
    assert_eq!(
        h.count(
            "SELECT count(*) FROM organization_audit_events WHERE summary::text ILIKE '%secret%'"
        )
        .await,
        0
    );
    // Another pass changes nothing.
    let r = run(&h, &pa).await;
    assert_eq!(r, provisioning::ProvisioningReport::default());
}

#[tokio::test]
async fn unavailable_backs_off_then_fails_and_a_retry_recovers_without_duplicates() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::on();
    let org = s(&h.create_org(Uuid::new_v4(), "bind-outage").await["id"]);
    let id = uuid(&json!(org));
    // The scope steps succeed once; the bindings are unavailable for every attempt.
    pa.binding_unavailable.store(true, Ordering::SeqCst);
    for attempt in 1..=provisioning::MAX_ATTEMPTS {
        let r = run(&h, &pa).await;
        if attempt < provisioning::MAX_ATTEMPTS {
            assert_eq!(r.retried, 1, "attempt {attempt}: {r:?}");
            due_now(&h).await;
        } else {
            assert_eq!(r.failed, 1, "{r:?}");
        }
    }
    let p = read(&h, &org).await;
    assert_eq!(p["provisioningState"], "FAILED");
    assert_eq!(p["lastErrorClass"], "PLATFORM_ACCESS_UNAVAILABLE");
    // The verified scope is kept and its tenant is NOT suspended (nothing is wrong
    // with it).
    assert_eq!(p["scopes"][0]["mappingState"], "READY");
    assert_eq!(pa.scope(id).unwrap().tenant_status, "ACTIVE");
    resolve(&h, &org)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    pa.binding_unavailable.store(false, Ordering::SeqCst);
    retry(&h, &org).await.expect(200);
    let r = run(&h, &pa).await;
    assert_eq!(r.ready, 1, "{r:?}");
    resolve(&h, &org).await.expect(200);
    for service in ["calling", "messaging"] {
        assert_eq!(pa.creations(id, service), 1, "{service}");
    }
    // The retry did not re-emit the (unchanged) scope mapping.
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = 'scope.mapping_changed' AND organization_id = '{org}'")).await, 1);
}

#[tokio::test]
async fn conflict_and_unverified_read_back_fail_at_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::on();
    *pa.refuse_binding.lock().unwrap() = Some((409, "CONFLICT".into()));
    let a = s(&h.create_org(Uuid::new_v4(), "bind-conflict").await["id"]);
    let r = run(&h, &pa).await;
    assert_eq!(r.failed, 1, "{r:?}");
    let p = read(&h, &a).await;
    assert_eq!(p["provisioningState"], "FAILED");
    assert_eq!(p["lastErrorClass"], "SERVICE_BINDING_CONFLICT");
    assert_eq!(p["attempts"], 1);
    *pa.refuse_binding.lock().unwrap() = Some((403, "INTERNAL_PERMISSION_DENIED".into()));
    retry(&h, &a).await.expect(200);
    run(&h, &pa).await;
    assert_eq!(
        read(&h, &a).await["lastErrorClass"],
        "PLATFORM_ACCESS_REFUSED"
    );
    *pa.refuse_binding.lock().unwrap() = None;

    let b = s(&h.create_org(Uuid::new_v4(), "bind-tamper").await["id"]);
    pa.tamper
        .lock()
        .unwrap()
        .push(FakePlatformAccess::key(uuid(&json!(b)), "messaging"));
    let r = run(&h, &pa).await;
    assert_eq!(r.failed, 1, "{r:?}");
    let p = read(&h, &b).await;
    assert_eq!(p["lastErrorClass"], "SERVICE_BINDING_VERIFICATION_FAILED");
    // The Calling binding verified before was recorded; Messaging was not.
    get_binding(&h, &b, "calling").await.expect(200);
    get_binding(&h, &b, "messaging")
        .await
        .expect_error(404, "NOT_FOUND");
    resolve(&h, &b)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    // Fixed at Platform Access: an operator retry completes it.
    pa.tamper.lock().unwrap().clear();
    retry(&h, &b).await.expect(200);
    assert_eq!(run(&h, &pa).await.ready, 1);
    resolve(&h, &b).await.expect(200);
}

#[tokio::test]
async fn a_lost_lease_after_platform_access_succeeded_resumes_idempotently() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::on();
    let org = s(&h.create_org(Uuid::new_v4(), "bind-lease").await["id"]);
    let id = uuid(&json!(org));
    *pa.steal_lease_after.lock().unwrap() =
        Some((h.db.conn.clone(), FakePlatformAccess::key(id, "messaging")));
    let r = run(&h, &pa).await;
    assert_eq!(r.lease_lost, 1, "{r:?}");
    // Calling was recorded (checkpoint); Messaging exists in Platform Access only.
    assert_eq!(read(&h, &org).await["step"], "BINDING_CALLING_VERIFIED");
    get_binding(&h, &org, "messaging")
        .await
        .expect_error(404, "NOT_FOUND");
    due_now(&h).await;
    let r = run(&h, &pa).await;
    assert_eq!(r.ready, 1, "{r:?}");
    for service in ["calling", "messaging"] {
        assert_eq!(pa.creations(id, service), 1, "{service}");
        let g = get_binding(&h, &org, service).await.expect(200).clone();
        assert_eq!(
            uuid(&g["credentialId"]),
            pa.binding(id, service).unwrap().credential_id
        );
        assert_eq!(g["version"], 1);
    }
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.service_binding_provisioned' AND organization_id = '{org}'")).await, 2);
}

#[tokio::test]
async fn archive_stops_resolution_at_once_and_revocation_reaches_platform_access() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::on();
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bind-archive").await["id"]);
    let id = uuid(&json!(org));
    assert_eq!(run(&h, &pa).await.ready, 1);
    resolve(&h, &org).await.expect(200);
    pa.revoke_unavailable.store(true, Ordering::SeqCst);
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/archive"),
            json!({}),
        )
        .await
        .expect(200);
    // Immediately: no binding is resolvable or readable any more.
    resolve(&h, &org)
        .await
        .expect_error(409, "ORGANIZATION_ARCHIVED");
    for service in ["calling", "messaging"] {
        get_binding(&h, &org, service)
            .await
            .expect_error(404, "NOT_FOUND");
    }
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_scope_service_bindings WHERE organization_id = '{org}' AND revoked_at IS NOT NULL AND revocation_applied_at IS NULL")).await, 2);
    // Platform Access down: retried with backoff, never abandoned.
    let r = run(&h, &pa).await;
    assert_eq!(r.revocations_failed, 2, "{r:?}");
    assert!(!pa.binding(id, "calling").unwrap().revoked);
    let r = run(&h, &pa).await;
    assert_eq!(r.revocations_failed, 0, "backoff: {r:?}");
    pa.revoke_unavailable.store(false, Ordering::SeqCst);
    due_now(&h).await;
    let r = run(&h, &pa).await;
    assert_eq!(r.revocations_applied, 2, "{r:?}");
    for service in ["calling", "messaging"] {
        assert!(pa.binding(id, service).unwrap().revoked, "{service}");
    }
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.service_binding_revoked' AND organization_id = '{org}'")).await, 2);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.service_binding_revocation_applied' AND organization_id = '{org}'")).await, 2);
    // Nothing is recreated for an archived organization.
    checks_due(&h).await;
    let r = run(&h, &pa).await;
    assert_eq!((r.bindings_checked, r.revocations_applied), (0, 0), "{r:?}");
}

#[tokio::test]
async fn suspension_refuses_resolution_and_reactivation_restores_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::on();
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bind-suspend").await["id"]);
    let id = uuid(&json!(org));
    assert_eq!(run(&h, &pa).await.ready, 1);
    let before = resolve(&h, &org).await.expect(200).clone();
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    resolve(&h, &org)
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    run(&h, &pa).await;
    // The tenant is suspended (every credential fails); bindings are kept, not revoked.
    assert_eq!(pa.scope(id).unwrap().tenant_status, "SUSPENDED");
    assert!(!pa.binding(id, "calling").unwrap().revoked);
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/reactivate"),
            json!({}),
        )
        .await
        .expect(200);
    // Not before the tenant is resumed in Platform Access.
    resolve(&h, &org)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    run(&h, &pa).await;
    let after = resolve(&h, &org).await.expect(200).clone();
    assert_eq!(after["serviceBindings"], before["serviceBindings"]);
}

#[tokio::test]
async fn operator_rotation_records_the_new_credential() {
    let pa = Arc::new(FakePlatformAccess::on());
    let Some(h) = Harness::with_platform(pa.clone()).await else {
        return;
    };
    let org = s(&h.create_org(Uuid::new_v4(), "bind-rotate").await["id"]);
    let id = uuid(&json!(org));
    assert_eq!(run(&h, &pa).await.ready, 1);
    let before = pa.binding(id, "calling").unwrap();
    let path = format!("{}/rotate", binding_path(&org, "calling"));
    // Only organization.administer.
    h.by(&h.peers.messaging)
        .post(&path, json!({}))
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    let r = h
        .by(&h.peers.operator)
        .post(&path, json!({}))
        .await
        .expect(200)
        .clone();
    let after = pa.binding(id, "calling").unwrap();
    assert_ne!(after.credential_id, before.credential_id);
    assert_eq!(uuid(&r["credentialId"]), after.credential_id);
    assert_eq!(uuid(&r["serviceAccountId"]), before.service_account_id);
    assert_eq!(r["version"], 2);
    assert_eq!(r["source"], "AUTOMATIC");
    let v = resolve(&h, &org).await.expect(200).clone();
    assert_eq!(
        uuid(&v["serviceBindings"]["calling"]["credentialId"]),
        after.credential_id
    );
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.service_binding_rotated' AND organization_id = '{org}' AND actor_kind = 'OPERATOR'")).await, 1);
    // An operator binding is replaced with PUT, never rotated here.
    h.by(&h.peers.operator)
        .send(
            axum::http::Method::PUT,
            &binding_path(&org, "messaging"),
            Some(json!({ "serviceAccountId": Uuid::new_v4(), "credentialId": Uuid::new_v4() })),
        )
        .await
        .expect(200);
    h.by(&h.peers.operator)
        .post(
            &format!("{}/rotate", binding_path(&org, "messaging")),
            json!({}),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // Unknown services look like a missing binding.
    h.by(&h.peers.operator)
        .post(
            &format!("{}/rotate", binding_path(&org, "telephony")),
            json!({}),
        )
        .await
        .expect_error(404, "NOT_FOUND");
}

#[tokio::test]
async fn operator_rotation_is_refused_while_automatic_bindings_are_off() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = s(&h.create_org(Uuid::new_v4(), "bind-rotate-off").await["id"]);
    h.by(&h.peers.operator)
        .post(
            &format!("{}/rotate", binding_path(&org, "calling")),
            json!({}),
        )
        .await
        .expect_error(503, "SERVICE_UNAVAILABLE");
}

#[tokio::test]
async fn reconcile_backfills_renews_detects_drift_and_respects_operator_bindings() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Organizations made READY while automatic bindings were off.
    let pa = FakePlatformAccess::default();
    let a = s(&h.create_org(Uuid::new_v4(), "bind-backfill").await["id"]);
    let b = s(&h.create_org(Uuid::new_v4(), "bind-manual").await["id"]);
    let (ia, ib) = (uuid(&json!(a)), uuid(&json!(b)));
    assert_eq!(run(&h, &pa).await.ready, 2);
    // An operator bound b's Calling by hand.
    let (manual_sa, manual_cred) = (Uuid::new_v4(), Uuid::new_v4());
    h.by(&h.peers.operator)
        .send(
            axum::http::Method::PUT,
            &binding_path(&b, "calling"),
            Some(json!({ "serviceAccountId": manual_sa, "credentialId": manual_cred })),
        )
        .await
        .expect(200);
    assert_eq!(
        resolve(&h, &a).await.expect(200)["serviceBindings"],
        json!({ "calling": null, "messaging": null })
    );

    // Automation switched on: the next pass backfills (organizations stay READY).
    pa.bindings_on.store(true, Ordering::SeqCst);
    let r = run(&h, &pa).await;
    assert_eq!(r.bindings_checked, 2, "{r:?}");
    let va = resolve(&h, &a).await.expect(200).clone();
    assert_eq!(
        uuid(&va["serviceBindings"]["messaging"]["credentialId"]),
        pa.binding(ia, "messaging").unwrap().credential_id
    );
    assert!(va["serviceBindings"]["calling"].is_object());
    // b: the operator binding is untouched; Messaging was added automatically.
    let gb = get_binding(&h, &b, "calling").await.expect(200).clone();
    assert_eq!(gb["source"], "OPERATOR");
    assert_eq!(uuid(&gb["credentialId"]), manual_cred);
    assert_eq!(pa.creations(ib, "calling"), 0);
    assert_eq!(
        get_binding(&h, &b, "messaging").await.expect(200)["source"],
        "AUTOMATIC"
    );
    // Not due again before the reconciliation interval.
    assert_eq!(run(&h, &pa).await.bindings_checked, 0);

    // Renewal: Platform Access issued a successor credential for a's Calling.
    pa.renew
        .lock()
        .unwrap()
        .push(FakePlatformAccess::key(ia, "calling"));
    checks_due(&h).await;
    let r = run(&h, &pa).await;
    assert_eq!(r.bindings_checked, 2, "{r:?}");
    let g = get_binding(&h, &a, "calling").await.expect(200).clone();
    assert_eq!(g["version"], 2);
    assert_eq!(
        uuid(&g["credentialId"]),
        pa.binding(ia, "calling").unwrap().credential_id
    );
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.service_binding_rotated' AND organization_id = '{a}' AND actor_kind = 'SYSTEM'")).await, 1);

    // Drift: a's Messaging binding no longer verifies → removed locally, a FAILED.
    pa.tamper
        .lock()
        .unwrap()
        .push(FakePlatformAccess::key(ia, "messaging"));
    checks_due(&h).await;
    let r = run(&h, &pa).await;
    assert_eq!(r.bindings_drifted, 1, "{r:?}");
    let p = read(&h, &a).await;
    assert_eq!(p["provisioningState"], "FAILED");
    assert_eq!(p["lastErrorClass"], "SERVICE_BINDING_DRIFT");
    resolve(&h, &a)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    get_binding(&h, &a, "messaging")
        .await
        .expect_error(404, "NOT_FOUND");
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'provisioning.drift_detected' AND organization_id = '{a}'")).await, 1);
    // Fixed at Platform Access; an operator retry re-provisions the same identity.
    pa.tamper.lock().unwrap().clear();
    retry(&h, &a).await.expect(200);
    assert_eq!(run(&h, &pa).await.ready, 1);
    resolve(&h, &a).await.expect(200);
    assert_eq!(pa.creations(ia, "messaging"), 1);

    // An operator removing an automatic binding revokes it; it is not recreated.
    h.by(&h.peers.operator)
        .send(
            axum::http::Method::DELETE,
            &binding_path(&a, "messaging"),
            None,
        )
        .await
        .expect(200);
    let r = run(&h, &pa).await;
    assert_eq!(r.revocations_applied, 1, "{r:?}");
    assert!(pa.binding(ia, "messaging").unwrap().revoked);
    checks_due(&h).await;
    run(&h, &pa).await;
    assert_eq!(
        resolve(&h, &a).await.expect(200)["serviceBindings"]["messaging"],
        json!(null)
    );
    assert_eq!(pa.creations(ia, "messaging"), 1);
}

//! Section 7: the Organizations ↔ Platform Access provisioning saga against an in-memory
//! Platform Access (`FakePlatformAccess`, the same contract as the real
//! `/internal/v1/organization-scopes/**`).
//!
//! Certified failure modes: duplicate create requests; partial Platform Access success
//! with a local database failure; suspension during provisioning; duplicate / out-of-order
//! events (no duplicate emission, monotonic versions, drift); Platform Access unavailable
//! (capped backoff, explicit FAILED, operator retry); replay; failed compensation. Two
//! organizations get distinct triples, no split-brain mapping is ever recorded, READY only
//! after server-side verification, and provisioning is OFF by default.

mod common;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

use async_trait::async_trait;
use common::{s, Harness};
use organizations::provisioning::{self, PlatformScopes, PortError, ScopeFacts};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{json, Value};
use uuid::Uuid;

/// In-memory Platform Access: idempotent provisioning keyed by organization, read-back,
/// tenant status, and switchable failures.
#[derive(Default)]
struct FakePlatformAccess {
    scopes: Mutex<HashMap<Uuid, ScopeFacts>>,
    provision_calls: AtomicUsize,
    status_calls: AtomicUsize,
    unavailable: AtomicBool,
    status_unavailable: AtomicBool,
    refuse: Mutex<Option<(u16, String)>>,
    /// Read-back answers EXTERNAL for these organizations (a forged / wrong triple).
    tamper_reads: Mutex<Vec<Uuid>>,
    /// Provisioning returns this triple for an organization instead of its own.
    forced_triple: Mutex<HashMap<Uuid, (Uuid, Uuid, Uuid)>>,
    /// Suspends this organization in the Organizations database while provisioning
    /// (an operator acting concurrently).
    suspend_during_provision: Mutex<Option<(DatabaseConnection, Uuid)>>,
}

impl FakePlatformAccess {
    fn new_facts(org: Uuid, triple: Option<(Uuid, Uuid, Uuid)>) -> ScopeFacts {
        let (t, a, e) = triple.unwrap_or_else(|| (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()));
        ScopeFacts {
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
        }
    }

    fn tenant_status(&self, org: Uuid) -> Option<String> {
        self.scopes
            .lock()
            .unwrap()
            .get(&org)
            .map(|f| f.tenant_status.clone())
    }

    fn triple(&self, org: Uuid) -> (Uuid, Uuid, Uuid) {
        let f = self.scopes.lock().unwrap().get(&org).cloned().unwrap();
        (f.tenant_id, f.application_id, f.environment_id)
    }
}

#[async_trait]
impl PlatformScopes for FakePlatformAccess {
    async fn provision(&self, org: Uuid, capability: &str) -> Result<ScopeFacts, PortError> {
        assert_eq!(capability, "primary");
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(PortError::Unavailable("TRANSPORT".into()));
        }
        if let Some((status, code)) = self.refuse.lock().unwrap().clone() {
            return Err(PortError::Refused { status, code });
        }
        self.provision_calls.fetch_add(1, Ordering::SeqCst);
        let hook = self.suspend_during_provision.lock().unwrap().take();
        if let Some((db, target)) = hook {
            db.execute_unprepared(&format!(
                "UPDATE organizations SET lifecycle_status = 'SUSPENDED', lifecycle_reason = 'OPERATOR_ACTION', version = version + 1 WHERE id = '{target}'"
            ))
            .await
            .unwrap();
        }
        let forced = self.forced_triple.lock().unwrap().get(&org).copied();
        let mut scopes = self.scopes.lock().unwrap();
        Ok(scopes
            .entry(org)
            .or_insert_with(|| Self::new_facts(org, forced))
            .clone())
    }

    async fn read(&self, org: Uuid, _capability: &str) -> Result<Option<ScopeFacts>, PortError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(PortError::Unavailable("TRANSPORT".into()));
        }
        let mut f = self.scopes.lock().unwrap().get(&org).cloned();
        if self.tamper_reads.lock().unwrap().contains(&org) {
            if let Some(f) = f.as_mut() {
                f.tenant_classification = "EXTERNAL".into();
                f.verified = false;
            }
        }
        Ok(f)
    }

    async fn set_status(
        &self,
        org: Uuid,
        _capability: &str,
        suspended: bool,
    ) -> Result<ScopeFacts, PortError> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) || self.status_unavailable.load(Ordering::SeqCst)
        {
            return Err(PortError::Unavailable("TRANSPORT".into()));
        }
        let mut scopes = self.scopes.lock().unwrap();
        let f = scopes.get_mut(&org).ok_or(PortError::Refused {
            status: 404,
            code: "NOT_FOUND".into(),
        })?;
        f.tenant_status = if suspended { "SUSPENDED" } else { "ACTIVE" }.into();
        f.active = !suspended;
        f.tenant_revision += 1;
        Ok(f.clone())
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

/// Makes every workflow / sync due now (instead of waiting for the backoff).
async fn due_now(h: &Harness) {
    h.exec("UPDATE organization_provisioning SET next_attempt_at = now() WHERE state IN ('PENDING', 'IN_PROGRESS')").await.unwrap();
    h.exec("UPDATE organization_platform_scopes SET next_sync_at = now() WHERE next_sync_at IS NOT NULL").await.unwrap();
}

async fn retry(h: &Harness, org: &str) -> common::Response {
    h.by(&h.peers.operator)
        .post(
            &format!("/internal/v1/admin/organizations/{org}/provisioning/retry"),
            json!({}),
        )
        .await
}

fn uuid(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn provisioning_is_off_by_default_and_reported_truthfully() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "off-org").await["id"]);
    // The maintenance pass alone never provisions anything.
    organizations::worker::run_once(&h.db.conn, &organizations::outbox::DisabledDispatcher)
        .await
        .unwrap();
    let p = read(&h, &org).await;
    assert_eq!(p["provisioningState"], "PENDING");
    assert_eq!(p["provisioningEnabled"], false);
    assert_eq!(p["workflowId"], json!(null));
    assert_eq!(p["scopes"], json!([]));
    assert_eq!(
        h.count("SELECT count(*) FROM organization_provisioning")
            .await,
        0
    );
    // Nothing organization-scoped is advertised before READY.
    resolve(&h, &org)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    let ready = h
        .raw(axum::http::Method::GET, "/ready", Default::default(), None)
        .await;
    assert_eq!(ready.expect(200)["provisioning"]["enabled"], false);
}

#[tokio::test]
async fn two_organizations_get_distinct_triples_ready_only_after_server_side_verification() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::default();
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    let a = s(&h.create_org(alice, "prov-a").await["id"]);
    let b = s(&h.create_org(bob, "prov-b").await["id"]);
    resolve(&h, &a)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    let r = run(&h, &pa).await;
    assert_eq!((r.enqueued, r.ready), (2, 2), "{r:?}");
    for org in [&a, &b] {
        let p = read(&h, org).await;
        assert_eq!(p["provisioningState"], "READY");
        assert_eq!(p["workflowState"], "READY");
        assert_eq!(p["step"], "VERIFIED");
        let scope = &p["scopes"][0];
        assert_eq!(scope["mappingState"], "READY");
        assert_eq!(scope["tenantStatusDesired"], "ACTIVE");
        assert!(scope["verifiedAt"].is_string());
        let (t, ap, e) = pa.triple(Uuid::parse_str(org).unwrap());
        assert_eq!(uuid(&scope["tenantId"]), t);
        let resolved = resolve(&h, org).await;
        let v = resolved.expect(200);
        assert_eq!(uuid(&v["tenantId"]), t);
        assert_eq!(uuid(&v["applicationId"]), ap);
        assert_eq!(uuid(&v["environmentId"]), e);
    }
    let (ta, tb) = (pa.triple(uuid(&json!(a))), pa.triple(uuid(&json!(b))));
    assert_ne!(ta.0, tb.0);
    assert_ne!(ta.1, tb.1);
    assert_ne!(ta.2, tb.2);
    // Events and audit: one provisioned / mapping change per organization.
    for (event, n) in [
        ("organization.provisioned", 2),
        ("scope.mapping_changed", 2),
    ] {
        assert_eq!(
            h.count(&format!(
                "SELECT count(*) FROM organization_outbox WHERE event_type = '{event}'"
            ))
            .await,
            n,
            "{event}"
        );
    }
    assert_eq!(h.count("SELECT count(*) FROM organization_audit_events WHERE action = 'provisioning.ready' AND actor_kind = 'SYSTEM'").await, 2);
    // A service with a membership check: the user must be an ACTIVE member.
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/scopes/resolve",
            json!({ "organizationId": a, "userId": bob }),
        )
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/scopes/resolve",
            json!({ "organizationId": a, "userId": alice }),
        )
        .await
        .expect(200);
    // A read-back that does not verify (EXTERNAL / unverified) never becomes READY, and
    // the recorded tenant is suspended (compensation).
    let c_owner = Uuid::new_v4();
    let c = s(&h.create_org(c_owner, "prov-c").await["id"]);
    pa.tamper_reads.lock().unwrap().push(uuid(&json!(c)));
    let r = run(&h, &pa).await;
    assert_eq!(r.failed, 1, "{r:?}");
    let p = read(&h, &c).await;
    assert_eq!(p["provisioningState"], "FAILED");
    assert_eq!(p["lastErrorClass"], "SCOPE_VERIFICATION_FAILED");
    assert_eq!(p["scopes"][0]["mappingState"], "FAILED");
    assert_eq!(p["scopes"][0]["tenantStatusDesired"], "SUSPENDED");
    resolve(&h, &c)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    run(&h, &pa).await;
    assert_eq!(
        pa.tenant_status(uuid(&json!(c))).as_deref(),
        Some("SUSPENDED")
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_platform_scopes WHERE mapping_state = 'READY'")
            .await,
        2
    );
}

#[tokio::test]
async fn duplicate_requests_and_replays_create_one_workflow_one_scope_and_no_duplicate_events() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::default();
    let owner = Uuid::new_v4();
    // Duplicate create requests (same Idempotency-Key) create one organization.
    let body = json!({ "name": "Dup", "slug": "dup-org" });
    let first = h
        .user(owner)
        .idem("create-1")
        .post("/internal/v1/organizations", body.clone())
        .await;
    let again = h
        .user(owner)
        .idem("create-1")
        .post("/internal/v1/organizations", body)
        .await;
    assert_eq!(first.expect(201)["id"], again.expect(201)["id"]);
    let org = s(&first.json["id"]);
    // Concurrent enqueues create one workflow (stable idempotency key).
    let (x, y) = tokio::join!(
        provisioning::enqueue(&h.db.conn, 10),
        provisioning::enqueue(&h.db.conn, 10)
    );
    assert_eq!(x.unwrap() + y.unwrap(), 1);
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_provisioning WHERE organization_id = '{org}' AND idempotency_key = 'org-scope:{org}:primary'")).await,
        1
    );
    run(&h, &pa).await;
    assert_eq!(read(&h, &org).await["provisioningState"], "READY");
    let events = h.count("SELECT count(*) FROM organization_outbox").await;
    // Replayed passes and operator retries of a READY organization change nothing.
    for _ in 0..3 {
        due_now(&h).await;
        run(&h, &pa).await;
    }
    for _ in 0..2 {
        let r = retry(&h, &org).await;
        assert_eq!(r.expect(200)["provisioningState"], "READY");
    }
    assert_eq!(pa.provision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(pa.scopes.lock().unwrap().len(), 1);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_outbox").await,
        events
    );
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE event_type = 'organization.provisioned'").await, 1);
    // Versions are monotonic per provisioning / scope aggregate (consumers apply by
    // aggregateVersion and ignore duplicates).
    assert_eq!(
        h.count("SELECT count(*) FROM (SELECT aggregate_id, aggregate_version FROM organization_outbox WHERE aggregate_type IN ('provisioning', 'scope') GROUP BY 1, 2 HAVING count(*) > 1) d").await,
        0
    );
    // The retry route needs organization.administer.
    h.by(&h.peers.messaging)
        .post(
            &format!("/internal/v1/admin/organizations/{org}/provisioning/retry"),
            json!({}),
        )
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    retry(&h, &Uuid::new_v4().to_string())
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
}

#[tokio::test]
async fn platform_access_unavailable_backs_off_then_fails_and_an_operator_retry_recovers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::default();
    pa.unavailable.store(true, Ordering::SeqCst);
    let org = s(&h.create_org(Uuid::new_v4(), "outage-org").await["id"]);
    let mut delays = Vec::new();
    for attempt in 1..=provisioning::MAX_ATTEMPTS {
        let r = run(&h, &pa).await;
        let p = read(&h, &org).await;
        if attempt < provisioning::MAX_ATTEMPTS {
            assert_eq!(r.retried, 1, "attempt {attempt}: {r:?}");
            assert_eq!(p["workflowState"], "IN_PROGRESS");
            assert_eq!(p["attempts"], attempt);
            assert_eq!(p["lastErrorClass"], "PLATFORM_ACCESS_UNAVAILABLE");
            delays.push(
                h.count(&format!("SELECT extract(epoch FROM next_attempt_at - updated_at)::bigint FROM organization_provisioning WHERE organization_id = '{org}'")).await,
            );
            // Not due again until the backoff elapsed.
            let idle = run(&h, &pa).await;
            assert_eq!(idle.retried, 0);
            due_now(&h).await;
        } else {
            assert_eq!(r.failed, 1, "{r:?}");
            assert_eq!(p["provisioningState"], "FAILED");
            assert_eq!(p["workflowState"], "FAILED");
            assert_eq!(p["lastErrorClass"], "PLATFORM_ACCESS_UNAVAILABLE");
        }
    }
    // Capped exponential backoff.
    assert!(delays.windows(2).all(|w| w[1] >= w[0]), "{delays:?}");
    assert_eq!(delays[0], provisioning::backoff_seconds(1));
    assert!(delays.iter().all(|d| *d <= 900));
    resolve(&h, &org)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    // Platform Access recovers: FAILED stays FAILED until an operator retries.
    pa.unavailable.store(false, Ordering::SeqCst);
    run(&h, &pa).await;
    assert_eq!(read(&h, &org).await["provisioningState"], "FAILED");
    let r = retry(&h, &org).await;
    assert_eq!(r.expect(200)["provisioningState"], "PENDING");
    assert_eq!(r.json["attempts"], 0);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'provisioning.retry_requested' AND organization_id = '{org}' AND actor_kind = 'OPERATOR'")).await, 1);
    run(&h, &pa).await;
    assert_eq!(read(&h, &org).await["provisioningState"], "READY");
    resolve(&h, &org).await.expect(200);
    // A refusal (409 CONFLICT from Platform Access) fails at once, without retries.
    *pa.refuse.lock().unwrap() = Some((409, "CONFLICT".into()));
    let other = s(&h.create_org(Uuid::new_v4(), "conflict-org").await["id"]);
    let r = run(&h, &pa).await;
    assert_eq!(r.failed, 1);
    let p = read(&h, &other).await;
    assert_eq!(p["lastErrorClass"], "PLATFORM_SCOPE_CONFLICT");
    assert_eq!(p["attempts"], 1);
}

#[tokio::test]
async fn partial_platform_access_success_with_a_local_failure_resumes_with_the_same_triple() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::default();
    let org = s(&h.create_org(Uuid::new_v4(), "partial-org").await["id"]);
    let org_id = uuid(&json!(org));
    // Local database failure right after Platform Access created the scope: the
    // checkpoint cannot record the mapping.
    h.exec("CREATE FUNCTION test_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected failure'; END $$")
        .await
        .unwrap();
    h.exec("CREATE TRIGGER test_refuse_mapping BEFORE INSERT ON organization_platform_scopes FOR EACH ROW EXECUTE FUNCTION test_refuse()")
        .await
        .unwrap();
    let r = run(&h, &pa).await;
    assert_eq!(r.local_errors, 1, "{r:?}");
    assert_eq!(pa.provision_calls.load(Ordering::SeqCst), 1);
    let created = pa.triple(org_id);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_platform_scopes")
            .await,
        0
    );
    assert_ne!(read(&h, &org).await["provisioningState"], "READY");
    // The database recovers; the lease expires; the saga resumes and Platform Access
    // (idempotent on the organization) returns the SAME triple.
    h.exec("DROP TRIGGER test_refuse_mapping ON organization_platform_scopes")
        .await
        .unwrap();
    h.exec("UPDATE organization_provisioning SET lease_until = now() - interval '1 second'")
        .await
        .unwrap();
    let r = run(&h, &pa).await;
    assert_eq!(r.ready, 1, "{r:?}");
    assert_eq!(pa.provision_calls.load(Ordering::SeqCst), 2);
    assert_eq!(pa.scopes.lock().unwrap().len(), 1);
    let v = resolve(&h, &org).await;
    assert_eq!(uuid(&v.expect(200)["tenantId"]), created.0);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_platform_scopes")
            .await,
        1
    );
}

#[tokio::test]
async fn suspension_during_provisioning_and_lifecycle_follow_the_tenant_fail_closed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::default();
    let owner = Uuid::new_v4();
    // (a) Suspended after the request, before Platform Access was called: nothing is
    // created while suspended; reactivation makes the workflow due at once.
    let a = s(&h.create_org(owner, "susp-a").await["id"]);
    provisioning::enqueue(&h.db.conn, 10).await.unwrap();
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{a}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    let r = run(&h, &pa).await;
    assert_eq!(r.waiting, 1, "{r:?}");
    assert_eq!(pa.provision_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        read(&h, &a).await["step"],
        "WAITING_FOR_ACTIVE_ORGANIZATION"
    );
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{a}/reactivate"),
            json!({}),
        )
        .await
        .expect(200);
    assert_eq!(run(&h, &pa).await.ready, 1);
    resolve(&h, &a).await.expect(200);

    // (b) Suspended concurrently while Platform Access creates the scope: the mapping is
    // recorded SUSPENDED and the tenant is suspended; resolution refuses until the
    // organization is reactivated AND the tenant resumed.
    let b = s(&h.create_org(owner, "susp-b").await["id"]);
    *pa.suspend_during_provision.lock().unwrap() = Some((h.db.conn.clone(), uuid(&json!(b))));
    let r = run(&h, &pa).await;
    assert_eq!(r.ready, 1, "{r:?}");
    let p = read(&h, &b).await;
    assert_eq!(p["provisioningState"], "READY");
    assert_eq!(p["scopes"][0]["mappingState"], "SUSPENDED");
    assert_eq!(p["scopes"][0]["tenantStatusDesired"], "SUSPENDED");
    run(&h, &pa).await;
    assert_eq!(
        pa.tenant_status(uuid(&json!(b))).as_deref(),
        Some("SUSPENDED")
    );
    resolve(&h, &b)
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    h.by(&h.peers.operator)
        .patch(
            &format!("/internal/v1/admin/organizations/{b}"),
            json!({ "status": "ACTIVE" }),
        )
        .await
        .expect(200);
    // Reactivated locally, tenant not yet resumed: still not advertised.
    resolve(&h, &b)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    run(&h, &pa).await;
    assert_eq!(pa.tenant_status(uuid(&json!(b))).as_deref(), Some("ACTIVE"));
    resolve(&h, &b).await.expect(200);

    // (c) A READY organization is suspended: resolution refuses at once (before Platform
    // Access is reached); a failing compensation is retried with backoff, never dropped.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{a}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    resolve(&h, &a)
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    assert_eq!(read(&h, &a).await["scopes"][0]["mappingState"], "SUSPENDED");
    pa.status_unavailable.store(true, Ordering::SeqCst);
    let r = run(&h, &pa).await;
    assert_eq!(r.sync_failed, 1, "{r:?}");
    let scope = read(&h, &a).await["scopes"][0].clone();
    assert_eq!(scope["tenantStatusApplied"], "ACTIVE");
    assert_eq!(scope["lastSyncErrorClass"], "PLATFORM_ACCESS_UNAVAILABLE");
    assert_eq!(scope["syncAttempts"], 1);
    // Not due before the backoff; then retried until applied.
    assert_eq!(run(&h, &pa).await.sync_failed, 0);
    due_now(&h).await;
    assert_eq!(run(&h, &pa).await.sync_failed, 1);
    assert_eq!(read(&h, &a).await["scopes"][0]["syncAttempts"], 2);
    pa.status_unavailable.store(false, Ordering::SeqCst);
    due_now(&h).await;
    assert_eq!(run(&h, &pa).await.synced, 1);
    assert_eq!(
        pa.tenant_status(uuid(&json!(a))).as_deref(),
        Some("SUSPENDED")
    );
    assert_eq!(
        read(&h, &a).await["scopes"][0]["tenantStatusApplied"],
        "SUSPENDED"
    );
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'scope.tenant_status_applied' AND organization_id = '{a}'")).await, 1);

    // (d) Archival cancels an open workflow and suspends a recorded tenant.
    let d = s(&h.create_org(owner, "susp-d").await["id"]);
    provisioning::enqueue(&h.db.conn, 10).await.unwrap();
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{d}/archive"),
            json!({}),
        )
        .await
        .expect(200);
    assert_eq!(read(&h, &d).await["workflowState"], "CANCELLED");
    retry(&h, &d)
        .await
        .expect_error(409, "ORGANIZATION_ARCHIVED");
    assert_eq!(run(&h, &pa).await.ready, 0);
    // An archived READY organization keeps its tenant suspended.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{b}/archive"),
            json!({}),
        )
        .await
        .expect(200);
    run(&h, &pa).await;
    assert_eq!(
        pa.tenant_status(uuid(&json!(b))).as_deref(),
        Some("SUSPENDED")
    );
    resolve(&h, &b)
        .await
        .expect_error(409, "ORGANIZATION_ARCHIVED");
}

#[tokio::test]
async fn split_brain_mappings_and_drift_are_refused_and_reported() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = FakePlatformAccess::default();
    let owner = Uuid::new_v4();
    let a = s(&h.create_org(owner, "brain-a").await["id"]);
    run(&h, &pa).await;
    let ta = pa.triple(uuid(&json!(a)));
    // Platform Access answers organization B with A's triple: never mapped twice.
    let b = s(&h.create_org(owner, "brain-b").await["id"]);
    pa.forced_triple.lock().unwrap().insert(uuid(&json!(b)), ta);
    let r = run(&h, &pa).await;
    assert_eq!(r.failed, 1, "{r:?}");
    let p = read(&h, &b).await;
    assert_eq!(p["lastErrorClass"], "SCOPE_MISMATCH");
    assert_eq!(p["scopes"], json!([]));
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_platform_scopes WHERE tenant_id = '{}'",
            ta.0
        ))
        .await,
        1
    );
    // The schema refuses a second mapping of one tenant outright.
    let dup = h
        .exec(&format!(
            "INSERT INTO organization_platform_scopes (id, organization_id, capability, tenant_id, application_id, environment_id, mapping_state, created_at, updated_at) VALUES (gen_random_uuid(), '{b}', 'primary', '{}', '{}', '{}', 'VERIFYING', now(), now())",
            ta.0, ta.1, ta.2
        ))
        .await;
    assert!(dup.is_err());
    // Drift: Platform Access no longer has A's scope. Reconciliation (after its interval)
    // marks the mapping and the organization FAILED, monotonically versioned.
    let before = h.count(&format!("SELECT mapping_version FROM organization_platform_scopes WHERE organization_id = '{a}'")).await;
    pa.scopes.lock().unwrap().remove(&uuid(&json!(a)));
    let r = run(&h, &pa).await;
    assert_eq!(r.drifted, 0, "not yet due");
    h.exec(&format!("UPDATE organization_platform_scopes SET verified_at = now() - interval '7 hours' WHERE organization_id = '{a}'")).await.unwrap();
    let r = run(&h, &pa).await;
    assert_eq!(r.drifted, 1, "{r:?}");
    let p = read(&h, &a).await;
    assert_eq!(p["provisioningState"], "FAILED");
    assert_eq!(p["lastErrorClass"], "SCOPE_DRIFT");
    assert_eq!(p["scopes"][0]["mappingState"], "FAILED");
    assert!(p["scopes"][0]["mappingVersion"].as_i64().unwrap() > before);
    resolve(&h, &a)
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE event_type = 'scope.mapping_changed' AND organization_id = '{a}' AND payload->'payload'->>'mappingState' = 'FAILED'")).await, 1);
}

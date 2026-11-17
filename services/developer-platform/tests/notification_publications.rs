//! Notification Service N7 / N11: the Developer Platform's publications
//! (`developer-platform.credential.expiring-soon` / `.expired`,
//! `developer-platform.production-access.decided`) through the REAL router (operator
//! decisions), the real credential sweep over the in-memory Platform Access, the real
//! outbox and relay, and a fake I1 that verifies the REAL service token
//! (`cfmeet.notifications`, `notifications-event.publish`) and validates every envelope
//! with packages/notification-client. Real PostgreSQL.

mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    routing::post,
    Json, Router,
};
use cf_notification_client::{validate, Envelope};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Duration, Utc};
use common::*;
use developer_platform::{
    control::credential_lifecycle::{
        notification_sweep_once, CredentialLifecycleSettings, Notice, EXPIRING_SOON,
    },
    notification_publications::{
        self as np, credential_notice, enqueue, maintain, relay_due, NotificationServiceEvents,
        PublicationReport, Publishing, INGEST_PATH,
    },
    platform_access::PlatformAccessPort,
    worker,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

// --------------------------------------------------------------------------- fake I1

#[derive(Clone, Copy)]
enum Scripted {
    Status(u16),
    /// Every event of the next batch gets this outcome and reason.
    Outcome(&'static str, &'static str),
}

#[derive(Default)]
struct I1 {
    /// Every authenticated request body.
    batches: Vec<Value>,
    script: Vec<Scripted>,
    committed: BTreeSet<String>,
    unauthenticated: usize,
    /// Envelopes refused by the client validator (must stay empty).
    invalid: Vec<String>,
}

#[derive(Clone)]
struct FakeI1 {
    url: String,
    inner: Arc<Mutex<I1>>,
}

struct Ctx {
    verifier: ServiceTokenVerifier,
    inner: Arc<Mutex<I1>>,
}

async fn ingest(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let error = |status: u16, code: &str| {
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({ "error": { "code": code, "message": code } })),
        )
    };
    let Ok(caller) = ctx.verifier.verify_headers(&headers) else {
        ctx.inner.lock().unwrap().unauthenticated += 1;
        return error(401, "INTERNAL_AUTHENTICATION_FAILED");
    };
    if !caller.has_permission(InternalPermission::NotificationsEventPublish) {
        return error(403, "INTERNAL_PERMISSION_DENIED");
    }
    let issuer = caller.authenticated_service().as_str().to_owned();
    let mut inner = ctx.inner.lock().unwrap();
    inner.batches.push(body.clone());
    // The contract body: `events` (1..=100) only (the Developer Platform sends no shadow).
    let keys: BTreeSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let events = body["events"].as_array().cloned().unwrap_or_default();
    if keys != BTreeSet::from(["events"]) || !(1..=100).contains(&events.len()) {
        return error(400, "VALIDATION_ERROR");
    }
    let scripted = (!inner.script.is_empty()).then(|| inner.script.remove(0));
    if let Some(Scripted::Status(status)) = scripted {
        return error(status, "SCRIPTED");
    }
    let mut results = Vec::new();
    for e in events {
        let id = e["eventId"].as_str().unwrap_or_default().to_owned();
        match serde_json::from_value::<Envelope>(e.clone()) {
            Ok(env) if validate(&env, Some(&issuer)).is_ok() => {}
            other => inner.invalid.push(format!("{id}: {other:?}")),
        }
        let (outcome, reason) = match scripted {
            Some(Scripted::Outcome(o, r)) => (o, Some(r)),
            _ if inner.committed.insert(id.clone()) => ("ACCEPTED", None),
            _ => ("DUPLICATE", None),
        };
        let mut r = json!({ "eventId": id, "outcome": outcome });
        if let Some(reason) = reason {
            r["reason"] = json!(reason);
        }
        results.push(r);
    }
    (StatusCode::OK, Json(json!({ "results": results })))
}

impl FakeI1 {
    /// The Notification Service's I1, trusting the Developer Platform's `key` with
    /// `notifications-event.publish` only.
    async fn start(key: &ServiceSigningKey) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::NOTIFICATIONS,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::DEVELOPER_PLATFORM,
                "audience": cf_service_auth::services::NOTIFICATIONS,
                "runtime_environment": ENV,
                "keys": [key.public_jwk()],
                "permissions": ["notifications-event.publish"],
            }],
        })
        .to_string();
        let policy = TrustPolicy::from_json(&bundle, &ServiceId::notifications(), ENV).unwrap();
        let clock: SharedClock = Arc::new(SystemClock);
        let inner = Arc::new(Mutex::new(I1::default()));
        let ctx = Arc::new(Ctx {
            verifier: ServiceTokenVerifier::new(policy, clock),
            inner: inner.clone(),
        });
        let app = Router::new()
            .route(INGEST_PATH, post(ingest))
            .with_state(ctx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { url, inner }
    }

    fn batches(&self) -> Vec<Value> {
        self.inner.lock().unwrap().batches.clone()
    }

    fn events(&self) -> Vec<Value> {
        self.batches()
            .iter()
            .flat_map(|b| b["events"].as_array().cloned().unwrap_or_default())
            .collect()
    }

    fn script(&self, s: &[Scripted]) {
        self.inner.lock().unwrap().script.extend_from_slice(s);
    }

    fn invalid(&self) -> Vec<String> {
        self.inner.lock().unwrap().invalid.clone()
    }

    fn unauthenticated(&self) -> usize {
        self.inner.lock().unwrap().unauthenticated
    }
}

/// The Developer Platform's outbound client signing as `cfmeet.developer-platform` with
/// `key`, with a Notification Service target at `url` (or none).
fn client(key: ServiceSigningKey, url: Option<&str>) -> Arc<InternalServiceClient> {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::developer_platform(),
        ENV,
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(ENV);
    if let Some(url) = url {
        targets.insert(ServiceId::notifications(), url).unwrap();
    }
    Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    )
}

fn copy(k: &ServiceSigningKey) -> ServiceSigningKey {
    ServiceSigningKey::from_pkcs8_pem(k.kid().clone(), &k.to_pkcs8_pem()).unwrap()
}

/// A trusted key, its fake I1 and the matching sink.
async fn i1() -> (FakeI1, NotificationServiceEvents, ServiceSigningKey) {
    let k = key("devplat-test-1");
    let fake = FakeI1::start(&k).await;
    let sink = NotificationServiceEvents::new(client(copy(&k), Some(&fake.url)));
    (fake, sink, k)
}

// ------------------------------------------------------------------------- fixtures

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

struct Org {
    owner: String,
    owner_id: Uuid,
    tenant: Uuid,
    project: Uuid,
    environment: Uuid,
}

/// Organization -> project -> SANDBOX environment of a new owner (through the API).
async fn org(c: &Control, email: &str) -> Org {
    let (owner, owner_id) = c.sessions.sign_in(email);
    let o = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let tenant = id(&o.json);
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{tenant}/projects"),
            Some(json!({ "displayName": "Backend" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let project = id(&p.json);
    let e = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    Org {
        owner,
        owner_id,
        tenant,
        project,
        environment: id(&e.json),
    }
}

/// A production-access request of `o`'s project.
async fn request(c: &Control, o: &Org) -> Uuid {
    let r = c
        .req(
            As::User(&o.owner),
            Method::POST,
            &format!("/v1/projects/{}/production-access-requests", o.project),
            Some(json!({ "capabilities": ["MEETINGS"], "useCaseSummary": "Appointments" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    id(&r.json)
}

async fn decide(c: &Control, rid: Uuid, command: &str) -> Value {
    let body = (command == "reject").then(|| json!({ "reasonCode": "NOT_ELIGIBLE" }));
    let r = c
        .operator_as(
            Method::POST,
            &format!("/internal/v1/operator/production-access-requests/{rid}/{command}"),
            body,
            Some(Uuid::new_v4()),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{command}: {}", r.json);
    r.json
}

/// A harness whose production-access decisions are (or are not) published.
async fn control(publish: bool) -> Option<Control> {
    Control::with_state(move |s| {
        s.with_production_reviewers(vec!["ops.tester".into()])
            .with_notifications(Publishing {
                production_access: publish,
            })
    })
    .await
}

async fn add_member(db: &DatabaseConnection, tenant: Uuid, role: &str, status: &str) -> Uuid {
    let user = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "INSERT INTO devplat_workspace_members (id, tenant_id, user_id, role, status, joined_via, created_at, \
           updated_at, removed_at) VALUES ($1, $2, $3, $4, $5, 'OPERATOR', now(), now(), \
           CASE WHEN $5 = 'REMOVED' THEN now() END)",
        vec![
            Uuid::new_v4().into(),
            tenant.into(),
            user.into(),
            role.into(),
            status.into(),
        ],
    ))
    .await
    .unwrap();
    user
}

/// A service account of `o`'s environment with one credential expiring at `expires_at`.
fn credential(c: &Control, o: &Org, expires_at: DateTime<Utc>, revoked: bool) -> Uuid {
    let sa = Uuid::new_v4();
    c.pa.service_accounts.lock().unwrap().insert(
        sa,
        MemServiceAccount {
            tenant: o.tenant,
            app: o.project,
            env: o.environment,
            name: format!("sa-{}", &sa.simple().to_string()[..8]),
            description: None,
            status: "ACTIVE".into(),
            created_at: Utc::now() - Duration::days(60),
        },
    );
    let id = Uuid::new_v4();
    c.pa.credentials.lock().unwrap().insert(
        id,
        MemCredential {
            tenant: o.tenant,
            app: o.project,
            env: o.environment,
            sa,
            scopes: vec!["calls.read".into()],
            label: None,
            revoked,
            suspended: false,
            superseded_by: None,
            expires_at,
            created_at: Utc::now() - Duration::days(30),
        },
    );
    id
}

async fn text(c: &Control, sql: &str) -> String {
    c.db.conn
        .query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
        .await
        .unwrap()
        .map(|r| {
            r.try_get_by_index::<Option<String>>(0)
                .unwrap()
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

async fn rows(c: &Control) -> i64 {
    c.count("SELECT count(*) FROM devplat_notification_publications")
        .await
}

fn recipients(e: &Value) -> Vec<String> {
    let mut r: Vec<String> = e["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ref"].as_str().unwrap().to_owned())
        .collect();
    r.sort();
    r
}

fn user_ref(u: Uuid) -> String {
    format!("cfmeet-user:{u}")
}

// ---------------------------------------------------------------------------- tests

#[tokio::test]
async fn production_access_decisions_are_published_only_on_the_explicit_route() {
    // Default route (`legacy`): the operator-console email stays the only notice, so
    // nothing at all is enqueued for the Notification Service (mutual exclusion).
    let Some(legacy) = control(false).await else {
        return;
    };
    let o = org(&legacy, "owner@legacy.example").await;
    let rid = request(&legacy, &o).await;
    assert_eq!(decide(&legacy, rid, "approve").await["state"], "APPROVED");
    assert_eq!(rows(&legacy).await, 0, "legacy route: nothing is published");

    // Explicit `notification-service` route: one publication in the decision transaction.
    let Some(c) = control(true).await else {
        return;
    };
    let o = org(&c, "owner@routed.example").await;
    let rid = request(&c, &o).await;
    assert_eq!(rows(&c).await, 0, "a submission is not a decision");
    decide(&c, rid, "review").await;
    assert_eq!(rows(&c).await, 0, "a review is not a decision");
    decide(&c, rid, "approve").await;
    assert_eq!(rows(&c).await, 1);
    assert_eq!(
        text(
            &c,
            "SELECT idempotency_key FROM devplat_notification_publications"
        )
        .await,
        format!("devplat-access:{rid}:APPROVED"),
        "today's Meeting ledger key"
    );
    // An approved request may still be rejected: a second, distinct decision.
    decide(&c, rid, "reject").await;
    assert_eq!(rows(&c).await, 2);

    let (fake, sink, _) = i1().await;
    let r = relay_due(&c.db.conn, &sink, 100, Utc::now()).await.unwrap();
    assert_eq!(r.acked, 2, "{r:?}");
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
    let events = fake.events();
    assert_eq!(events.len(), 2);
    let mut decisions = BTreeSet::new();
    for e in &events {
        assert_eq!(e["eventType"], np::PRODUCTION_ACCESS_DECIDED);
        assert_eq!(e["producer"], "cfmeet.developer-platform");
        assert_eq!(e["subject"]["kind"], "PRODUCTION_ACCESS_REQUEST");
        assert_eq!(e["subject"]["id"], rid.to_string());
        assert!(e.get("scope").is_none());
        assert!(e.get("sealed").is_none());
        // Ids only: exactly the catalogue's two data keys.
        let keys: BTreeSet<&str> = e["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, BTreeSet::from(["decision", "requestId"]));
        assert_eq!(e["data"]["requestId"], rid.to_string());
        let d = e["data"]["decision"].as_str().unwrap().to_owned();
        assert_eq!(
            e["delivery"]["idempotencyKey"],
            format!("devplat-access:{rid}:{d}")
        );
        // The applicant (the owner who submitted).
        assert_eq!(recipients(e), vec![user_ref(o.owner_id)]);
        decisions.insert(d);
    }
    assert_eq!(
        decisions,
        BTreeSet::from(["APPROVED".into(), "REJECTED".into()])
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_notification_publications WHERE state = 'ACKED' AND outcome = 'ACCEPTED' AND recipients = 1")
            .await,
        2
    );
}

#[tokio::test]
async fn a_departed_applicant_is_replaced_by_the_owners() {
    let Some(c) = control(true).await else {
        return;
    };
    let o = org(&c, "owner@departed.example").await;
    let rid = request(&c, &o).await;
    let second_owner = add_member(&c.db.conn, o.tenant, "OWNER", "ACTIVE").await;
    add_member(&c.db.conn, o.tenant, "ADMIN", "ACTIVE").await;
    // The applicant left the organization before the decision.
    c.db.conn
        .execute_unprepared(&format!(
            "UPDATE devplat_workspace_members SET status = 'REMOVED', removed_at = now() WHERE user_id = '{}'",
            o.owner_id
        ))
        .await
        .unwrap();
    decide(&c, rid, "reject").await;
    let (fake, sink, _) = i1().await;
    assert_eq!(
        relay_due(&c.db.conn, &sink, 100, Utc::now())
            .await
            .unwrap()
            .acked,
        1
    );
    let e = &fake.events()[0];
    assert_eq!(e["data"]["decision"], "REJECTED");
    assert_eq!(recipients(e), vec![user_ref(second_owner)], "owners only");
    assert!(fake.invalid().is_empty());
}

#[tokio::test]
async fn credential_notices_go_to_owners_and_admins_once() {
    let Some(c) = control(false).await else {
        return;
    };
    let o = org(&c, "owner@credentials.example").await;
    let admin = add_member(&c.db.conn, o.tenant, "ADMIN", "ACTIVE").await;
    add_member(&c.db.conn, o.tenant, "DEVELOPER", "ACTIVE").await;
    add_member(&c.db.conn, o.tenant, "ADMIN", "REMOVED").await;
    let now = Utc::now();
    let soon = credential(&c, &o, now + Duration::days(10), false);
    let gone = credential(&c, &o, now - Duration::days(2), false);
    credential(&c, &o, now + Duration::days(3), true); // revoked: no notice
    credential(&c, &o, now + Duration::days(60), false); // outside every lead time
    let settings = CredentialLifecycleSettings::default();

    let first = notification_sweep_once(&c.db.conn, c.pa.as_ref(), &settings, now)
        .await
        .unwrap();
    assert_eq!((first.environments, first.notified), (1, 2), "{first:?}");
    assert_eq!(first.deliveries, 0, "no webhook is involved");
    let again = notification_sweep_once(&c.db.conn, c.pa.as_ref(), &settings, now)
        .await
        .unwrap();
    assert_eq!((again.notified, again.deduplicated), (0, 2), "{again:?}");
    assert_eq!(rows(&c).await, 2);
    // Customer webhooks are untouched: no webhook event, no lifecycle notice row.
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_credential_lifecycle_notices")
            .await,
        0
    );

    let (fake, sink, _) = i1().await;
    let r = relay_due(&c.db.conn, &sink, 100, now).await.unwrap();
    assert_eq!(r.acked, 2, "{r:?}");
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
    let events = fake.events();
    for e in &events {
        let credential = Uuid::parse_str(e["data"]["credentialId"].as_str().unwrap()).unwrap();
        let expected = if credential == soon {
            np::CREDENTIAL_EXPIRING_SOON
        } else {
            assert_eq!(credential, gone);
            np::CREDENTIAL_EXPIRED
        };
        assert_eq!(e["eventType"], expected);
        assert_eq!(e["subject"]["kind"], "CREDENTIAL");
        assert_eq!(e["subject"]["id"], credential.to_string());
        assert_eq!(e["scope"]["tenantId"], o.tenant.to_string());
        assert_eq!(e["scope"]["applicationId"], o.project.to_string());
        assert_eq!(e["scope"]["environmentId"], o.environment.to_string());
        assert_eq!(e["data"]["applicationId"], o.project.to_string());
        let keys: BTreeSet<&str> = e["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from(["applicationId", "credentialId", "expiresAt"])
        );
        assert!(e.get("sealed").is_none());
        let mut want = vec![user_ref(o.owner_id), user_ref(admin)];
        want.sort();
        assert_eq!(recipients(e), want, "ACTIVE owners and admins only");
        assert!(e["delivery"]["idempotencyKey"]
            .as_str()
            .unwrap()
            .starts_with(&format!("devplat-credential:{credential}:")));
    }
    // The 14-day lead time of the soon-expiring credential.
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_notification_publications WHERE subject_id = '{soon}' \
               AND dedupe_key LIKE '%:{soon}:14:%'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn a_redelivery_is_byte_identical_and_acknowledged_as_duplicate() {
    let Some(c) = control(true).await else {
        return;
    };
    let o = org(&c, "owner@redeliver.example").await;
    let rid = request(&c, &o).await;
    decide(&c, rid, "approve").await;
    let (fake, sink, _) = i1().await;
    assert_eq!(
        relay_due(&c.db.conn, &sink, 100, Utc::now())
            .await
            .unwrap()
            .acked,
        1
    );
    let stored = text(
        &c,
        "SELECT envelope::text FROM devplat_notification_publications",
    )
    .await;
    // A crash after I1 committed but before the row settled: the row is posted again.
    c.db.conn
        .execute_unprepared(
            "UPDATE devplat_notification_publications SET state = 'PENDING', outcome = NULL, completed_at = NULL",
        )
        .await
        .unwrap();
    let r = relay_due(&c.db.conn, &sink, 100, Utc::now()).await.unwrap();
    assert_eq!(r.acked, 1);
    let batches = fake.batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0], batches[1], "byte-identical re-post");
    assert_eq!(
        text(&c, "SELECT outcome FROM devplat_notification_publications").await,
        "DUPLICATE"
    );
    assert_eq!(
        text(
            &c,
            "SELECT envelope::text FROM devplat_notification_publications"
        )
        .await,
        stored
    );
    // Enqueuing the same fact again is a no-op (deterministic event id).
    let p = np::production_access_decided(
        rid,
        o.tenant,
        o.project,
        "APPROVED",
        "corr_00000000000000000000000000000000",
        Utc::now(),
    );
    assert!(!enqueue(&c.db.conn, &p, Utc::now()).await.unwrap());
    assert_eq!(rows(&c).await, 1);
}

/// Enqueues one credential notice of `o` (recipients: its owner).
async fn one(c: &Control, o: &Org, now: DateTime<Utc>) -> String {
    let notice = Notice {
        event_type: EXPIRING_SOON,
        threshold_days: 7,
        occurred_at: now,
    };
    let p = credential_notice(
        (o.tenant, o.project, o.environment),
        Uuid::new_v4(),
        &notice,
        now + Duration::days(5),
        "corr_0123456789abcdef0123456789abcdef",
        now,
    );
    assert!(enqueue(&c.db.conn, &p, now).await.unwrap());
    p.event_id
}

async fn state_of(c: &Control, event_id: &str) -> (String, String, i64) {
    let r =
        c.db.conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT state, coalesce(last_error, '') AS e, attempts::bigint AS a \
                   FROM devplat_notification_publications WHERE event_id = '{event_id}'"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
    (
        r.try_get("", "state").unwrap(),
        r.try_get("", "e").unwrap(),
        r.try_get("", "a").unwrap(),
    )
}

#[tokio::test]
async fn transient_failures_retry_with_backoff_and_contract_refusals_dead_letter() {
    let Some(c) = control(false).await else {
        return;
    };
    let o = org(&c, "owner@retry.example").await;
    let (fake, sink, _) = i1().await;
    let mut now = Utc::now();
    let event = one(&c, &o, now).await;

    // 5xx, 401 and 429 are retried with a growing backoff; nothing is lost.
    fake.script(&[
        Scripted::Status(503),
        Scripted::Status(401),
        Scripted::Status(429),
    ]);
    for attempt in 1..=3 {
        let r = relay_due(&c.db.conn, &sink, 100, now).await.unwrap();
        assert_eq!(r.retried, 1, "attempt {attempt}: {r:?}");
        assert_eq!(
            state_of(&c, &event).await,
            ("PENDING".into(), "UNAVAILABLE".into(), attempt)
        );
        // Not due again before the backoff.
        assert_eq!(
            relay_due(&c.db.conn, &sink, 100, now).await.unwrap(),
            PublicationReport::default()
        );
        now += np::backoff(attempt as i32) + Duration::seconds(1);
    }
    // A REAL authentication failure (a key I1 does not trust) is retried too.
    let untrusted = NotificationServiceEvents::new(client(key("devplat-rogue-1"), Some(&fake.url)));
    assert_eq!(
        relay_due(&c.db.conn, &untrusted, 100, now)
            .await
            .unwrap()
            .retried,
        1
    );
    assert_eq!(fake.unauthenticated(), 1);
    now += np::backoff(4) + Duration::seconds(1);
    let r = relay_due(&c.db.conn, &sink, 100, now).await.unwrap();
    assert_eq!(r.acked, 1, "{r:?}");
    assert_eq!(state_of(&c, &event).await.0, "ACKED");
    // Every attempt posted the same envelope.
    let batches = fake.batches();
    assert!(batches.windows(2).all(|w| w[0] == w[1]));

    // A contract refusal of the batch (4xx) dead-letters it.
    let refused = one(&c, &o, now).await;
    fake.script(&[Scripted::Status(400)]);
    assert_eq!(
        relay_due(&c.db.conn, &sink, 100, now).await.unwrap().dead,
        1
    );
    assert_eq!(
        state_of(&c, &refused).await,
        ("DEAD".into(), "CONTRACT_REJECTED".into(), 1)
    );
    let unprocessable = one(&c, &o, now).await;
    fake.script(&[Scripted::Status(422)]);
    relay_due(&c.db.conn, &sink, 100, now).await.unwrap();
    assert_eq!(state_of(&c, &unprocessable).await.0, "DEAD");

    // A per-event REJECTED dead-letters the row with the service's reason.
    let rejected = one(&c, &o, now).await;
    fake.script(&[Scripted::Outcome("REJECTED", "DIGEST_MISMATCH")]);
    assert_eq!(
        relay_due(&c.db.conn, &sink, 100, now).await.unwrap().dead,
        1
    );
    assert_eq!(
        state_of(&c, &rejected).await,
        ("DEAD".into(), "DIGEST_MISMATCH".into(), 1)
    );
    // HELD (the service's category is off) is an acknowledgement.
    let held = one(&c, &o, now).await;
    fake.script(&[Scripted::Outcome("HELD", "CATEGORY_NOT_ACTIVE")]);
    assert_eq!(
        relay_due(&c.db.conn, &sink, 100, now).await.unwrap().acked,
        1
    );
    assert_eq!(state_of(&c, &held).await.0, "ACKED");
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
}

#[tokio::test]
async fn an_event_without_recipients_is_skipped_and_overdue_rows_expire_and_purge() {
    let Some(c) = control(false).await else {
        return;
    };
    let o = org(&c, "owner@skip.example").await;
    let (fake, sink, _) = i1().await;
    let now = Utc::now();
    // An organization nobody is an active member of any more.
    let orphan = Org {
        tenant: Uuid::new_v4(),
        ..org(&c, "owner@orphan.example").await
    };
    let skipped = one(&c, &orphan, now).await;
    let r = relay_due(&c.db.conn, &sink, 100, now).await.unwrap();
    assert_eq!(r.skipped, 1, "{r:?}");
    assert_eq!(
        state_of(&c, &skipped).await,
        ("SKIPPED".into(), "NO_RECIPIENTS".into(), 0)
    );
    assert!(fake.batches().is_empty(), "nothing posted");

    // A row past its expiry is never posted.
    let late = one(&c, &o, now).await;
    let r = relay_due(&c.db.conn, &sink, 100, now + Duration::days(6))
        .await
        .unwrap();
    assert_eq!((r.expired, r.acked), (1, 0), "{r:?}");
    assert_eq!(state_of(&c, &late).await.0, "EXPIRED");
    assert!(fake.batches().is_empty());

    // Settled rows are purged after the retention period, PENDING rows never.
    let pending = one(&c, &o, now).await;
    let (expired, purged) = maintain(&c.db.conn, now + Duration::days(10), 100)
        .await
        .unwrap();
    assert_eq!((expired, purged), (1, 0), "the pending row expires first");
    let (_, purged) = maintain(&c.db.conn, now + Duration::days(70), 100)
        .await
        .unwrap();
    assert_eq!(purged, 3);
    assert_eq!(rows(&c).await, 0);
    let _ = pending;
}

#[tokio::test]
async fn nothing_is_sent_without_the_base_url() {
    let Some(c) = control(false).await else {
        return;
    };
    let o = org(&c, "owner@unset.example").await;
    credential(&c, &o, Utc::now() + Duration::days(2), false);
    let rid = request(&c, &o).await;
    decide(&c, rid, "approve").await;
    // The worker without CF_NOTIFICATIONS_BASE_URL (no notification jobs): nothing is
    // enqueued, relayed or scheduled, whatever is due.
    let on = CredentialLifecycleSettings::default();
    worker::run_pass_with_jobs(
        &c.db.conn,
        "worker-unset",
        "test",
        None,
        Some(c.pa.as_ref() as &dyn PlatformAccessPort),
        &worker::PeriodicJobs {
            credential_lifecycle: on,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rows(&c).await, 0);
    assert_eq!(
        c.count(
            "SELECT count(*) FROM devplat_worker_schedules \
              WHERE job IN ('credential-notifications', 'notification-publications')"
        )
        .await,
        0
    );
    // A row enqueued anyway (another process configured) waits: a client without the
    // Notification Service target sends nothing and does not count an attempt.
    let k = key("devplat-test-1");
    let fake = FakeI1::start(&k).await;
    let unconfigured = NotificationServiceEvents::new(client(copy(&k), None));
    let event = one(&c, &o, Utc::now()).await;
    let r = relay_due(&c.db.conn, &unconfigured, 100, Utc::now())
        .await
        .unwrap();
    assert_eq!(r.not_configured, 1);
    assert_eq!(
        state_of(&c, &event).await,
        ("PENDING".into(), "NOT_CONFIGURED".into(), 0)
    );
    assert!(fake.batches().is_empty());
}

#[tokio::test]
async fn the_worker_enqueues_and_relays_credential_notices_in_one_pass() {
    let Some(c) = control(false).await else {
        return;
    };
    let o = org(&c, "owner@worker.example").await;
    let cred = credential(&c, &o, Utc::now() + Duration::hours(20), false);
    let (fake, sink, _) = i1().await;
    let jobs = |credential_notices| worker::PeriodicJobs {
        credential_lifecycle: CredentialLifecycleSettings::default(),
        notifications: Some(worker::NotificationJobs {
            sink: Arc::new(NotificationServiceEvents::new(client(
                copy(&key("devplat-unused-1")),
                Some(&fake.url),
            ))),
            credential_notices,
        }),
        ..Default::default()
    };
    // Route `off`: the relay runs, no credential notice is enqueued.
    worker::run_pass_with_jobs(
        &c.db.conn,
        "worker-a",
        "test",
        None,
        Some(c.pa.as_ref() as &dyn PlatformAccessPort),
        &jobs(false),
    )
    .await
    .unwrap();
    assert_eq!(rows(&c).await, 0);
    // Default route: enqueued and relayed in the same pass (1-day lead time).
    let routed = worker::PeriodicJobs {
        notifications: Some(worker::NotificationJobs {
            sink: Arc::new(sink),
            credential_notices: true,
        }),
        ..jobs(true)
    };
    worker::run_pass_with_jobs(
        &c.db.conn,
        "worker-a",
        "test",
        None,
        Some(c.pa.as_ref() as &dyn PlatformAccessPort),
        &routed,
    )
    .await
    .unwrap();
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_notification_publications WHERE state = 'ACKED'")
            .await,
        1
    );
    let e = &fake.events()[0];
    assert_eq!(e["eventType"], np::CREDENTIAL_EXPIRING_SOON);
    assert_eq!(e["data"]["credentialId"], cred.to_string());
    assert_eq!(recipients(e), vec![user_ref(o.owner_id)]);
    assert!(fake.invalid().is_empty());
    // The sweep is scheduled at most once per interval across workers.
    worker::run_pass_with_jobs(
        &c.db.conn,
        "worker-b",
        "test",
        None,
        Some(c.pa.as_ref() as &dyn PlatformAccessPort),
        &routed,
    )
    .await
    .unwrap();
    assert_eq!(rows(&c).await, 1);
    assert_eq!(fake.events().len(), 1);
}

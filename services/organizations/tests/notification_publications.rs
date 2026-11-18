//! Notification Service N7: Organizations' publications of its catalogue events
//! (`organizations.*`, Document 4 §5.2 #1–10) on the real API flows, and the relay to a
//! fake I1 that verifies the REAL service token (`cfmeet.notifications`,
//! `notifications-event.publish`) and validates every envelope with
//! packages/notification-client. Real PostgreSQL.

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use cf_notification_client::{validate, Envelope};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use common::{key, s, Harness, ENV};
use organizations::{
    config::OrganizationsConfig,
    notification_publications::{
        discover, maintain, publication_event_id, relay_due, run_once, NotificationServiceEvents,
        PublicationReport, INGEST_PATH, MAX_ATTEMPTS,
    },
    outbox::DisabledDispatcher,
    worker,
};
use sea_orm::{FromQueryResult, Statement};
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
    /// Every authenticated, well-formed request body.
    batches: Vec<Value>,
    script: Vec<Scripted>,
    /// eventId → the first accepted body (a re-post must be byte-identical).
    committed: BTreeMap<String, Value>,
    unauthenticated: usize,
    /// Envelopes refused by the client validator or re-posted with another body (must
    /// stay empty).
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
    // The contract body: `events` (1..=100) only (Organizations never sends shadow copies).
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
    inner.batches.push(body.clone());
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
            _ => match inner.committed.get(&id) {
                None => {
                    inner.committed.insert(id.clone(), e.clone());
                    ("ACCEPTED", None)
                }
                Some(first) if *first == e => ("DUPLICATE", None),
                Some(_) => {
                    inner
                        .invalid
                        .push(format!("{id}: re-posted with another body"));
                    ("REJECTED", Some("DIGEST_MISMATCH"))
                }
            },
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
    /// The Notification Service's I1, trusting Organizations' `key` with
    /// `notifications-event.publish` only.
    async fn start(key: &ServiceSigningKey) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::NOTIFICATIONS,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::ORGANIZATIONS,
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

    /// Every posted envelope, in order.
    fn envelopes(&self) -> Vec<Value> {
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

/// Organizations' outbound client signing as `cfmeet.organizations` with `key`, with a
/// Notification Service target at `url` (or none).
fn sink(key: ServiceSigningKey, url: Option<&str>) -> NotificationServiceEvents {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::organizations(),
        ENV,
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(ENV);
    if let Some(url) = url {
        targets.insert(ServiceId::notifications(), url).unwrap();
    }
    NotificationServiceEvents::new(Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    ))
}

fn copy(k: &ServiceSigningKey) -> ServiceSigningKey {
    ServiceSigningKey::from_pkcs8_pem(k.kid().clone(), &k.to_pkcs8_pem()).unwrap()
}

// -------------------------------------------------------------------------- helpers

#[derive(Debug, Clone, FromQueryResult)]
struct Publication {
    event_id: String,
    source_event_id: Uuid,
    chunk: i16,
    organization_id: Uuid,
    event_type: String,
    envelope: Option<Value>,
    state: String,
    outcome: Option<String>,
    last_error: Option<String>,
    attempts: i32,
}

async fn publications(h: &Harness) -> Vec<Publication> {
    Publication::find_by_statement(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT event_id, source_event_id, chunk, organization_id, event_type, envelope, state, outcome, last_error, attempts \
           FROM organization_notification_publications ORDER BY created_at, occurred_at, chunk",
    ))
    .all(&h.db.conn)
    .await
    .unwrap()
}

async fn of_type(h: &Harness, t: &str) -> Vec<Publication> {
    publications(h)
        .await
        .into_iter()
        .filter(|p| p.event_type == t)
        .collect()
}

fn refs(env: &Value) -> Vec<String> {
    env["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ref"].as_str().unwrap().to_string())
        .collect()
}

fn user_ref(u: Uuid) -> String {
    format!("cfmeet-user:{u}")
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// Makes every waiting publication due now (instead of moving the clock).
async fn due_now(h: &Harness) {
    h.exec("UPDATE organization_notification_publications SET next_attempt_at = now() WHERE state = 'PENDING'")
        .await
        .unwrap();
}

async fn patch_member(
    h: &Harness,
    org: &str,
    actor: Uuid,
    membership: &Value,
    body: Value,
) -> Value {
    let mut body = body;
    body["expectedVersion"] = membership["version"].clone();
    h.user(actor)
        .patch(
            &format!(
                "/internal/v1/organizations/{org}/members/{}",
                s(&membership["id"])
            ),
            body,
        )
        .await
        .expect(200)
        .clone()
}

/// An organization owned by a fresh owner with one MEMBER; returns (org id, owner,
/// member, membership).
async fn small_org(h: &Harness, slug: &str) -> (String, Uuid, Uuid, Value) {
    let owner = Uuid::new_v4();
    let member = Uuid::new_v4();
    let org = s(&h.create_org(owner, slug).await["id"]);
    let m = h.add_member(&org, owner, member, "MEMBER").await;
    (org, owner, member, m)
}

// --------------------------------------------------------------------------- tests

/// Every Organizations catalogue type, driven through the real API (and the expiry
/// sweeper), becomes ids-only envelopes that pass the client validator under the real
/// token, addressed exactly as the recipient rules say, never to the actor and never to
/// another organization's members.
#[tokio::test]
async fn every_catalogue_event_is_published_ids_only_to_its_recipients() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (owner, a, b, x) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let org = s(&h.create_org(owner, "notif-all").await["id"]);
    // Another organization whose member must never be addressed.
    let outsider = Uuid::new_v4();
    h.create_org(outsider, "notif-other").await;

    // invitation.created (invitee unknown) + invitation.accepted (→ inviter), twice.
    let a_m = h.add_member(&org, owner, a, "MEMBER").await;
    let b_m = h.add_member(&org, owner, b, "ADMIN").await;
    // invitation.declined (→ inviter).
    let (_, token) = h
        .invite(&org, owner, "decliner@example.com", "MEMBER")
        .await;
    h.user(x)
        .email("decliner@example.com")
        .post(
            "/internal/v1/organization-invitations/decline",
            json!({ "token": token }),
        )
        .await
        .expect(200);
    // invitation.revoked: B invited, the owner revokes (→ B).
    let (inv, _) = h.invite(&org, b, "revoked@example.com", "MEMBER").await;
    h.user(owner)
        .post(
            &format!(
                "/internal/v1/organizations/{org}/invitations/{}/revoke",
                s(&inv["id"])
            ),
            json!({}),
        )
        .await
        .expect(200);
    // invitation.expired by the sweeper (SYSTEM actor; → the inviter).
    let (late, _) = h.invite(&org, owner, "late@example.com", "MEMBER").await;
    h.exec(&format!(
        "UPDATE organization_invitations SET created_at = now() - interval '8 days', expires_at = now() - interval '1 day' WHERE id = '{}'",
        s(&late["id"])
    ))
    .await
    .unwrap();
    assert_eq!(worker::expire_invitations(&h.db.conn, 10).await.unwrap(), 1);
    // membership.role_changed / suspended / removed (→ A).
    let a_m = patch_member(&h, &org, owner, &a_m, json!({ "role": "ADMIN" })).await;
    let a_m = patch_member(&h, &org, owner, &a_m, json!({ "status": "SUSPENDED" })).await;
    patch_member(&h, &org, owner, &a_m, json!({ "status": "REMOVED" })).await;
    // ownership.transferred (→ B; the owner acts) and the two role changes it implies.
    let version = h.version(&org, owner).await;
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/ownership-transfer"),
            json!({ "toMembershipId": b_m["id"], "expectedVersion": version }),
        )
        .await
        .expect(200);
    // organization.suspended by an operator (no acting user): every ACTIVE member.
    h.by(&h.peers.operator)
        .post(
            &format!("/internal/v1/organizations/{org}/suspend"),
            json!({}),
        )
        .await
        .expect(200);

    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    let report = run_once(&h.db.conn, &sink(copy(&k), Some(&fake.url)))
        .await
        .unwrap();
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
    assert_eq!(fake.unauthenticated(), 0);
    assert_eq!(report.dead, 0, "{report:?}");
    assert_eq!(report.retried, 0, "{report:?}");

    let posted: Vec<Value> = fake.envelopes();
    let by_type = |t: &str| -> Vec<Value> {
        posted
            .iter()
            .filter(|e| e["eventType"] == t)
            .cloned()
            .collect()
    };
    let one = |t: &str| -> Value {
        let all = by_type(t);
        assert_eq!(all.len(), 1, "{t}: {all:#?}");
        all[0].clone()
    };
    let org_id = Uuid::parse_str(&org).unwrap();
    for e in &posted {
        let text = e.to_string();
        assert!(
            !text.contains('@') && !text.contains("example.com"),
            "{text}"
        );
        assert!(!text.contains("Org notif"), "never a name: {text}");
        assert_eq!(e["producer"], "cfmeet.organizations");
        assert_eq!(e["data"]["organizationId"], org);
        assert!(e.get("sealed").is_none());
        let keys: BTreeSet<&str> = e["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            keys.is_subset(&BTreeSet::from([
                "invitationId",
                "membershipId",
                "organizationId",
                "role"
            ])),
            "{keys:?}"
        );
        assert!(!refs(e).contains(&user_ref(outsider)), "isolation");
    }

    // invitation.created: invitees are email-addressed and unknown to Organizations.
    let created = of_type(&h, "organizations.invitation.created").await;
    assert_eq!(created.len(), 5);
    assert!(created
        .iter()
        .all(|p| p.state == "SKIPPED" && p.last_error.as_deref() == Some("NO_RECIPIENTS")));
    assert!(by_type("organizations.invitation.created").is_empty());

    let accepted = by_type("organizations.invitation.accepted");
    assert_eq!(accepted.len(), 2);
    for e in &accepted {
        assert_eq!(refs(e), vec![user_ref(owner)]);
        assert_eq!(e["subject"]["kind"], "ORGANIZATION_INVITATION");
        assert_eq!(e["subject"]["id"], e["data"]["invitationId"]);
    }
    assert_eq!(
        sorted(
            accepted
                .iter()
                .map(|e| e["data"]["role"].as_str().unwrap().to_string())
                .collect()
        ),
        vec!["ADMIN", "MEMBER"]
    );
    assert_eq!(
        refs(&one("organizations.invitation.declined")),
        vec![user_ref(owner)]
    );
    let revoked = one("organizations.invitation.revoked");
    assert_eq!(refs(&revoked), vec![user_ref(b)]);
    assert_eq!(revoked["data"]["invitationId"], inv["id"]);
    let expired = one("organizations.invitation.expired");
    assert_eq!(refs(&expired), vec![user_ref(owner)]);
    assert_eq!(expired["data"]["invitationId"], late["id"]);

    // Role changes: A → ADMIN, B → OWNER; the owner's own demotion is not notified.
    let roles = by_type("organizations.membership.role_changed");
    let mut got: Vec<(String, String)> = roles
        .iter()
        .map(|e| {
            (
                refs(e)[0].clone(),
                e["data"]["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    got.sort();
    let mut want = vec![
        (user_ref(a), "ADMIN".to_string()),
        (user_ref(b), "OWNER".to_string()),
    ];
    want.sort();
    assert_eq!(got, want);
    assert!(of_type(&h, "organizations.membership.role_changed")
        .await
        .iter()
        .any(|p| p.state == "SKIPPED" && p.last_error.as_deref() == Some("NO_RECIPIENTS")));
    for t in [
        "organizations.membership.suspended",
        "organizations.membership.removed",
    ] {
        let e = one(t);
        assert_eq!(refs(&e), vec![user_ref(a)], "{t}");
        assert_eq!(e["subject"]["kind"], "ORGANIZATION_MEMBERSHIP");
        assert_eq!(e["data"]["membershipId"], a_m["id"]);
    }
    let transferred = one("organizations.ownership.transferred");
    assert_eq!(refs(&transferred), vec![user_ref(b)]);
    assert_eq!(transferred["subject"]["kind"], "ORGANIZATION");
    assert_eq!(transferred["subject"]["id"], org);
    let suspended = one("organizations.organization.suspended");
    assert_eq!(
        refs(&suspended),
        sorted(vec![user_ref(owner), user_ref(b)]),
        "ACTIVE members only (A was removed), never the other organization"
    );

    // Every published row is acknowledged and belongs to this organization.
    let rows = publications(&h).await;
    assert!(rows
        .iter()
        .all(|p| p.organization_id == org_id || p.state == "SKIPPED"));
    let pending: Vec<&Publication> = rows.iter().filter(|p| p.envelope.is_some()).collect();
    assert_eq!(pending.len(), posted.len());
    for p in pending {
        assert_eq!(
            (p.state.as_str(), p.outcome.as_deref(), p.attempts),
            ("ACKED", Some("ACCEPTED"), 1)
        );
        assert_eq!(p.event_id, publication_event_id(p.source_event_id, p.chunk));
    }
}

/// A source event that does name an account (here a record row carrying
/// `inviteeUserId`) yields `organizations.invitation.created` for that account.
#[tokio::test]
async fn an_invitation_created_for_a_known_account_is_published() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org, owner, _, _) = small_org(&h, "notif-created").await;
    let (inv, _) = h.invite(&org, owner, "known@example.com", "GUEST").await;
    let invitee = Uuid::new_v4();
    // Simulates a source whose invitee is known (the payload contract allows it).
    h.exec(&format!(
        "UPDATE organization_outbox SET payload = jsonb_set(payload, '{{payload,inviteeUserId}}', '\"{invitee}\"') \
          WHERE event_type = 'invitation.created' AND aggregate_id = '{}'",
        s(&inv["id"])
    ))
    .await
    .unwrap();
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    run_once(&h.db.conn, &sink(copy(&k), Some(&fake.url)))
        .await
        .unwrap();
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
    let created: Vec<Value> = fake
        .envelopes()
        .into_iter()
        .filter(|e| e["eventType"] == "organizations.invitation.created")
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(refs(&created[0]), vec![user_ref(invitee)]);
    assert_eq!(created[0]["data"]["role"], "GUEST");
    assert_eq!(created[0]["data"]["invitationId"], inv["id"]);
}

/// A re-post (crash after the service committed) is byte-identical and answered
/// DUPLICATE; discovery never creates a second publication for a source event.
#[tokio::test]
async fn redelivery_is_a_duplicate_never_a_second_notification() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org, owner, _, m) = small_org(&h, "notif-dup").await;
    patch_member(&h, &org, owner, &m, json!({ "status": "SUSPENDED" })).await;
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    let sink = sink(copy(&k), Some(&fake.url));
    let first = run_once(&h.db.conn, &sink).await.unwrap();
    assert_eq!(first.acked, 2, "{first:?}"); // accepted + suspended
    let before = publications(&h).await;

    // Discovery is idempotent.
    assert_eq!(discover(&h.db.conn, 100).await.unwrap(), (0, 0));
    assert_eq!(publications(&h).await.len(), before.len());

    // The relay lost its acknowledgement: the rows are PENDING again.
    h.exec("UPDATE organization_notification_publications SET state = 'PENDING', outcome = NULL, completed_at = NULL WHERE state = 'ACKED'")
        .await
        .unwrap();
    let again = relay_due(&h.db.conn, &sink, 100).await.unwrap();
    assert_eq!(again.acked, 2);
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
    let batches = fake.batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0], batches[1], "byte-identical re-post");
    for p in publications(&h)
        .await
        .iter()
        .filter(|p| p.envelope.is_some())
    {
        assert_eq!(
            (p.state.as_str(), p.outcome.as_deref()),
            ("ACKED", Some("DUPLICATE"))
        );
    }
}

/// 5xx, 401 and 429 are retried with backoff; the row is acknowledged once the service
/// accepts it.
#[tokio::test]
async fn unavailable_unauthenticated_and_rate_limited_answers_are_retried() {
    let Some(h) = Harness::new().await else {
        return;
    };
    small_org(&h, "notif-retry").await;
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    let good = sink(copy(&k), Some(&fake.url));
    assert_eq!(discover(&h.db.conn, 100).await.unwrap(), (2, 1));

    fake.script(&[Scripted::Status(503)]);
    let r = relay_due(&h.db.conn, &good, 100).await.unwrap();
    assert_eq!((r.retried, r.acked, r.dead), (1, 0, 0));
    let p = &of_type(&h, "organizations.invitation.accepted").await[0];
    assert_eq!(
        (p.state.as_str(), p.attempts, p.last_error.as_deref()),
        ("PENDING", 1, Some("UNAVAILABLE"))
    );
    // Backoff: not due yet.
    assert_eq!(
        relay_due(&h.db.conn, &good, 100).await.unwrap(),
        PublicationReport::default()
    );

    due_now(&h).await;
    fake.script(&[Scripted::Status(429)]);
    assert_eq!(relay_due(&h.db.conn, &good, 100).await.unwrap().retried, 1);

    // A key the service does not trust: 401, retried.
    due_now(&h).await;
    let untrusted = sink(key("organizations-test-1"), Some(&fake.url));
    assert_eq!(
        relay_due(&h.db.conn, &untrusted, 100)
            .await
            .unwrap()
            .retried,
        1
    );
    assert_eq!(fake.unauthenticated(), 1);

    due_now(&h).await;
    let r = relay_due(&h.db.conn, &good, 100).await.unwrap();
    assert_eq!(r.acked, 1);
    let p = &of_type(&h, "organizations.invitation.accepted").await[0];
    assert_eq!(
        (p.state.as_str(), p.attempts, p.outcome.as_deref()),
        ("ACKED", 4, Some("ACCEPTED"))
    );
    assert!(fake.invalid().is_empty());
}

/// A contract refusal of the batch (4xx) and a per-event REJECTED dead-letter; HELD
/// acknowledges; attempts are bounded.
#[tokio::test]
async fn contract_refusals_and_rejections_dead_letter_and_held_acknowledges() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    let sink = sink(copy(&k), Some(&fake.url));
    let accepted = |h: &Harness| {
        let h_conn = h.db.conn.clone();
        async move {
            Publication::find_by_statement(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT event_id, source_event_id, chunk, organization_id, event_type, envelope, state, outcome, last_error, attempts \
                   FROM organization_notification_publications WHERE event_type = 'organizations.invitation.accepted' ORDER BY created_at DESC LIMIT 1",
            ))
            .one(&h_conn)
            .await
            .unwrap()
            .unwrap()
        }
    };

    small_org(&h, "notif-dead-a").await;
    discover(&h.db.conn, 100).await.unwrap();
    fake.script(&[Scripted::Status(400)]);
    let r = relay_due(&h.db.conn, &sink, 100).await.unwrap();
    assert_eq!((r.dead, r.retried), (1, 0));
    let p = accepted(&h).await;
    assert_eq!(
        (p.state.as_str(), p.last_error.as_deref(), p.attempts),
        ("DEAD", Some("CONTRACT_REJECTED"), 1)
    );

    small_org(&h, "notif-dead-b").await;
    discover(&h.db.conn, 100).await.unwrap();
    fake.script(&[Scripted::Outcome("REJECTED", "ROUTE_NOT_NEW")]);
    assert_eq!(relay_due(&h.db.conn, &sink, 100).await.unwrap().dead, 1);
    let p = accepted(&h).await;
    assert_eq!(
        (
            p.state.as_str(),
            p.outcome.as_deref(),
            p.last_error.as_deref()
        ),
        ("DEAD", Some("REJECTED"), Some("ROUTE_NOT_NEW"))
    );

    small_org(&h, "notif-held").await;
    discover(&h.db.conn, 100).await.unwrap();
    fake.script(&[Scripted::Outcome("HELD", "CATEGORY_NOT_ACTIVE")]);
    assert_eq!(relay_due(&h.db.conn, &sink, 100).await.unwrap().acked, 1);
    let p = accepted(&h).await;
    assert_eq!(
        (p.state.as_str(), p.outcome.as_deref()),
        ("ACKED", Some("HELD"))
    );

    // Bounded retries: the last allowed failure dead-letters.
    small_org(&h, "notif-exhausted").await;
    discover(&h.db.conn, 100).await.unwrap();
    h.exec(&format!(
        "UPDATE organization_notification_publications SET attempts = {} WHERE state = 'PENDING'",
        MAX_ATTEMPTS - 1
    ))
    .await
    .unwrap();
    fake.script(&[Scripted::Status(502)]);
    assert_eq!(relay_due(&h.db.conn, &sink, 100).await.unwrap().dead, 1);
    let p = accepted(&h).await;
    assert_eq!(
        (p.state.as_str(), p.last_error.as_deref()),
        ("DEAD", Some("ATTEMPTS_EXHAUSTED"))
    );
    assert!(fake.invalid().is_empty());
}

/// Overdue rows expire unsent, old sources are never discovered, settled rows are
/// purged, and a missing target leaves rows waiting without spending attempts.
#[tokio::test]
async fn expiry_purge_and_a_missing_target() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    let sink_ok = sink(copy(&k), Some(&fake.url));

    // A source event older than the lifetime is never published.
    small_org(&h, "notif-old").await;
    h.exec("UPDATE organization_outbox SET created_at = now() - interval '25 hours'")
        .await
        .unwrap();
    assert_eq!(discover(&h.db.conn, 100).await.unwrap(), (0, 0));

    // No target: rows wait, attempts unchanged.
    small_org(&h, "notif-wait").await;
    discover(&h.db.conn, 100).await.unwrap();
    let none = sink(copy(&k), None);
    let r = relay_due(&h.db.conn, &none, 100).await.unwrap();
    assert_eq!((r.not_configured, r.retried, r.dead), (1, 0, 0));
    let p = &of_type(&h, "organizations.invitation.accepted").await[0];
    assert_eq!(
        (p.state.as_str(), p.attempts, p.last_error.as_deref()),
        ("PENDING", 0, Some("NOT_CONFIGURED"))
    );

    // Past its expiry the row ends EXPIRED and is never posted.
    h.exec("UPDATE organization_notification_publications SET expires_at = now() - interval '1 second', next_attempt_at = now() WHERE state = 'PENDING'")
        .await
        .unwrap();
    assert_eq!(
        relay_due(&h.db.conn, &sink_ok, 100).await.unwrap(),
        PublicationReport::default()
    );
    assert_eq!(maintain(&h.db.conn, 100).await.unwrap(), (1, 0));
    let p = &of_type(&h, "organizations.invitation.accepted").await[0];
    assert_eq!(p.state, "EXPIRED");
    assert!(fake.batches().is_empty());

    // Settled rows older than the retention are purged (PENDING never).
    small_org(&h, "notif-purge").await;
    discover(&h.db.conn, 100).await.unwrap();
    h.exec("UPDATE organization_notification_publications SET completed_at = now() - interval '31 days' WHERE state <> 'PENDING'")
        .await
        .unwrap();
    let total = publications(&h).await.len();
    let (_, purged) = maintain(&h.db.conn, 1000).await.unwrap();
    assert_eq!(purged as usize, total - 1);
    assert_eq!(publications(&h).await.len(), 1);
    assert_eq!(publications(&h).await[0].state, "PENDING");
}

/// A member who leaves (the actor) is not told about it; another organization's events
/// are addressed to its own members only.
#[tokio::test]
async fn the_actor_is_never_notified_and_organizations_stay_isolated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org_a, _, member_a, _) = small_org(&h, "notif-iso-a").await;
    let (org_b, owner_b, member_b, _) = small_org(&h, "notif-iso-b").await;
    h.user(member_a)
        .post(
            &format!("/internal/v1/organizations/{org_a}/members/me/leave"),
            json!({}),
        )
        .await
        .expect(200);
    h.user(owner_b)
        .post(
            &format!("/internal/v1/organizations/{org_b}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    run_once(&h.db.conn, &sink(copy(&k), Some(&fake.url)))
        .await
        .unwrap();
    let removed = of_type(&h, "organizations.membership.removed").await;
    assert_eq!(removed.len(), 1);
    assert_eq!(
        (removed[0].state.as_str(), removed[0].last_error.as_deref()),
        ("SKIPPED", Some("NO_RECIPIENTS"))
    );
    let suspended: Vec<Value> = fake
        .envelopes()
        .into_iter()
        .filter(|e| e["eventType"] == "organizations.organization.suspended")
        .collect();
    assert_eq!(suspended.len(), 1);
    // The suspending owner is the actor: only B's member.
    assert_eq!(refs(&suspended[0]), vec![user_ref(member_b)]);
    assert_eq!(suspended[0]["data"]["organizationId"], org_b);
    assert!(fake.invalid().is_empty());
}

/// Without `CF_NOTIFICATIONS_BASE_URL` nothing is published and nothing is written: the
/// configuration yields no publisher and today's maintenance pass leaves the publication
/// table empty and the outbox exactly as before (no new destination).
#[tokio::test]
async fn nothing_is_sent_or_written_without_the_base_url() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let cfg = OrganizationsConfig::from_lookup(|k| match k {
        "DATABASE_URL" => Some("postgres://unused/organizations".into()),
        "CF_RUNTIME_ENVIRONMENT" => Some("test".into()),
        "CF_SERVICE_TRUST_BUNDLE_FILE" => Some("/unused/trust.json".into()),
        _ => None,
    })
    .unwrap();
    assert!(cfg
        .notification_sink(Arc::new(SystemClock))
        .unwrap()
        .is_none());
    let (org, owner, _, m) = small_org(&h, "notif-off").await;
    patch_member(&h, &org, owner, &m, json!({ "role": "ADMIN" })).await;
    worker::run_once(&h.db.conn, &DisabledDispatcher)
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM organization_notification_publications")
            .await,
        0
    );
    // The outbox keeps exactly today's destinations: the record and the subscribed
    // Meeting Service; the Notification Service is never an outbox destination.
    assert_eq!(
        h.count(
            "SELECT count(*)::bigint FROM organization_outbox \
              WHERE destination NOT IN ('cfmeet.reconciliation', 'cfmeet.meeting-service')"
        )
        .await,
        0
    );
    assert_eq!(
        h.count(
            "SELECT count(*)::bigint FROM organization_outbox \
              WHERE destination = 'cfmeet.meeting-service' AND state <> 'PENDING'"
        )
        .await,
        0,
        "no consumer configured: nothing delivered"
    );
}

/// A large organization's suspension is published in recipient chunks: one envelope per
/// chunk, each with its own deterministic event id and the same dedupe key.
#[tokio::test]
async fn a_large_audience_is_published_in_chunks() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "notif-large").await["id"]);
    h.exec(&format!(
        "INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_at, updated_at) \
         SELECT gen_random_uuid(), '{org}', gen_random_uuid(), 'MEMBER', 'ACTIVE', 'IMPORTED', now() - interval '1 minute', now() \
           FROM generate_series(1, 45)"
    ))
    .await
    .unwrap();
    h.by(&h.peers.operator)
        .post(
            &format!("/internal/v1/organizations/{org}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;
    run_once(&h.db.conn, &sink(copy(&k), Some(&fake.url)))
        .await
        .unwrap();
    assert!(fake.invalid().is_empty(), "{:?}", fake.invalid());
    let chunks: Vec<Value> = fake
        .envelopes()
        .into_iter()
        .filter(|e| e["eventType"] == "organizations.organization.suspended")
        .collect();
    assert_eq!(chunks.len(), 2);
    let all: BTreeSet<String> = chunks.iter().flat_map(refs).collect();
    assert_eq!(all.len(), 46, "the owner and 45 members, each once");
    assert_eq!(refs(&chunks[0]).len(), 40);
    assert_ne!(chunks[0]["eventId"], chunks[1]["eventId"]);
    assert_eq!(
        chunks[0]["delivery"]["dedupeKey"],
        chunks[1]["delivery"]["dedupeKey"]
    );
    let rows = of_type(&h, "organizations.organization.suspended").await;
    assert_eq!(rows.iter().map(|p| p.chunk).collect::<Vec<_>>(), vec![0, 1]);
    assert!(rows.iter().all(|p| p.state == "ACKED"));
}

/// Gap audit G19: archive and reactivation reach the organization's ACTIVE members (never
/// the acting owner) through the same publication path — but only with the lifecycle
/// switch on (`ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED`, default off). While the
/// Notification Service catalogue does not carry the two types, they are recorded
/// `SKIPPED` (`NOT_IN_CATALOGUE`) and nothing is sent.
#[tokio::test]
async fn archive_and_reactivation_are_published_only_when_enabled() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org_a, owner_a, member_a, _) = small_org(&h, "notif-life-a").await;
    let (org_b, owner_b, member_b, _) = small_org(&h, "notif-life-b").await;
    // A: suspended by its owner, then reactivated (organization.reactivated).
    for action in ["suspend", "reactivate"] {
        h.user(owner_a)
            .post(
                &format!("/internal/v1/organizations/{org_a}/{action}"),
                json!({}),
            )
            .await
            .expect(200);
    }
    // B: archived by its owner (organization.archived).
    h.user(owner_b)
        .post(
            &format!("/internal/v1/organizations/{org_b}/archive"),
            json!({}),
        )
        .await
        .expect(200);
    let k = key("organizations-test-1");
    let fake = FakeI1::start(&k).await;

    // Default: the lifecycle events are not discovered at all.
    run_once(&h.db.conn, &sink(copy(&k), Some(&fake.url)))
        .await
        .unwrap();
    for t in [
        "organizations.organization.archived",
        "organizations.organization.reactivated",
    ] {
        assert!(of_type(&h, t).await.is_empty(), "{t} published while off");
    }

    // Enabled (after the pending G19 schema change, which releases apply together with the
    // switch): discovered through the same path.
    h.exec(LIFECYCLE_TYPES_SQL).await.unwrap();
    assert!(
        organizations::notification_publications::lifecycle_types_admitted(&h.db.conn)
            .await
            .unwrap()
    );
    let enabled = sink(copy(&k), Some(&fake.url)).with_lifecycle_events(true);
    run_once(&h.db.conn, &enabled).await.unwrap();
    for (t, org, member) in [
        ("organizations.organization.archived", &org_b, member_b),
        ("organizations.organization.reactivated", &org_a, member_a),
    ] {
        let rows = of_type(&h, t).await;
        assert_eq!(rows.len(), 1, "{t}");
        assert_eq!(rows[0].organization_id.to_string(), *org);
        match cf_notification_client::catalogue::spec(t) {
            None => assert_eq!(
                (rows[0].state.as_str(), rows[0].last_error.as_deref()),
                ("SKIPPED", Some("NOT_IN_CATALOGUE")),
                "{t}"
            ),
            Some(_) => {
                let env = rows[0].envelope.clone().expect("built");
                assert_eq!(refs(&env), vec![user_ref(member)], "{t}");
                assert_eq!(env["data"]["organizationId"], *org);
            }
        }
    }
    let sent: Vec<Value> = fake
        .envelopes()
        .into_iter()
        .filter(|e| {
            e["eventType"] == "organizations.organization.archived"
                || e["eventType"] == "organizations.organization.reactivated"
        })
        .collect();
    for e in &sent {
        // Never the acting owner.
        assert!(!refs(e).contains(&user_ref(owner_a)) && !refs(e).contains(&user_ref(owner_b)));
    }
    if cf_notification_client::catalogue::spec("organizations.organization.archived").is_none() {
        assert!(sent.is_empty(), "nothing the catalogue lacks is sent");
    }
    assert!(fake.invalid().is_empty());
}

/// The pending G19 migration's UP (migration/pending/m20261118_000007_lifecycle_notification_types.rs).
const LIFECYCLE_TYPES_SQL: &str = r#"
ALTER TABLE organization_notification_publications
    DROP CONSTRAINT ck_organization_notification_publications_event_type,
    ADD CONSTRAINT ck_organization_notification_publications_event_type CHECK (event_type IN (
        'organizations.invitation.created', 'organizations.invitation.accepted',
        'organizations.invitation.declined', 'organizations.invitation.revoked',
        'organizations.invitation.expired', 'organizations.membership.role_changed',
        'organizations.membership.suspended', 'organizations.membership.removed',
        'organizations.ownership.transferred', 'organizations.organization.suspended',
        'organizations.organization.archived', 'organizations.organization.reactivated'));
"#;

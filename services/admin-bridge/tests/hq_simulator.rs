//! HQ simulator (design §16): real TLS 1.3 mutual TLS against the real listener, real
//! HMAC v2 signatures and Ed25519 staff assertions, a test CA from rcgen and the
//! in-memory store. Covers the A1 acceptance: HQ reads; forged, replayed, expired,
//! out-of-scope, unenrolled and unaudited requests are refused.

mod common;

use common::*;
use std::path::PathBuf;

use admin_bridge::{
    contract::{catalog::catalog, model::*},
    store::DatasetDay,
    wire,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

// ---- Acceptance: reads -------------------------------------------------------------

#[tokio::test]
async fn discovery_reports_runtime_states_and_the_openapi_hash() {
    let h = start(Options::default()).await;
    let (status, v) = send(&h, &discovery_call()).await;
    assert_eq!(status, 200, "{v}");
    assert_envelope(&v, true);
    let r = &v["result"];
    assert_eq!(r["contract_version"], CONTRACT_VERSION);
    let committed =
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/contract/openapi.json"))
            .unwrap();
    assert_eq!(
        r["openapi_sha256"],
        wire::sha256_hex(&committed),
        "runtime hash must equal the committed openapi.json"
    );
    assert_eq!(r["policy_revision"], 1);
    let caps = r["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), catalog().len());
    let state = |id: &str| {
        caps.iter().find(|c| c["id"] == id).unwrap()["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(state("bridge.datasets.read"), "READ_ONLY");
    assert_eq!(state("meetings.reports.list"), "BLOCKED_DEPENDENCY");
    assert_eq!(state("verification.partner-go-live.approve"), "DISABLED");
    // audited before the answer
    let audit = h.store.audit_entries();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0.decision, "ALLOWED");
    assert_eq!(audit[0].0.actor, "hq:staff-0001");
    assert_eq!(
        audit[0].0.capability.as_deref(),
        Some("bridge.capabilities.read")
    );
}

#[tokio::test]
async fn datasets_are_null_not_zero_until_sources_report() {
    let h = start(Options::default()).await;
    let c = call(
        "/datasets/cfmeet.engagement_daily",
        &format!("{}{}", scoped("metrics-read"), window(7)),
        "bridge.datasets.read",
        &["cfmeet.viewer"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert_eq!(r["coverage"], "NONE");
    assert_eq!(v["freshness"], "UNAVAILABLE");
    let series = r["series"].as_array().unwrap();
    assert!(series.len() >= 7);
    for day in series {
        for m in ["dau", "mau", "signups"] {
            assert!(day["metrics"][m].is_null(), "{m} must be null, never zero");
        }
    }
    // one day fully reported -> PARTIAL with that day's values
    let today = (Utc::now() - Duration::minutes(1)).date_naive();
    let mut metrics = serde_json::Map::new();
    metrics.insert("dau".into(), json!(12));
    metrics.insert("mau".into(), json!(40));
    metrics.insert("signups".into(), json!(0));
    h.store.put_dataset_day(DatasetDay {
        dataset_id: "cfmeet.engagement_daily".into(),
        day: today,
        metrics,
        sources_reported: vec!["meeting-service:PULL".into()],
        updated_at: Utc::now(),
    });
    let mut c2 = c.clone();
    c2.jti = uuid::Uuid::new_v4().to_string();
    c2.nonce = hex::encode(rand_bytes());
    let (_, v) = send(&h, &c2).await;
    assert_eq!(v["result"]["coverage"], "PARTIAL");
    let last = v["result"]["series"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["metrics"]["dau"], 12);
    assert_eq!(last["metrics"]["signups"], 0, "a reported zero stays zero");
}

#[tokio::test]
async fn worker_identity_reads_datasets_with_the_workload_acr() {
    let h = start(Options::default()).await;
    let mut c = call(
        "/datasets/cfmeet.meetings_daily",
        &format!("{}{}", scoped("metrics-read"), window(31)),
        "bridge.datasets.read",
        &["cfmeet.source_sync"],
    );
    c.acr = "urn:cosmicforge:acr:workload";
    assert_eq!(send(&h, &c).await.0, 200);
    // the workload acr never belongs to a staff role ...
    let mut staff = discovery_call();
    staff.acr = "urn:cosmicforge:acr:workload";
    assert_eq!(code(&send(&h, &staff).await.1), "STAFF_ASSERTION_INVALID");
    // ... and the worker never uses the staff acr
    let mut w = c.clone();
    w.acr = "urn:cosmicforge:acr:mfa";
    w.jti = uuid::Uuid::new_v4().to_string();
    w.nonce = hex::encode(rand_bytes());
    assert_eq!(code(&send(&h, &w).await.1), "STAFF_ASSERTION_INVALID");
    // the worker cannot read discovery
    let mut d = call(
        "/capabilities",
        &scoped("admin-ui-capabilities"),
        "bridge.capabilities.read",
        &["cfmeet.source_sync"],
    );
    d.acr = "urn:cosmicforge:acr:workload";
    assert_eq!(code(&send(&h, &d).await.1), "PERMISSION_DENIED");
}

#[tokio::test]
async fn health_reports_unconfigured_owners_honestly() {
    let h = start(Options::default()).await;
    let c = call(
        "/health/aggregate",
        &scoped("metrics-read"),
        "bridge.health.read",
        &["cfmeet.viewer"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert!(DateTime::parse_from_rfc3339(r["as_of"].as_str().unwrap()).is_ok());
    // No owner probe is configured: the bridge vouches only for itself.
    assert_eq!(r["status"], "UNKNOWN");
    let services = r["services"].as_array().unwrap();
    assert_eq!(
        services.len(),
        1,
        "owners without a probe are omitted, never OK"
    );
    assert_eq!(services[0]["service"], "admin-bridge");
    assert_eq!(services[0]["status"], "OK");
    assert!(services[0]["checked_at"].is_string());
}

#[tokio::test]
async fn projection_status_is_unavailable_until_owner_events() {
    let h = start(Options::default()).await;
    let c = call(
        "/hq/projections/status",
        &scoped("ops-incident"),
        "hq.projections.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let list = v["result"]["projections"].as_array().unwrap();
    assert_eq!(list.len(), 9);
    assert!(list
        .iter()
        .all(|p| p["freshness"] == "UNAVAILABLE" && p["open_gaps"].is_null()));
}

// ---- Acceptance: refusals ----------------------------------------------------------

#[tokio::test]
async fn forged_signature_is_refused_and_audited() {
    let h = start(Options::default()).await;
    let mut c = discovery_call();
    c.hmac_key = b"not-the-enrolled-key-but-long-enough-0000".to_vec();
    let (status, v) = send(&h, &c).await;
    assert_eq!((status, code(&v)), (401, "SIGNATURE_INVALID"));
    assert_envelope(&v, false);
    assert_eq!(
        h.store.audit_entries().last().unwrap().0.decision,
        "SIGNATURE_INVALID"
    );
}

#[tokio::test]
async fn tampered_query_breaks_the_signature() {
    let h = start(Options::default()).await;
    let c = discovery_call();
    let mut hs = headers(&c);
    let mut tampered = c.clone();
    tampered.uri = c.uri.replace("GLOBAL", "NG");
    hs.retain(|_| true);
    let tls = client_config(
        &h.pki,
        Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
        false,
    );
    let (status, v) = send_with(&h, &tampered, tls, Some(hs)).await.unwrap();
    assert_eq!((status, code(&v)), (401, "SIGNATURE_INVALID"));
}

#[tokio::test]
async fn replayed_request_is_refused() {
    let h = start(Options::default()).await;
    let c = discovery_call();
    let hs = headers(&c);
    let tls = || {
        client_config(
            &h.pki,
            Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
            false,
        )
    };
    assert_eq!(
        send_with(&h, &c, tls(), Some(hs.clone())).await.unwrap().0,
        200
    );
    let (status, v) = send_with(&h, &c, tls(), Some(hs)).await.unwrap();
    assert_eq!((status, code(&v)), (409, "REPLAY"));
    // a new nonce with the same jti is a replay too
    let mut same_jti = c.clone();
    same_jti.nonce = hex::encode(rand_bytes());
    assert_eq!(code(&send(&h, &same_jti).await.1), "REPLAY");
}

#[tokio::test]
async fn expired_or_long_lived_assertions_are_refused() {
    let h = start(Options::default()).await;
    let mut expired = discovery_call();
    expired.iat_offset = -120;
    assert_eq!(code(&send(&h, &expired).await.1), "STAFF_ASSERTION_EXPIRED");
    let mut long = discovery_call();
    long.lifetime = 61;
    assert_eq!(code(&send(&h, &long).await.1), "STAFF_ASSERTION_INVALID");
    let mut skew = discovery_call();
    skew.hmac_ts_offset = -31;
    assert_eq!(code(&send(&h, &skew).await.1), "TIMESTAMP_SKEW");
}

#[tokio::test]
async fn digest_mismatch_is_refused() {
    let h = start(Options::default()).await;
    let mut c = discovery_call();
    c.digest_uri = Some(c.uri.replace("admin-ui-capabilities", "metrics-read"));
    assert_eq!(code(&send(&h, &c).await.1), "REQUEST_DIGEST_MISMATCH");
}

#[tokio::test]
async fn scopes_outside_the_assertion_or_the_enrolment_are_refused() {
    let h = start(Options::default()).await;
    // tenant in the request but not in the assertion
    let mut c = discovery_call();
    c.tenant_scopes = vec!["partner-tenant"];
    assert_eq!(code(&send(&h, &c).await.1), "SCOPE_DENIED");
    // tenant in the assertion but not enrolled for HQ
    let mut c2 = call(
        "/capabilities",
        "market=GLOBAL&organization=cfmeet&zone=production&tenant=partner-tenant&purpose=admin-ui-capabilities",
        "bridge.capabilities.read",
        &["cfmeet.viewer"],
    );
    c2.tenant_scopes = vec!["partner-tenant"];
    assert_eq!(code(&send(&h, &c2).await.1), "SCOPE_DENIED");
}

#[tokio::test]
async fn permission_role_and_purpose_must_all_be_granted() {
    let h = start(Options::default()).await;
    let mut no_perm = discovery_call();
    no_perm.permissions = vec!["bridge.health.read".into()];
    assert_eq!(code(&send(&h, &no_perm).await.1), "PERMISSION_DENIED");
    // finance may not read health at all
    let c = call(
        "/health/aggregate",
        &scoped("metrics-read"),
        "bridge.health.read",
        &["cfmeet.finance"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "PERMISSION_DENIED");
    // a purpose the capability does not list
    let c = call(
        "/capabilities",
        &scoped("metrics-read"),
        "bridge.capabilities.read",
        &["cfmeet.viewer"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "PURPOSE_NOT_ALLOWED");
    // unknown role strings are refused outright
    let c = call(
        "/capabilities",
        &scoped("admin-ui-capabilities"),
        "bridge.capabilities.read",
        &["cfmeet.root"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "STAFF_ASSERTION_INVALID");
}

#[tokio::test]
async fn revoked_sessions_and_staff_are_refused() {
    let h = start(Options::default()).await;
    h.store
        .revoke_session("sess-0001", Utc::now() - Duration::seconds(1));
    assert_eq!(code(&send(&h, &discovery_call()).await.1), "REVOKED");
    let mut other = discovery_call();
    other.session = "sess-0002".into();
    assert_eq!(send(&h, &other).await.0, 200);
    h.store
        .revoke_sub("staff-0001", Utc::now() - Duration::seconds(1));
    let mut again = discovery_call();
    again.session = "sess-0003".into();
    assert_eq!(code(&send(&h, &again).await.1), "REVOKED");
}

#[tokio::test]
async fn audit_failure_answers_503_with_no_data() {
    let h = start(Options::default()).await;
    h.store.fail_audit(true);
    let (status, v) = send(&h, &discovery_call()).await;
    assert_eq!((status, code(&v)), (503, "AUDIT_UNAVAILABLE"));
    assert_envelope(&v, false);
}

#[tokio::test]
async fn client_certificates_must_be_present_tls13_and_enrolled() {
    let h = start(Options::default()).await;
    // no client certificate: the handshake (or first read) fails
    let none = client_config(&h.pki, None, false);
    assert!(send_with(&h, &discovery_call(), none, None).await.is_err());
    // TLS 1.2 is refused
    let tls12 = client_config(
        &h.pki,
        Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
        true,
    );
    assert!(send_with(&h, &discovery_call(), tls12, None).await.is_err());
    // same CA and URI SAN, but not enrolled
    let other = client_config(
        &h.pki,
        Some((&h.pki.other_cert_pem, &h.pki.other_key_pem)),
        false,
    );
    let (status, v) = send_with(&h, &discovery_call(), other, None).await.unwrap();
    assert_eq!((status, code(&v)), (401, "CLIENT_CERTIFICATE_REFUSED"));
    // unauthenticated refusals are not audited
    assert!(h.store.audit_entries().is_empty());
}

#[tokio::test]
async fn unknown_routes_404_and_unserved_capabilities_503() {
    let h = start(Options::default()).await;
    let c = call(
        "/nope",
        &scoped("support-review"),
        "x.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_NOT_FOUND");
    let c = call(
        "/accounts/users",
        &format!("{}&q=ada", scoped("support-review")),
        "accounts.users.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_DISABLED");
}

#[tokio::test]
async fn disabled_bridge_and_missing_policy_refuse_everything() {
    let h = start(Options {
        enabled: false,
        ..Default::default()
    })
    .await;
    assert_eq!(
        code(&send(&h, &discovery_call()).await.1),
        "BRIDGE_DISABLED"
    );
    let h = start(Options {
        policy: false,
        ..Default::default()
    })
    .await;
    assert_eq!(
        code(&send(&h, &discovery_call()).await.1),
        "POLICY_UNAVAILABLE"
    );
}

#[tokio::test]
async fn rate_limit_applies_per_staff_permission_and_scope() {
    let h = start(Options::default()).await;
    for _ in 0..5 {
        assert_eq!(send(&h, &discovery_call()).await.0, 200);
    }
    assert_eq!(code(&send(&h, &discovery_call()).await.1), "RATE_LIMITED");
    let mut other = discovery_call();
    other.sub = "staff-0002".into();
    assert_eq!(send(&h, &other).await.0, 200);
}

// ---- Approval and MFA paths (on a test catalog where a command is served) ----------

fn served_command_catalog() -> Vec<Capability> {
    catalog()
        .into_iter()
        .map(|mut c| {
            if c.id == "verification.partner-go-live.approve"
                || c.id == "partners.webhook-delivery.retry"
            {
                c.served = true;
            }
            c
        })
        .collect()
}

const APPROVE: &str = "verification.partner-go-live.approve";

fn command(path: &str, permission: &str, purpose: &str, role: &'static str) -> Call {
    let mut c = call(path, &scoped(purpose), permission, &[role]);
    c.method = "POST";
    c.body = br#"{"expected_version":3}"#.to_vec();
    c.idempotency_key = Some(uuid::Uuid::new_v4().to_string());
    c
}

fn approval(
    capability: &str,
    requester: &str,
    approver: &str,
    decided: DateTime<Utc>,
    expires: DateTime<Utc>,
) -> Value {
    json!({
        "approval_id": uuid::Uuid::new_v4().to_string(),
        "capability": capability,
        "requester_ref": requester,
        "approver_ref": approver,
        "intent_digest": wire::sha256_hex(b"intent"),
        "decided_at": decided.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": expires.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

fn approve_call() -> Call {
    command(
        "/verification/cases/case-1/partner-go-live/approve",
        APPROVE,
        "verification-review",
        "cfmeet.verification.reviewer",
    )
}

async fn allowed(h: &Harness, c: &Call) -> bool {
    let before = h.store.audit_entries().len();
    let _ = send(h, c).await;
    h.store
        .audit_entries()
        .get(before)
        .is_some_and(|e| e.0.decision == "ALLOWED")
}

#[tokio::test]
async fn two_person_commands_need_a_valid_single_use_approval() {
    let h = start(Options {
        catalog: Some(served_command_catalog()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    let now = Utc::now();
    assert_eq!(
        code(&send(&h, &approve_call()).await.1),
        "APPROVAL_REQUIRED"
    );

    let mut wrong_cap = approve_call();
    wrong_cap.approval = Some(approval(
        "verification.app-review.approve",
        "staff-0001",
        "staff-0002",
        now,
        now + Duration::hours(12),
    ));
    assert_eq!(code(&send(&h, &wrong_cap).await.1), "APPROVAL_INVALID");

    let mut self_approved = approve_call();
    self_approved.approval = Some(approval(
        APPROVE,
        "staff-0001",
        "staff-0001",
        now,
        now + Duration::hours(1),
    ));
    assert_eq!(code(&send(&h, &self_approved).await.1), "APPROVAL_INVALID");

    let mut not_requester = approve_call();
    not_requester.approval = Some(approval(
        APPROVE,
        "staff-0009",
        "staff-0002",
        now,
        now + Duration::hours(1),
    ));
    assert_eq!(code(&send(&h, &not_requester).await.1), "APPROVAL_INVALID");

    let mut too_long = approve_call();
    too_long.approval = Some(approval(
        APPROVE,
        "staff-0001",
        "staff-0002",
        now - Duration::minutes(1),
        now + Duration::hours(12),
    ));
    assert_eq!(code(&send(&h, &too_long).await.1), "APPROVAL_INVALID");

    let mut expired = approve_call();
    expired.approval = Some(approval(
        APPROVE,
        "staff-0001",
        "staff-0002",
        now - Duration::hours(13),
        now - Duration::hours(1),
    ));
    assert_eq!(code(&send(&h, &expired).await.1), "APPROVAL_EXPIRED");

    let good = approval(
        APPROVE,
        "staff-0001",
        "staff-0002",
        now - Duration::minutes(5),
        now + Duration::hours(11),
    );
    let mut first = approve_call();
    first.approval = Some(good.clone());
    assert!(
        allowed(&h, &first).await,
        "a valid approval passes the chain"
    );
    let entry = h.store.audit_entries().last().unwrap().0.clone();
    assert_eq!(entry.approver.as_deref(), Some("hq:staff-0002"));
    assert_eq!(entry.approval_id.as_deref(), good["approval_id"].as_str());
    let mut reuse = approve_call();
    reuse.approval = Some(good);
    assert_eq!(code(&send(&h, &reuse).await.1), "APPROVAL_REUSED");
}

#[tokio::test]
async fn risky_commands_need_fresh_mfa_idempotency_and_switches() {
    let h = start(Options {
        catalog: Some(served_command_catalog()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    // R2 command: MFA older than 300 s is refused
    let retry = || {
        command(
            "/partners/workspaces/ws-1/webhook-deliveries/d-1/retry",
            "partners.webhook-delivery.retry",
            "ops-incident",
            "cfmeet.partner_ops",
        )
    };
    let mut stale = retry();
    stale.auth_age = 301;
    assert_eq!(code(&send(&h, &stale).await.1), "RECENT_MFA_REQUIRED");
    let mut no_key = retry();
    no_key.idempotency_key = None;
    assert_eq!(code(&send(&h, &no_key).await.1), "IDEMPOTENCY_KEY_REQUIRED");
    assert!(allowed(&h, &retry()).await);

    // writes off: the same command is disabled
    let off = start(Options {
        catalog: Some(served_command_catalog()),
        switches_all_on: false,
        ..Default::default()
    })
    .await;
    assert_eq!(code(&send(&off, &retry()).await.1), "CAPABILITY_DISABLED");
}

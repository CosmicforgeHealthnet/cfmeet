//! DATA-017 — the Developer Platform's report-only reference reconciliation: local
//! invariant breaks and references missing / mismatched at Platform Access are REPORTED,
//! nothing is repaired or deleted; the worker logs it at most once per interval.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::{reference_reconciliation, worker};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

/// Organization -> project -> SANDBOX environment of `owner` (through the API, so both
/// the Developer Platform and the in-memory Platform Access hold them).
async fn sandbox(c: &Control, owner: &str) -> (Uuid, Uuid, Uuid) {
    let o = c
        .req(
            As::User(owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let o = id(&o.json);
    let (p, e) = project_env(c, owner, o).await;
    (o, p, e)
}

/// Another project with a SANDBOX environment in organization `o`.
async fn project_env(c: &Control, owner: &str, o: Uuid) -> (Uuid, Uuid) {
    let p = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Backend" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let p = id(&p.json);
    let e = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/projects/{p}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    (p, id(&e.json))
}

fn check<'a>(
    r: &'a reference_reconciliation::ReferenceReport,
    name: &str,
) -> &'a reference_reconciliation::ReferenceCheck {
    r.checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}"))
}

const TABLES: &[&str] = &[
    "devplat_organizations",
    "devplat_project_profiles",
    "devplat_environment_profiles",
    "devplat_environment_refs",
    "devplat_workspace_members",
    "devplat_quota_restrictions",
];

async fn counts(c: &Control) -> Vec<i64> {
    let mut out = Vec::new();
    for t in TABLES {
        out.push(c.count(&format!("SELECT count(*) FROM {t}")).await);
    }
    out
}

#[tokio::test]
async fn local_and_remote_reference_breaks_are_reported_and_nothing_is_deleted() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    let (project_b, env_b) = project_env(&c, &owner, org).await;

    // Consistent: every check OK, remote verification finds everything.
    let clean = reference_reconciliation::full_report(&c.db.conn, Some(c.pa.as_ref()), 100)
        .await
        .unwrap();
    assert_eq!(clean.anomalies, 0, "{clean:#?}");
    assert!(clean.checks.iter().all(|k| k.status == "OK"));
    assert_eq!(check(&clean, "environment_profiles_to_projects").rows, 2);
    assert_eq!(
        check(&clean, "workspace_member_accounts").distinct_foreign_ids,
        1
    );
    let remote = clean.remote.as_ref().unwrap();
    assert_eq!(
        (
            remote.examined,
            remote.missing,
            remote.mismatched,
            remote.unavailable
        ),
        (5, 0, 0, 0),
        "1 organization, 2 projects, 2 environments"
    );
    assert_eq!(
        clean.remote_verification,
        "PERFORMED: Platform Access delegated reads"
    );

    // Local break: the environment is also recorded under ANOTHER tenant / application.
    c.db.conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_quota_restrictions (id, tenant_id, application_id, environment_id, subject_kind, \
               subject_id, api_requests_per_minute, updated_by, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', '{env}', 'CREDENTIAL', '{}', 10, 'test', now(), now())",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4()
        ))
        .await
        .unwrap();
    // Remote breaks: project B's environment vanished at Platform Access; project B now
    // belongs to another tenant there.
    c.pa.envs.lock().unwrap().remove(&env_b);
    c.pa.apps
        .lock()
        .unwrap()
        .get_mut(&project_b)
        .unwrap()
        .tenant = Uuid::new_v4();
    let before = counts(&c).await;
    let delegated_before = c.pa.audit.lock().unwrap().len();
    let r = reference_reconciliation::full_report(&c.db.conn, Some(c.pa.as_ref()), 100)
        .await
        .unwrap();
    assert_eq!(check(&r, "environment_single_owner").anomalies, 1, "{r:#?}");
    assert_eq!(check(&r, "quota_restriction_environments").anomalies, 1);
    assert_eq!(check(&r, "environment_single_owner").status, "ATTENTION");
    assert_eq!(check(&r, "environment_profiles_to_projects").anomalies, 0);
    let remote = r.remote.as_ref().unwrap();
    assert_eq!((remote.missing, remote.mismatched), (1, 1), "{remote:?}");
    let mut findings: Vec<_> = remote
        .findings
        .iter()
        .map(|f| (f.kind, f.id, f.problem))
        .collect();
    findings.sort();
    assert_eq!(
        findings,
        vec![
            ("APPLICATION", project_b, "MISMATCHED"),
            ("ENVIRONMENT", env_b, "MISSING")
        ]
    );
    assert_eq!(r.anomalies, 4);
    // Report only: every row is still there, Platform Access was not asked to change
    // anything.
    assert_eq!(counts(&c).await, before);
    assert_eq!(c.pa.audit.lock().unwrap().len(), delegated_before);
    // JSON report: ids and counters only.
    let text = serde_json::to_string(&r).unwrap();
    assert!(text.contains("\"remoteVerification\""));
    assert!(!text.contains("owner@acme.example"));

    // Platform Access down: nothing is counted as missing, the report says PARTIAL.
    c.pa.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = reference_reconciliation::full_report(&c.db.conn, Some(c.pa.as_ref()), 100)
        .await
        .unwrap();
    let remote = r.remote.as_ref().unwrap();
    assert_eq!((remote.missing, remote.mismatched), (0, 0));
    assert_eq!(remote.unavailable, 5);
    assert!(r.remote_verification.starts_with("PARTIAL"));
    c.pa.down.store(false, std::sync::atomic::Ordering::SeqCst);

    // Without Platform Access: local checks only.
    let local = reference_reconciliation::report(&c.db.conn).await.unwrap();
    assert!(local.remote.is_none());
    assert!(local.remote_verification.starts_with("NOT_PERFORMED"));
    assert_eq!(local.anomalies, 2);
    let _ = project;
}

#[tokio::test]
async fn local_scope_checks_cover_every_holder() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (_, project, env) = sandbox(&c, &owner).await;
    // An environment profile whose tenant is not its project's tenant.
    c.db.conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_environment_profiles (environment_id, application_id, tenant_id, kind, \
               display_name, status, production_access_state, created_by, created_at, updated_at) \
             VALUES ('{}', '{project}', '{}', 'TEST', 'Rogue', 'ACTIVE', 'NOT_APPLICABLE', 'test', now(), now())",
            Uuid::new_v4(),
            Uuid::new_v4()
        ))
        .await
        .unwrap();
    // A credential lifecycle notice recorded under another tenant for a known environment.
    c.db.conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_credential_lifecycle_notices (credential_id, event_type, threshold_days, \
               expires_at, tenant_id, application_id, environment_id, service_account_id, source_event_id, \
               webhook_event_id, deliveries, notified_at) \
             VALUES ('{}', 'credential.expired', 0, now(), '{}', '{project}', '{env}', '{}', \
               'evt_00000000000000000000000000000001', 'whe_00000000000000000000000000000001', 0, now())",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4()
        ))
        .await
        .unwrap();
    let r = reference_reconciliation::report(&c.db.conn).await.unwrap();
    assert_eq!(
        check(&r, "environment_profiles_to_projects").anomalies,
        1,
        "{r:#?}"
    );
    assert_eq!(check(&r, "credential_notice_environments").anomalies, 1);
    assert_eq!(check(&r, "environment_single_owner").anomalies, 1);
    for name in [
        "webhook_subscription_environments",
        "webhook_event_environments",
        "quota_restriction_environments",
        "usage_fact_environments",
        "production_environments",
        "workspace_member_accounts",
    ] {
        assert_eq!(check(&r, name).anomalies, 0, "{name}");
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_environment_profiles")
            .await,
        2,
        "nothing repaired"
    );

    // The worker logs the report at most once per interval (several instances).
    let interval = std::time::Duration::from_secs(3600);
    let (a, b) = tokio::join!(
        worker::reference_reconciliation_pass(&c.db.conn, None, interval, 0),
        worker::reference_reconciliation_pass(&c.db.conn, None, interval, 0),
    );
    assert!(
        a.is_some() != b.is_some(),
        "exactly one instance reconciled"
    );
    assert_eq!(a.or(b).unwrap().anomalies, 3);
    assert!(
        worker::reference_reconciliation_pass(&c.db.conn, None, interval, 0)
            .await
            .is_none(),
        "not due yet"
    );
    assert_eq!(
        c.count(
            "SELECT count(*) FROM devplat_worker_schedules WHERE job = 'reference-reconciliation' \
                 AND last_outcome = 'ATTENTION' AND last_completed_at IS NOT NULL"
        )
        .await,
        1
    );
}

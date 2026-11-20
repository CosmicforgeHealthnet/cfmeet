//! PostgreSQL behaviour of `PgStore` and the schema guards (triggers, constraints).
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets its
//! own database cloned from a freshly migrated template. Without it the tests FAIL, unless
//! `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED line).

use admin_bridge::store::{
    chain_hash,
    pg::{verify_audit_chain, PgStore},
    AuditDraft, Claim, ClaimKind, ClaimOutcome, PolicyRow, PrincipalKind, RateKey, Store,
    GENESIS_HASH,
};
use admin_bridge_migration::MigratorTrait;
use chrono::{Duration, NaiveDate, SubsecRound, TimeZone, Utc};
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, Value as DbValue,
};
use serde_json::json;
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

fn admin_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!("SKIPPED: database integration test (CFMEET_SKIP_DB_TESTS=1, TEST_DATABASE_URL unset)");
                None
            } else {
                panic!(
                    "TEST_DATABASE_URL is not set. Admin bridge store tests require PostgreSQL \
                     (e.g. TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres). \
                     Set CFMEET_SKIP_DB_TESTS=1 to skip explicitly."
                )
            }
        }
    }
}

fn with_db(admin: &str, db: &str) -> String {
    let (base, _) = admin
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL must include a database name");
    format!("{base}/{db}")
}

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

async fn template(admin: &str) -> String {
    TEMPLATE
        .get_or_init(|| async {
            let name = format!(
                "abr_tmpl_{}_{}",
                std::process::id(),
                Uuid::new_v4().simple()
            );
            let conn = Database::connect(admin).await.expect("connect admin db");
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            admin_bridge_migration::Migrator::up(&tmpl, None)
                .await
                .expect("migrations apply");
            tmpl.close().await.ok();
            name
        })
        .await
        .clone()
}

struct TestDb {
    store: PgStore,
    admin: String,
    name: String,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        let name = format!("abr_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        {
            // CREATE DATABASE ... TEMPLATE needs the template without other sessions.
            let _g = CREATE_LOCK.lock().await;
            let conn = Database::connect(&admin).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        let store = PgStore::connect(&with_db(&admin, &name), 5)
            .await
            .expect("connect test db");
        Some(Self { store, admin, name })
    }

    fn db(&self) -> &DatabaseConnection {
        self.store.connection()
    }

    async fn exec(&self, sql: &str, values: Vec<DbValue>) -> Result<u64, sea_orm::DbErr> {
        self.db()
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await
            .map(|r| r.rows_affected())
    }

    async fn drop(self) {
        self.store.connection().clone().close().await.ok();
        if let Ok(conn) = Database::connect(&self.admin).await {
            let _ = conn
                .execute_unprepared(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    self.name
                ))
                .await;
            conn.close().await.ok();
        }
    }
}

const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn draft(n: u16) -> AuditDraft {
    AuditDraft {
        correlation_id: Uuid::new_v4(),
        // Nanoseconds on purpose: the store hashes the microsecond value it stores.
        occurred_at: Utc.timestamp_opt(1_795_000_000, 123_456_789).unwrap()
            + Duration::seconds(n.into()),
        actor: "hq:staff-0001".into(),
        session_id: Some("sess-1".into()),
        capability: Some("bridge.datasets.read".into()),
        method: "GET".into(),
        path: "/internal/cfmeet-admin/v1/datasets/cfmeet.engagement_daily".into(),
        purpose: Some("metrics-read".into()),
        // Key order that jsonb would rewrite (it sorts by length): kept exact by `json`.
        scope: json!({"market": "GLOBAL", "organization": "cfmeet", "zone": "production", "tenant": "t1"}),
        decision: "ALLOWED".into(),
        http_status: 200,
        request_digest: Some(HEX_A.into()),
        approval_id: None,
        approver: None,
        detail: json!({"role": "cfmeet.viewer", "risk": "R0", "policy_revision": 1}),
    }
}

async fn enroll_cert(db: &TestDb, fp: &str) {
    db.exec(
        "INSERT INTO admin_remote_principals (principal, kind, key_id, material, not_before, expires_at, created_by) \
         VALUES ('cosmicforge-hq-cfmeet-admin', 'CLIENT_CERT', $1, $1, now() - interval '1 hour', now() + interval '1 day', 'test')",
        vec![fp.into()],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn principals_are_looked_up_and_revoked_only_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    enroll_cert(&db, HEX_A).await;
    let p = db
        .store
        .principal(PrincipalKind::ClientCert, HEX_A)
        .await
        .unwrap()
        .expect("enrolled");
    assert!(p.live_at(Utc::now()));
    assert!(db
        .store
        .principal(PrincipalKind::HmacKey, HEX_A)
        .await
        .unwrap()
        .is_none());

    // Only revoked_at may change, and only once.
    assert!(db
        .exec(
            "UPDATE admin_remote_principals SET material = $1 WHERE key_id = $2",
            vec![HEX_B.into(), HEX_A.into()]
        )
        .await
        .is_err());
    assert!(db.exec("UPDATE admin_remote_principals SET expires_at = now() + interval '9 days' WHERE key_id = $1", vec![HEX_A.into()]).await.is_err());
    assert_eq!(
        db.exec(
            "UPDATE admin_remote_principals SET revoked_at = now() WHERE key_id = $1",
            vec![HEX_A.into()]
        )
        .await
        .unwrap(),
        1
    );
    assert!(db
        .exec(
            "UPDATE admin_remote_principals SET revoked_at = now() WHERE key_id = $1",
            vec![HEX_A.into()]
        )
        .await
        .is_err());
    assert!(db
        .exec(
            "UPDATE admin_remote_principals SET revoked_at = NULL WHERE key_id = $1",
            vec![HEX_A.into()]
        )
        .await
        .is_err());
    assert!(db
        .exec("DELETE FROM admin_remote_principals", vec![])
        .await
        .is_err());
    assert!(db
        .exec("TRUNCATE admin_remote_principals", vec![])
        .await
        .is_err());
    let p = db
        .store
        .principal(PrincipalKind::ClientCert, HEX_A)
        .await
        .unwrap()
        .unwrap();
    assert!(!p.live_at(Utc::now() + Duration::seconds(1)));

    // A staff key needs issuer and audience; others must not carry them.
    assert!(db
        .exec(
            "INSERT INTO admin_remote_principals (principal, kind, key_id, material, not_before, expires_at, created_by) \
             VALUES ('cosmicforge-hq-cfmeet-admin', 'STAFF_KEY', 'kid-1', $1, now(), now() + interval '1 day', 'test')",
            vec![HEX_B.into()],
        )
        .await
        .is_err());
    db.drop().await;
}

#[tokio::test]
async fn claims_are_all_or_nothing_and_replays_refused() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let keep = Utc::now() + Duration::minutes(11);
    let c = |kind, v: &str| Claim {
        kind,
        value: v.into(),
        keep_until: keep,
    };
    assert_eq!(
        db.store
            .claim_once(&[c(ClaimKind::Nonce, "n1"), c(ClaimKind::Jti, "j1")])
            .await
            .unwrap(),
        ClaimOutcome::Recorded
    );
    assert_eq!(
        db.store
            .claim_once(&[c(ClaimKind::Nonce, "n2"), c(ClaimKind::Jti, "j1")])
            .await
            .unwrap(),
        ClaimOutcome::Replay(ClaimKind::Jti)
    );
    // n2 was rolled back with the refused batch.
    assert_eq!(
        db.store
            .claim_once(&[c(ClaimKind::Nonce, "n2")])
            .await
            .unwrap(),
        ClaimOutcome::Recorded
    );
    // The same value under another kind is a different claim; a duplicate inside one batch is a replay.
    assert_eq!(
        db.store
            .claim_once(&[c(ClaimKind::Approval, "n1")])
            .await
            .unwrap(),
        ClaimOutcome::Recorded
    );
    assert_eq!(
        db.store
            .claim_once(&[c(ClaimKind::Nonce, "n3"), c(ClaimKind::Nonce, "n3")])
            .await
            .unwrap(),
        ClaimOutcome::Replay(ClaimKind::Nonce)
    );
    db.drop().await;
}

#[tokio::test]
async fn rate_windows_count_per_bucket() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let w = Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap();
    let k = RateKey {
        bucket: HEX_A.into(),
        window_start: w,
    };
    assert!(db.store.rate_allow(&k, 2).await.unwrap());
    assert!(db.store.rate_allow(&k, 2).await.unwrap());
    assert!(!db.store.rate_allow(&k, 2).await.unwrap());
    let next = RateKey {
        bucket: HEX_A.into(),
        window_start: w + Duration::minutes(1),
    };
    assert!(db.store.rate_allow(&next, 2).await.unwrap());
    let other = RateKey {
        bucket: HEX_B.into(),
        window_start: w,
    };
    assert!(db.store.rate_allow(&other, 1).await.unwrap());
    db.drop().await;
}

#[tokio::test]
async fn revocations_by_staff_member_and_by_session() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let now = Utc::now();
    assert!(!db.store.is_revoked("staff-1", "sess-1", now).await.unwrap());
    db.exec(
        "INSERT INTO admin_revocations (sub, session_id, revoked_at, reason_code, created_by) VALUES ('staff-1', 'sess-1', $1, 'LOST_DEVICE', 'test')",
        vec![now.into()],
    )
    .await
    .unwrap();
    assert!(db.store.is_revoked("staff-1", "sess-1", now).await.unwrap());
    assert!(
        !db.store.is_revoked("staff-1", "sess-2", now).await.unwrap(),
        "only that session"
    );
    db.exec(
        "INSERT INTO admin_revocations (sub, session_id, revoked_at, reason_code, created_by) VALUES ('staff-2', NULL, $1, 'OFFBOARDED', 'test')",
        vec![(now + Duration::minutes(5)).into()],
    )
    .await
    .unwrap();
    assert!(
        !db.store.is_revoked("staff-2", "any", now).await.unwrap(),
        "not yet effective"
    );
    assert!(db
        .store
        .is_revoked("staff-2", "any", now + Duration::minutes(5))
        .await
        .unwrap());
    assert!(db
        .exec("DELETE FROM admin_revocations", vec![])
        .await
        .is_err());
    assert!(db
        .exec("UPDATE admin_revocations SET reason_code = 'X_Y'", vec![])
        .await
        .is_err());
    db.drop().await;
}

#[tokio::test]
async fn policy_revisions_round_trip_and_are_immutable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let doc = br#"{"policy":"cfmeet-admin-bridge.access-policy.v1","revision":1}"#.to_vec();
    let sig = "ab".repeat(64);
    let digest = admin_bridge::wire::sha256_hex(&doc);
    // The stored digest must match the stored bytes.
    assert!(db
        .exec(
            "INSERT INTO admin_policy_revisions (revision, document, signature, key_id, document_sha256, created_by) VALUES (1, $1, $2, 'policy-1', $3, 'test')",
            vec![doc.clone().into(), sig.clone().into(), HEX_A.into()],
        )
        .await
        .is_err());
    db.exec(
        "INSERT INTO admin_policy_revisions (revision, document, signature, key_id, document_sha256, created_by) VALUES (1, $1, $2, 'policy-1', $3, 'test')",
        vec![doc.clone().into(), sig.clone().into(), digest.into()],
    )
    .await
    .unwrap();
    assert_eq!(
        db.store.policy_revision(1).await.unwrap(),
        Some(PolicyRow {
            revision: 1,
            document: doc,
            signature: sig,
            key_id: "policy-1".into()
        })
    );
    assert_eq!(db.store.policy_revision(2).await.unwrap(), None);
    assert!(db
        .exec("UPDATE admin_policy_revisions SET key_id = 'other'", vec![])
        .await
        .is_err());
    assert!(db
        .exec("DELETE FROM admin_policy_revisions", vec![])
        .await
        .is_err());
    db.drop().await;
}

#[tokio::test]
async fn audit_is_hash_chained_and_append_only() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (d1, d2, d3) = (draft(1), draft(2), draft(3));
    let r1 = db.store.append_audit(&d1).await.unwrap();
    let r2 = db.store.append_audit(&d2).await.unwrap();
    let r3 = db.store.append_audit(&d3).await.unwrap();
    // Hashes cover the microsecond timestamp actually stored.
    let stored = |d: &AuditDraft| {
        let mut d = d.clone();
        d.occurred_at = Utc
            .timestamp_opt(
                d.occurred_at.timestamp(),
                d.occurred_at.timestamp_subsec_micros() * 1000,
            )
            .unwrap();
        d
    };
    assert_eq!(r1.entry_hash, chain_hash(GENESIS_HASH, &stored(&d1)));
    assert_eq!(r2.entry_hash, chain_hash(&r1.entry_hash, &stored(&d2)));
    assert_eq!(r3.entry_hash, chain_hash(&r2.entry_hash, &stored(&d3)));
    assert!(r1.sequence < r2.sequence && r2.sequence < r3.sequence);
    // Read-back reproduces every hash (exact json text, microsecond timestamps).
    assert_eq!(verify_audit_chain(&db.store).await.unwrap(), Ok(3));

    assert!(db
        .exec(
            "UPDATE admin_audit_entries SET decision = 'PERMISSION_DENIED'",
            vec![]
        )
        .await
        .is_err());
    assert!(db
        .exec("DELETE FROM admin_audit_entries", vec![])
        .await
        .is_err());
    assert!(db
        .exec("TRUNCATE admin_audit_entries", vec![])
        .await
        .is_err());

    // A row that does not continue the chain is refused by the trigger.
    let forged = db
        .exec(
            "INSERT INTO admin_audit_entries (entry_hash, previous_hash, correlation_id, occurred_at, actor, method, path, scope, decision, http_status, detail) \
             VALUES ($1, $2, $3, now(), 'hq:x', 'GET', '/x', 'null'::json, 'ALLOWED', 200, 'null'::json)",
            vec![HEX_B.into(), r1.entry_hash.clone().into(), Uuid::new_v4().into()],
        )
        .await;
    assert!(
        forged.is_err(),
        "previous_hash must be the last entry's hash"
    );
    assert_eq!(verify_audit_chain(&db.store).await.unwrap(), Ok(3));
    db.drop().await;
}

#[tokio::test]
async fn concurrent_audit_appends_keep_one_chain() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut tasks = Vec::new();
    for i in 0..12u16 {
        let store = db.store.clone();
        tasks.push(tokio::spawn(async move {
            store.append_audit(&draft(i)).await.unwrap()
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(verify_audit_chain(&db.store).await.unwrap(), Ok(12));
    db.drop().await;
}

#[tokio::test]
async fn cleanup_removes_expired_claims_and_old_buckets() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let now = Utc::now();
    let claim = |v: &str, keep| Claim {
        kind: ClaimKind::Jti,
        value: v.into(),
        keep_until: keep,
    };
    db.store
        .claim_once(&[
            claim("old", now - Duration::seconds(1)),
            claim("live", now + Duration::minutes(5)),
        ])
        .await
        .unwrap();
    db.store
        .rate_allow(
            &RateKey {
                bucket: HEX_A.into(),
                window_start: now - Duration::hours(2),
            },
            10,
        )
        .await
        .unwrap();
    db.store
        .rate_allow(
            &RateKey {
                bucket: HEX_A.into(),
                window_start: now - Duration::minutes(1),
            },
            10,
        )
        .await
        .unwrap();
    let stats = db.store.cleanup(now).await.unwrap();
    assert_eq!((stats.claims, stats.rate_buckets), (1, 1));
    // The expired jti may be used again; the live one may not.
    assert_eq!(
        db.store
            .claim_once(&[claim("old", now + Duration::minutes(5))])
            .await
            .unwrap(),
        ClaimOutcome::Recorded
    );
    assert_eq!(
        db.store
            .claim_once(&[claim("live", now + Duration::minutes(5))])
            .await
            .unwrap(),
        ClaimOutcome::Replay(ClaimKind::Jti)
    );
    db.drop().await;
}

#[tokio::test]
async fn dataset_days_are_ordered_and_limited() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    for day in [3, 1, 2, 5] {
        db.exec(
            "INSERT INTO admin_dataset_days (dataset_id, day, metrics, sources_reported) \
             VALUES ('cfmeet.engagement_daily', $1, '{\"dau\": 10, \"mau\": null}'::jsonb, ARRAY['meeting-service:PULL'])",
            vec![NaiveDate::from_ymd_opt(2026, 11, day).unwrap().into()],
        )
        .await
        .unwrap();
    }
    let d = |n| NaiveDate::from_ymd_opt(2026, 11, n).unwrap();
    let days = db
        .store
        .dataset_days("cfmeet.engagement_daily", d(1), d(4), 2)
        .await
        .unwrap();
    assert_eq!(
        days.iter().map(|x| x.day).collect::<Vec<_>>(),
        vec![d(1), d(2)]
    );
    assert_eq!(days[0].metrics["dau"], json!(10));
    assert_eq!(days[0].metrics["mau"], json!(null));
    assert_eq!(
        days[0].sources_reported,
        vec!["meeting-service:PULL".to_string()]
    );
    assert!(db
        .store
        .dataset_days("cfmeet.meetings_daily", d(1), d(30), 100)
        .await
        .unwrap()
        .is_empty());
    assert!(db
        .exec("INSERT INTO admin_dataset_days (dataset_id, day, metrics) VALUES ('cfmeet.x', $1, '[]'::jsonb)", vec![d(9).into()])
        .await
        .is_err());

    db.exec("INSERT INTO admin_projections (name, open_gaps, quarantined) VALUES ('cfmeet.verification_queue', 0, 2), ('cfmeet.apps_daily', 1, 0)", vec![])
        .await
        .unwrap();
    let names: Vec<String> = db
        .store
        .projections()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(
        names,
        vec!["cfmeet.apps_daily", "cfmeet.verification_queue"]
    );
    db.drop().await;
}

#[tokio::test]
async fn migrations_create_only_admin_tables_and_roll_back() {
    let Some(admin) = admin_url() else { return };
    let name = format!("abr_m_{}_{}", std::process::id(), Uuid::new_v4().simple());
    let conn = Database::connect(&admin).await.unwrap();
    conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let db = Database::connect(with_db(&admin, &name)).await.unwrap();
    admin_bridge_migration::Migrator::up(&db, None)
        .await
        .unwrap();
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM pg_tables WHERE schemaname = 'public' \
             AND tablename <> 'admin_bridge_seaql_migrations' AND tablename !~ '^admin_'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    admin_bridge_migration::Migrator::down(&db, None)
        .await
        .unwrap();
    admin_bridge_migration::Migrator::up(&db, None)
        .await
        .unwrap();
    db.close().await.ok();
    let _ = conn
        .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .await;
}

fn operation(key: Uuid, target: &str, version: &str) -> admin_bridge::store::Operation {
    admin_bridge::store::Operation {
        principal_sub: "staff-0001".into(),
        idempotency_key: key,
        operation_id: key,
        capability: "partners.webhook-delivery.retry".into(),
        owner: "developer-platform".into(),
        purpose: "ops-incident".into(),
        // Key order kept exact by `json` (the readback compares scopes).
        scope: json!({"market": "GLOBAL", "organization": "cfmeet", "zone": "production", "tenant": "t1"}),
        intent_sha256: HEX_A.into(),
        target: target.into(),
        expected_version: version.into(),
        approval_id: None,
        correlation_id: Uuid::new_v4(),
        status: admin_bridge::store::OpStatus::Pending,
        source_status: None,
        source_version: None,
        error_code: None,
        created_at: Utc.timestamp_opt(1_795_000_000, 123_456_789).unwrap(),
        sent_at: None,
        settled_at: None,
    }
}

#[tokio::test]
async fn operations_are_sent_once_settled_once_and_never_deleted() {
    use admin_bridge::store::{BeginOutcome, OpStatus, Settlement};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.store;
    let key = Uuid::new_v4();
    let op = operation(key, "t:1", "3");
    assert_eq!(s.begin_operation(&op).await.unwrap(), BeginOutcome::Started);
    let stored = s.operation("staff-0001", key).await.unwrap().unwrap();
    assert_eq!(stored.status, OpStatus::Pending);
    assert_eq!(stored.scope, op.scope);
    assert_eq!(stored.created_at, op.created_at.trunc_subsecs(6));
    // Same key: the stored row comes back. Same target and version under another key: taken.
    assert!(matches!(
        s.begin_operation(&op).await.unwrap(),
        BeginOutcome::Existing(_)
    ));
    assert_eq!(
        s.begin_operation(&operation(Uuid::new_v4(), "t:1", "3"))
            .await
            .unwrap(),
        BeginOutcome::VersionTaken
    );
    // Another staff member's key space is separate.
    let mut other = operation(key, "t:2", "1");
    other.principal_sub = "staff-0002".into();
    assert_eq!(
        s.begin_operation(&other).await.unwrap(),
        BeginOutcome::Started
    );

    let now = Utc::now();
    assert!(s.mark_sent("staff-0001", key, now).await.unwrap());
    assert!(
        !s.mark_sent("staff-0001", key, now).await.unwrap(),
        "sent once"
    );
    let unknown = Settlement {
        status: OpStatus::UnknownOutcome,
        source_status: None,
        source_version: None,
        error_code: Some("UNKNOWN_OUTCOME".into()),
        at: now,
    };
    assert!(s
        .settle_operation("staff-0001", key, &unknown)
        .await
        .unwrap());
    let done = Settlement {
        status: OpStatus::Succeeded,
        source_status: Some("RETRY_QUEUED".into()),
        source_version: Some("4".into()),
        error_code: None,
        at: now,
    };
    assert!(s.settle_operation("staff-0001", key, &done).await.unwrap());
    let failed = Settlement {
        status: OpStatus::Failed,
        ..done.clone()
    };
    assert!(
        !s.settle_operation("staff-0001", key, &failed)
            .await
            .unwrap(),
        "settled rows never change"
    );
    let row = s.operation("staff-0001", key).await.unwrap().unwrap();
    assert_eq!(
        (row.status, row.source_version.as_deref()),
        (OpStatus::Succeeded, Some("4"))
    );
    assert!(row.settled_at.is_some());

    // The trigger refuses what the store never does.
    assert!(db
        .exec("UPDATE admin_operations SET status = 'FAILED' WHERE idempotency_key = $1 AND principal_sub = 'staff-0001'", vec![key.into()])
        .await
        .is_err());
    assert!(db
        .exec(
            "UPDATE admin_operations SET target = 'x' WHERE principal_sub = 'staff-0002'",
            vec![]
        )
        .await
        .is_err());
    assert!(db
        .exec("UPDATE admin_operations SET status = 'SUCCEEDED', sent_at = now(), settled_at = now() WHERE principal_sub = 'staff-0002'", vec![])
        .await
        .is_err(), "PENDING cannot skip SENT");
    assert!(db
        .exec("DELETE FROM admin_operations", vec![])
        .await
        .is_err());
    assert!(db.exec("TRUNCATE admin_operations", vec![]).await.is_err());

    // A FAILED operation frees its target version.
    let k2 = Uuid::new_v4();
    let pending_fail = Settlement {
        status: OpStatus::Failed,
        source_status: Some("NOT_SENT".into()),
        source_version: None,
        error_code: Some("BRIDGE_UNAVAILABLE".into()),
        at: now,
    };
    assert_eq!(
        s.begin_operation(&operation(k2, "t:3", "1")).await.unwrap(),
        BeginOutcome::Started
    );
    assert!(s
        .settle_operation("staff-0001", k2, &pending_fail)
        .await
        .unwrap());
    assert_eq!(
        s.begin_operation(&operation(Uuid::new_v4(), "t:3", "1"))
            .await
            .unwrap(),
        BeginOutcome::Started
    );

    // Open rows, oldest first; settled ones are not listed.
    let open = s
        .open_operations(now + Duration::days(1), 10)
        .await
        .unwrap();
    assert_eq!(open.len(), 2);
    assert!(open.iter().all(|o| !o.status.settled()));
    db.drop().await;
}

#[tokio::test]
async fn privacy_requests_change_with_their_operation_in_one_transaction() {
    use admin_bridge::store::{LocalChange, LocalOutcome, OpStatus};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.store;
    let key = Uuid::new_v4();
    let mut register = operation(key, &format!("admin-bridge:privacy-request:{key}"), "0");
    register.capability = "privacy.request.register".into();
    register.owner = "admin-bridge".into();
    let subject = Uuid::new_v4();
    let out = s
        .local_command(
            &register,
            &LocalChange::PrivacyRegister {
                kind: "ERASURE".into(),
                subject_ref: subject,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        LocalOutcome::Applied {
            status: "OPEN".into(),
            version: 1
        }
    );
    let op = s.operation("staff-0001", key).await.unwrap().unwrap();
    assert_eq!(
        (op.status, op.source_version.as_deref()),
        (OpStatus::Succeeded, Some("1"))
    );
    let p = s.privacy_request(key).await.unwrap().unwrap();
    assert_eq!(
        (
            p.kind.as_str(),
            p.state.as_str(),
            p.subject_ref,
            p.services.len()
        ),
        ("ERASURE", "OPEN", subject, 7)
    );
    assert!(matches!(
        s.local_command(
            &register,
            &LocalChange::PrivacyRegister {
                kind: "ERASURE".into(),
                subject_ref: subject
            }
        )
        .await
        .unwrap(),
        LocalOutcome::Existing(_)
    ));

    // A stale version is refused and recorded FAILED with nothing changed.
    let k2 = Uuid::new_v4();
    let mut record = operation(k2, &format!("admin-bridge:privacy-request:{key}"), "5");
    record.owner = "admin-bridge".into();
    let stale = LocalChange::PrivacyRecord {
        request_id: key,
        service: "messaging".into(),
        outcome: "DONE".into(),
        expected_version: 5,
    };
    assert_eq!(
        s.local_command(&record, &stale).await.unwrap(),
        LocalOutcome::Refused("VERSION_CONFLICT")
    );
    assert_eq!(
        s.operation("staff-0001", k2).await.unwrap().unwrap().status,
        OpStatus::Failed
    );
    assert_eq!(s.privacy_request(key).await.unwrap().unwrap().version, 1);

    // Every service's outcome completes the request.
    let mut version = 1;
    for service in admin_bridge::store::PRIVACY_SERVICES {
        let k = Uuid::new_v4();
        let mut op = operation(
            k,
            &format!("admin-bridge:privacy-request:{key}"),
            &version.to_string(),
        );
        op.owner = "admin-bridge".into();
        let change = LocalChange::PrivacyRecord {
            request_id: key,
            service: service.into(),
            outcome: "DONE".into(),
            expected_version: version,
        };
        match s.local_command(&op, &change).await.unwrap() {
            LocalOutcome::Applied { version: v, .. } => version = v,
            other => panic!("{service}: {other:?}"),
        }
    }
    let p = s.privacy_request(key).await.unwrap().unwrap();
    assert_eq!((p.state.as_str(), p.version), ("COMPLETE", 8));
    assert!(p.completed_at.is_some());
    assert_eq!(s.privacy_requests(None, 10).await.unwrap().len(), 1);
    // Guards: outcomes never change, rows are never deleted.
    assert!(db
        .exec(
            "UPDATE admin_privacy_request_services SET outcome = 'LEGAL_HOLD'",
            vec![]
        )
        .await
        .is_err());
    assert!(db
        .exec("DELETE FROM admin_privacy_requests", vec![])
        .await
        .is_err());
    assert!(db
        .exec(
            "UPDATE admin_privacy_requests SET subject_ref = gen_random_uuid()",
            vec![]
        )
        .await
        .is_err());
    db.drop().await;
}

fn owner_event(subject: &str, version: i64, kind: &str) -> admin_bridge::store::OwnerEvent {
    admin_bridge::store::OwnerEvent {
        producer: "developer-platform".into(),
        event_id: Uuid::new_v4(),
        event_type: kind.into(),
        subject_kind: "verification_case".into(),
        subject_id: subject.into(),
        state: "OPEN".into(),
        version,
        occurred_at: Utc.timestamp_opt(1_795_000_000, 123_456_789).unwrap(),
    }
}

#[tokio::test]
async fn owner_events_append_in_sequence_once_and_acknowledgements_move_forward() {
    use admin_bridge::store::{AckOutcome, IngestOutcome};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.store;
    let at = Utc.timestamp_opt(1_795_000_100, 0).unwrap();
    const CASE: &str = "verification.case.changed.v1";
    let first = owner_event("case-1", 2, CASE);
    assert_eq!(
        s.ingest_event(&first, true, at).await.unwrap(),
        IngestOutcome::Appended(1)
    );
    assert_eq!(
        s.ingest_event(&first, true, at).await.unwrap(),
        IngestOutcome::Duplicate
    );
    assert_eq!(
        s.ingest_event(&owner_event("case-1", 2, CASE), true, at)
            .await
            .unwrap(),
        IngestOutcome::Stale
    );
    assert_eq!(
        s.ingest_event(&owner_event("case-1", 3, CASE), true, at)
            .await
            .unwrap(),
        IngestOutcome::Appended(2)
    );
    assert_eq!(
        s.ingest_event(&owner_event("case-2", 1, "unknown.thing.v1"), false, at)
            .await
            .unwrap(),
        IngestOutcome::Quarantined
    );
    // Concurrent appends: one sequence each, no gaps among the committed ones, in order.
    let mut set = tokio::task::JoinSet::new();
    for n in 0..8 {
        let st = s.clone();
        set.spawn(async move {
            st.ingest_event(&owner_event(&format!("c-{n}"), 1, CASE), true, at)
                .await
                .unwrap()
        });
    }
    while set.join_next().await.is_some() {}
    let feed = s
        .event_feed(
            at - Duration::seconds(1),
            at + Duration::seconds(1),
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(
        feed.iter().map(|f| f.sequence).collect::<Vec<_>>(),
        (1..=10).collect::<Vec<i64>>()
    );
    assert_eq!(
        feed[0].event.occurred_at,
        first.occurred_at.trunc_subsecs(6)
    );
    let page = s
        .event_feed(
            at - Duration::seconds(1),
            at + Duration::seconds(1),
            Some(2),
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        page.iter().map(|f| f.sequence).collect::<Vec<_>>(),
        [3, 4, 5]
    );
    let inbox = s
        .projections()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == "events.inbox")
        .unwrap();
    assert_eq!((inbox.quarantined, inbox.last_event_at), (1, Some(at)));

    assert_eq!(
        s.acknowledge_events("hq-worker", 4, at).await.unwrap(),
        AckOutcome::Acknowledged(4)
    );
    assert_eq!(
        s.acknowledge_events("hq-worker", 2, at).await.unwrap(),
        AckOutcome::Acknowledged(4)
    );
    assert_eq!(
        s.acknowledge_events("hq-worker", 11, at).await.unwrap(),
        AckOutcome::BeyondHead
    );
    // Guards: the feed and the inbox are append-only; acknowledgements never go back.
    for sql in [
        "UPDATE admin_hq_outbox SET state = 'CLOSED'",
        "DELETE FROM admin_hq_outbox",
        "DELETE FROM admin_event_inbox",
        "UPDATE admin_hq_consumer_cursors SET acknowledged_sequence = 1",
        "DELETE FROM admin_hq_consumer_cursors",
    ] {
        assert!(db.exec(sql, vec![]).await.is_err(), "{sql}");
    }
    db.drop().await;
}

#[tokio::test]
async fn activation_requests_record_their_dispatch_with_their_operation() {
    use admin_bridge::store::{
        ActivationRequest, ActivationUpdate, BeginOutcome, OpStatus, Settlement,
    };
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.store;
    let key = Uuid::new_v4();
    let mut op = operation(key, &format!("github:release:activation:{key}"), "0");
    op.capability = "operations.activation.request".into();
    op.owner = "github-actions".into();
    let rec = ActivationRequest {
        request_id: key,
        operation: "activate-oauth".into(),
        release_sha: "a".repeat(40),
        confirm: Some("owner-approved".into()),
        requested_by: "hq:staff-0001".into(),
        approver: "hq:staff-0002".into(),
        approval_id: "ap-1".into(),
        state: "REQUESTED".into(),
        created_at: op.created_at,
        dispatched_at: None,
        run_id: None,
    };
    assert_eq!(
        s.begin_activation(&op, &rec).await.unwrap(),
        BeginOutcome::Started
    );
    assert!(matches!(
        s.begin_activation(&op, &rec).await.unwrap(),
        BeginOutcome::Existing(_)
    ));
    // Another staff member with the same id: the slot is taken (nothing recorded).
    let mut other = op.clone();
    other.principal_sub = "staff-0009".into();
    assert_eq!(
        s.begin_activation(&other, &rec).await.unwrap(),
        BeginOutcome::VersionTaken
    );
    let at = op.created_at + Duration::seconds(1);
    assert!(s.mark_sent("staff-0001", key, at).await.unwrap());
    let unknown = Settlement {
        status: OpStatus::UnknownOutcome,
        source_status: None,
        source_version: None,
        error_code: Some("UNKNOWN_OUTCOME".into()),
        at,
    };
    let keep = ActivationUpdate {
        state: None,
        dispatched_at: None,
        run_id: None,
    };
    assert!(s
        .settle_activation("staff-0001", key, &unknown, &keep)
        .await
        .unwrap());
    let stored = s.activation_request(key).await.unwrap().unwrap();
    assert_eq!((stored.state.as_str(), stored.run_id), ("REQUESTED", None));
    let ok = Settlement {
        status: OpStatus::Succeeded,
        source_status: Some("DISPATCHED".into()),
        source_version: Some("88".into()),
        error_code: None,
        at,
    };
    let dispatched = ActivationUpdate {
        state: Some("DISPATCHED"),
        dispatched_at: Some(at),
        run_id: Some(88),
    };
    assert!(s
        .settle_activation("staff-0001", key, &ok, &dispatched)
        .await
        .unwrap());
    // Settled once: a second outcome changes nothing.
    assert!(!s
        .settle_activation("staff-0001", key, &ok, &dispatched)
        .await
        .unwrap());
    let stored = s.activation_request(key).await.unwrap().unwrap();
    assert_eq!(
        (stored.state.as_str(), stored.run_id),
        ("DISPATCHED", Some(88))
    );
    assert_eq!(
        s.operation("staff-0001", key)
            .await
            .unwrap()
            .unwrap()
            .status,
        OpStatus::Succeeded
    );
    assert_eq!(s.activations_for_runs(&[88, 89]).await.unwrap().len(), 1);
    assert!(
        !s.link_activation_run(key, 90).await.unwrap(),
        "linked once"
    );
    assert_eq!(s.activation_requests(None, 10).await.unwrap().len(), 1);
    assert!(s
        .activation_requests(Some((stored.created_at, key)), 10)
        .await
        .unwrap()
        .is_empty());
    for sql in [
        "UPDATE admin_activation_requests SET run_id = 91",
        "UPDATE admin_activation_requests SET operation = 'release'",
        "UPDATE admin_activation_requests SET state = 'FAILED'",
        "DELETE FROM admin_activation_requests",
    ] {
        assert!(db.exec(sql, vec![]).await.is_err(), "{sql}");
    }
    db.drop().await;
}

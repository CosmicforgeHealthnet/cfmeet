//! Workstream W disaster recovery: an SMS Gateway database backed up with
//! `infra/deployment/backup/backup.sh --service sms-gateway` and restored with
//! `restore.sh` into an empty database resumes without duplicate paid sends.
//!
//! Seeded state: one message already submitted to the provider and two authorized but
//! not yet submitted when the backup is taken. After the restore the migrator has
//! nothing pending, two dispatchers of the restarted service submit the two pending
//! messages once each, and the already-submitted message is never sent again.
//!
//! Requires `pg_dump`, `pg_restore` and `psql`: without them the test prints
//! `SKIPPED: disaster-recovery certification (pg_dump not found)` — unless
//! `DISASTER_RECOVERY_CERT_REQUIRED=1`, in which case it fails.

mod common;

use std::{path::Path, process::Command, sync::Arc};

use axum::http::StatusCode;
use common::{Harness, TestDb};
use serde_json::json;
use sms_gateway_migration::{Migrator, MigratorTrait};
use uuid::Uuid;

const DCN: &str = "+15550109001";

fn tools_available() -> bool {
    let ok = ["pg_dump", "pg_restore", "psql", "sha256sum"]
        .iter()
        .all(|t| {
            Command::new("sh")
                .args(["-c", &format!("command -v {t}")])
                .output()
                .is_ok_and(|o| o.status.success())
        });
    if !ok {
        assert!(
            std::env::var("DISASTER_RECOVERY_CERT_REQUIRED").as_deref() != Ok("1"),
            "disaster-recovery certification REQUIRED but pg_dump / pg_restore / psql are missing"
        );
        eprintln!("SKIPPED: disaster-recovery certification (pg_dump not found)");
    }
    ok
}

fn run(script: &str, args: &[&str]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/deployment/backup")
        .join(script);
    let out = Command::new("bash")
        .arg(path)
        .args(args)
        .output()
        .expect("run backup script");
    assert!(
        out.status.success(),
        "{script} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn a_restored_database_submits_pending_messages_once_and_never_resends() {
    if !tools_available() {
        return;
    }
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.send", "sms.read"]);
    let n = h.number(&c, DCN, Some(("PRINCIPAL", Uuid::new_v4())));
    let send = |to: &str| json!({ "from": { "numberResourceId": n.id }, "to": to, "text": "dr" });
    let r = h
        .send(&c, send("+15550104001"), &format!("k-{}", Uuid::new_v4()))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    assert_eq!(h.dispatch().await.submitted, 1);
    for to in ["+15550104002", "+15550104003"] {
        let r = h.send(&c, send(to), &format!("k-{}", Uuid::new_v4())).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    }
    assert_eq!(h.provider.submissions().len(), 1);

    let dir = std::env::temp_dir().join(format!("sms-dr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dump = dir.join("sms-gateway.dump");
    run(
        "backup.sh",
        &[
            "--service",
            "sms-gateway",
            "--database-url",
            &h.db.url,
            "--out",
            dump.to_str().unwrap(),
        ],
    );
    let target = TestDb::empty().await.expect("database");
    run(
        "restore.sh",
        &[
            "--database-url",
            &target.url,
            "--in",
            dump.to_str().unwrap(),
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(Migrator::get_pending_migrations(&target.conn)
        .await
        .unwrap()
        .is_empty());
    Migrator::up(&target.conn, None).await.unwrap();

    // The restarted service: restored database, the same provider and Numbering.
    let (provider, numbering, principals) = (
        h.provider.clone(),
        h.numbering.clone(),
        h.principals.clone(),
    );
    let h2 = Harness::on(Arc::new(target), move |s| {
        *s = s
            .clone()
            .with_provider(Arc::new(provider))
            .with_numbering(numbering)
            .with_principals(principals);
    });
    let (a, b) = tokio::join!(h2.dispatch(), h2.dispatch());
    assert_eq!(a.submitted + b.submitted, 2, "{a:?} {b:?}");
    let mut subs = h.provider.submissions();
    assert_eq!(subs.len(), 3);
    subs.sort();
    subs.dedup();
    assert_eq!(subs.len(), 3, "each message submitted exactly once");
    assert_eq!(h2.dispatch().await.claimed, 0);
    assert_eq!(
        h2.scalar("SELECT count(*) FROM sms_transports WHERE status = 'SUBMITTED'")
            .await,
        3
    );
}

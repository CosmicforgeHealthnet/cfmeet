//! Part V J §§136–138 — backup / restore and disaster recovery of the Messaging database
//! with the shipped tooling (`infra/deployment/backup/backup.sh` / `restore.sh`), and
//! key-transparency rollback detection after a restore (mandatory scenario 8).
//!
//! Seeded: a MANAGED group with messages (sealed at rest), a PRIVATE_E2EE conversation
//! with a real MLS message, transparency-log leaves, pending outbox events and webhook
//! relay rows, usage facts. Backed up, then the source moves on (a new device appends to
//! the log and the operator checkpoint follows it). Restored into an EMPTY database:
//!
//! * nothing is pending for the release and every table's row count equals the backup;
//! * the archive holds no message body of either profile (MANAGED bodies sealed,
//!   PRIVATE_E2EE ciphertext only) and no managed key; MANAGED search lexemes are present
//!   by design, so backups are protected like MANAGED content;
//! * a NEW Messaging instance over the restored database (same managed keyring, from
//!   the secret store — never from the backup) serves history, accepts new messages and
//!   drains the restored webhook relay exactly once;
//! * the restored transparency log is BEHIND the operator checkpoint taken after the
//!   backup: `KT_ROLLBACK_DETECTED`, and log appends (device registration) fail closed
//!   until an operator resolves it; the checkpoint taken at backup time is consistent.
//!
//! No RTO / RPO is claimed: the measured backup and restore durations of this run are
//! printed (`DR {json}`) for the evidence record, nothing more.
//!
//! Tooling: without `pg_dump` / `pg_restore` / `psql` this prints SKIPPED, unless
//! `DISASTER_RECOVERY_CERT_REQUIRED=1`, in which case it FAILS.

mod common;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use common::{flows::ALL_SCOPES, *};
use messaging::{
    operations::{self, KtVerdict},
    webhook_relay::{self, PlatformEventSink, RelayFailure},
};
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tooling() -> Option<PathBuf> {
    if ["pg_dump", "pg_restore", "psql"].iter().all(|t| have(t)) {
        return Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../infra/deployment/backup"),
        );
    }
    if std::env::var("DISASTER_RECOVERY_CERT_REQUIRED").as_deref() == Ok("1") {
        panic!("disaster-recovery certification REQUIRED but pg_dump / pg_restore / psql are not installed");
    }
    println!("SKIPPED: disaster-recovery certification (pg_dump not found)");
    None
}

fn run(script: &PathBuf, args: &[&str]) {
    let out = Command::new("bash")
        .arg(script)
        .args(args)
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{} {:?}: {}{}",
        script.display(),
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

async fn table_counts(conn: &sea_orm::DatabaseConnection) -> BTreeMap<String, i64> {
    let tables: Vec<String> = conn
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect();
    let mut out = BTreeMap::new();
    for t in tables {
        let n: i64 = conn
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT count(*) FROM \"{t}\""),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
        out.insert(t, n);
    }
    out
}

#[derive(Default)]
struct Sink {
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl PlatformEventSink for Sink {
    async fn publish(&self, envelope: &Value) -> Result<(), RelayFailure> {
        self.seen
            .lock()
            .unwrap()
            .push(envelope["event_id"].as_str().unwrap().to_string());
        Ok(())
    }
}

const MANAGED_PHRASE: &str = "the quarterly numbers are confidential";
const E2EE_PHRASE: &str = "end to end only words";

#[tokio::test]
async fn a_restored_backup_serves_history_drains_relays_once_and_detects_the_kt_rollback() {
    let Some(tools) = tooling() else {
        return;
    };
    let Some(h) = Harness::new().await else {
        return;
    };
    // ------------------------------------------------------------------ seed
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 3).await;
    for i in 0..5 {
        let r = h
            .send_managed(&c, &conv, people[i % 3], MANAGED_PHRASE)
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let e = h.customer(ALL_SCOPES);
    let (econv, mut alice, _bob) = h.e2ee_direct(&e).await;
    let sent = h.send_e2ee(&e, &econv, &mut alice, E2EE_PHRASE).await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let pending_relay = h
        .scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'PENDING'")
        .await;
    assert!(pending_relay > 0);
    assert!(
        h.scalar("SELECT count(*) FROM messaging_usage_records")
            .await
            >= 6
    );
    let at_backup = operations::kt_checkpoint(h.conn()).await.unwrap();
    assert!(at_backup.tree_size >= 2, "devices are in the log");
    let source_counts = table_counts(h.conn()).await;

    // ---------------------------------------------------------------- backup
    let dir = std::env::temp_dir().join(format!("messaging-dr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dump = dir.join("messaging.dump");
    let dump_s = dump.to_str().unwrap();
    let t = Instant::now();
    run(
        &tools.join("backup.sh"),
        &[
            "--service",
            "messaging",
            "--database-url",
            &h.db.url,
            "--out",
            dump_s,
        ],
    );
    let backup_ms = t.elapsed().as_millis();
    let raw = String::from_utf8_lossy(&std::fs::read(&dump).unwrap()).to_string();
    let rendered = Command::new("pg_restore")
        .args(["--no-owner", "--no-privileges", "-f", "-", dump_s])
        .output()
        .unwrap();
    assert!(rendered.status.success());
    let sql = String::from_utf8_lossy(&rendered.stdout).to_string();
    assert!(
        sql.contains("messaging_messages"),
        "the rendered dump has the data"
    );
    // MANAGED bodies are sealed at rest; PRIVATE_E2EE is ciphertext only. (The MANAGED
    // search index holds lexemes of MANAGED messages by design — server-readable
    // content — so a Messaging backup is protected like MANAGED content itself.)
    for needle in [MANAGED_PHRASE, E2EE_PHRASE] {
        assert!(
            !raw.contains(needle) && !sql.contains(needle),
            "the backup contains a message body: {needle}"
        );
    }
    assert!(
        !sql.contains("'only'") && !sql.contains("'words'"),
        "no E2EE lexeme is ever indexed"
    );

    // The source moves on after the backup: a new device appends to the log, and the
    // operator checkpoint (kept outside the database) follows it.
    let _carol = h.member(&e, 1).await;
    let after_backup = operations::kt_checkpoint(h.conn()).await.unwrap();
    assert!(after_backup.tree_size > at_backup.tree_size);

    // ---------------------------------------------------------------- restore
    let target = TestDb::empty().await.unwrap();
    let t = Instant::now();
    run(
        &tools.join("restore.sh"),
        &["--database-url", &target.url, "--in", dump_s],
    );
    let restore_ms = t.elapsed().as_millis();
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        messaging_migration::Migrator::get_pending_migrations(&target.conn)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        table_counts(&target.conn).await,
        source_counts,
        "the restore is complete"
    );
    println!(
        "DR {}",
        json!({ "backup_ms": backup_ms, "restore_ms": restore_ms, "tables": source_counts.len(),
        "rows": source_counts.values().sum::<i64>(), "rto_rpo_claimed": false })
    );

    // A NEW instance over the restored database: the managed keyring comes from the
    // secret store (the same keys), never from the backup.
    let keys = h.state.managed_keys.clone();
    let restored = Harness::on(Arc::new(target), move |s| s.managed_keys = keys);
    *restored.authority.tokens.lock().unwrap() = h.authority.tokens.lock().unwrap().clone();
    *restored.authority.delegated.lock().unwrap() = h.authority.delegated.lock().unwrap().clone();
    *restored.principals.known.lock().unwrap() = h.principals.known.lock().unwrap().clone();

    // History is served (and decrypted with the operator-held keys).
    let cid = conv["conversationId"].as_str().unwrap();
    let history = restored
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.text);
    let texts: Vec<&str> = history.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["managed"]["text"].as_str())
        .collect();
    assert_eq!(texts.len(), 5, "{}", history.text);
    assert!(texts.iter().all(|t| *t == MANAGED_PHRASE));
    // New messages are accepted after the restore.
    let r = restored
        .send_managed(&c, &conv, people[2], "after the restore")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Restored relay work drains once.
    let sink = Sink::default();
    let first = webhook_relay::relay_due(&restored.state, &sink, 500)
        .await
        .unwrap();
    assert!(
        first.relayed as i64 > pending_relay,
        "restored + new events relayed"
    );
    assert_eq!(
        webhook_relay::relay_due(&restored.state, &sink, 500)
            .await
            .unwrap()
            .relayed,
        0
    );
    let seen = sink.seen.lock().unwrap().clone();
    let unique: std::collections::BTreeSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "each event relayed once");

    // KT: the checkpoint taken at backup time is consistent; the newer one detects the
    // rollback, and appends fail closed until an operator resolves the incident.
    let now = chrono::Utc::now();
    assert!(matches!(
        operations::kt_verify(restored.conn(), &at_backup, now)
            .await
            .unwrap(),
        KtVerdict::Consistent { .. }
    ));
    assert!(matches!(
        operations::kt_verify(restored.conn(), &after_backup, now).await.unwrap(),
        KtVerdict::Behind { database_size, checkpoint_size }
            if database_size == at_backup.tree_size && checkpoint_size == after_backup.tree_size
    ));
    let (_, refused) = restored.register_device(&e, restored.principal(&e)).await;
    assert_eq!(
        refused.reason(),
        "KT_LOG_ROLLBACK_DETECTED",
        "{}",
        refused.text
    );
    // Reads keep working during the incident.
    let ecid = econv["conversationId"].as_str().unwrap();
    let r = restored
        .get(
            &e,
            Some(alice.principal),
            &format!("/v1/conversations/{ecid}/messages"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(!r.text.contains(E2EE_PHRASE));
}

// ------------------------------------------------------------------------------------
// Object storage, SMS transports and E2EE content across a restore (§137, Appendix E)
// ------------------------------------------------------------------------------------

struct Clean;

#[async_trait]
impl messaging::attachments::scanner::AttachmentScanner for Clean {
    fn engine(&self) -> &'static str {
        "dr-test"
    }
    async fn scan(&self, _: &[u8]) -> messaging::attachments::scanner::ScanOutcome {
        messaging::attachments::scanner::ScanOutcome::Clean
    }
}

/// Counts carrier submissions (POST /internal/v1/sms-bridge/messages).
#[derive(Default)]
struct CountingGateway {
    submissions: Mutex<usize>,
}

#[async_trait]
impl messaging::sms_bridge::SmsTransport for CountingGateway {
    async fn call(
        &self,
        _: &cf_service_auth::DelegatedPlatformContext,
        _: axum::http::Method,
        path: &str,
        _: Option<&Value>,
    ) -> Result<(u16, Value), messaging::sms_bridge::TransportError> {
        Ok(match path {
            "/internal/v1/sms-bridge/bindings" => (
                201,
                json!({ "bindingId": Uuid::new_v4(), "remote": { "masked": "+*******3456" }, "status": "ACTIVE" }),
            ),
            "/internal/v1/sms-bridge/messages" => {
                *self.submissions.lock().unwrap() += 1;
                (
                    202,
                    json!({ "smsMessageId": Uuid::new_v4(), "status": "AUTHORIZED", "segmentCount": 1 }),
                )
            }
            // Status follow-ups: still in flight at the carrier.
            _ => (200, json!({ "status": "SUBMITTED", "segmentCount": 1 })),
        })
    }
}

/// Copies every object of `from` into a fresh store (the operator's bucket backup /
/// mirror; Messaging stores no object outside its bucket).
async fn mirror(
    from: &cf_object_storage::MemoryObjectStore,
) -> Arc<cf_object_storage::MemoryObjectStore> {
    use cf_object_storage::ObjectStore;
    let to = Arc::new(cf_object_storage::MemoryObjectStore::new());
    for o in from.list("", None, 10_000).await.unwrap() {
        let bytes = from.get(&o.key, 64 * 1024 * 1024).await.unwrap().unwrap();
        let meta = from.head(&o.key).await.unwrap().unwrap();
        to.put(
            &o.key,
            bytes,
            meta.content_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
        )
        .await
        .unwrap();
    }
    to
}

/// The restore of a database backup plus the operator's bucket mirror serves attachments
/// with their integrity intact; SMS already handed to the carrier is never submitted again
/// (only the queued one is); and PRIVATE_E2EE content stays recoverable only with the
/// device-held MLS state — a new device without history transfer or a secure backup gets
/// nothing readable out of the restored ciphertext.
#[tokio::test]
async fn a_restore_keeps_attachments_never_resubmits_sms_and_cannot_recover_e2ee_without_device_state(
) {
    let Some(tools) = tooling() else {
        return;
    };
    let store = Arc::new(cf_object_storage::MemoryObjectStore::new());
    let gateway = Arc::new(CountingGateway::default());
    let (s2, g2) = (store.clone(), gateway.clone());
    let Some(h) = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = Arc::new(Clean);
        s.sms = g2;
    })
    .await
    else {
        return;
    };
    // ------------------------------------------------------------------ seed
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "attachments": { "enabled": true } }),
    )
    .await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let file: Vec<u8> = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89"
        .iter()
        .copied()
        .chain((0..4000).map(|i| (i % 251) as u8))
        .collect();
    let intent = h
        .post(&c, Some(people[0]), &format!("/v1/conversations/{cid}/attachments"),
              json!({ "kind": "IMAGE", "size": file.len(), "sha256": cf_object_storage::sha256_hex(&file),
                      "mediaType": "image/png", "fileName": "plan.png" }))
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    let upload = &intent.json["upload"];
    let write = cf_object_storage::SignedWrite {
        method: "PUT",
        url: upload["url"].as_str().unwrap().to_string(),
        headers: upload["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x["name"].as_str().unwrap().to_string(),
                    x["value"].as_str().unwrap().to_string(),
                )
            })
            .collect(),
        expires_at: chrono::DateTime::parse_from_rfc3339(upload["expiresAt"].as_str().unwrap())
            .unwrap()
            .into(),
    };
    store.upload_signed(&write, h.now(), file.clone()).unwrap();
    let aid = intent.json["attachmentId"].as_str().unwrap().to_string();
    let f = h
        .post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/attachments/{aid}/finalize"),
            json!({}),
        )
        .await;
    assert_eq!(f.status, StatusCode::OK, "{}", f.text);
    let m = h
        .post(&c, Some(people[0]), &format!("/v1/conversations/{cid}/messages"),
              json!({ "clientMessageId": Uuid::new_v4(), "text": "the plan", "attachmentIds": [aid] }))
        .await;
    assert_eq!(m.status, StatusCode::CREATED, "{}", m.text);
    // SMS: one message handed to the carrier before the backup, one still queued.
    let party = h.principal_of_kind(&c, "TELEPHONE_ENDPOINT");
    let b = h
        .post(&c, Some(people[0]), &format!("/v1/conversations/{cid}/sms-bridges"),
              json!({ "numberResourceId": Uuid::new_v4(), "to": "+15550123456", "externalPrincipalId": party,
                      "disclosureVersion": messaging::sms_bridge::DISCLOSURE_VERSION, "smsDisclosureAcknowledged": true }))
        .await;
    assert_eq!(b.status, StatusCode::CREATED, "{}", b.text);
    h.send_managed(&c, &conv, people[1], "submitted before the backup")
        .await;
    assert_eq!(
        messaging::sms_bridge::run_due(&h.state, 10).await.unwrap(),
        1
    );
    assert_eq!(*gateway.submissions.lock().unwrap(), 1);
    h.send_managed(&c, &conv, people[1], "queued at backup time")
        .await;
    // E2EE: a real MLS message the recipient has not read yet.
    let e = h.customer(ALL_SCOPES);
    let (econv, mut alice, mut bob) = h.e2ee_direct(&e).await;
    let sent = h.send_e2ee(&e, &econv, &mut alice, E2EE_PHRASE).await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);

    // ---------------------------------------------------- backup (database + bucket)
    let dir = std::env::temp_dir().join(format!("messaging-dr2-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dump = dir.join("messaging.dump");
    let dump_s = dump.to_str().unwrap();
    run(
        &tools.join("backup.sh"),
        &[
            "--service",
            "messaging",
            "--database-url",
            &h.db.url,
            "--out",
            dump_s,
        ],
    );
    let bucket = mirror(&store).await;

    // ---------------------------------------------------------------- restore
    let target = TestDb::empty().await.unwrap();
    run(
        &tools.join("restore.sh"),
        &["--database-url", &target.url, "--in", dump_s],
    );
    std::fs::remove_dir_all(&dir).ok();
    let after = Arc::new(CountingGateway::default());
    let (keys, b2, a2) = (h.state.managed_keys.clone(), bucket.clone(), after.clone());
    let restored = Harness::on(Arc::new(target), move |s| {
        s.managed_keys = keys;
        s.storage = b2;
        s.scanner = Arc::new(Clean);
        s.sms = a2;
    });
    *restored.authority.tokens.lock().unwrap() = h.authority.tokens.lock().unwrap().clone();
    *restored.authority.delegated.lock().unwrap() = h.authority.delegated.lock().unwrap().clone();
    *restored.principals.known.lock().unwrap() = h.principals.known.lock().unwrap().clone();

    // Attachments: metadata and object restored together, digest verified on read.
    let body = restored
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/attachments/{aid}/content"),
        )
        .await;
    assert_eq!(body.status, StatusCode::OK, "{}", body.text);
    assert_eq!(body.bytes, file);
    // A restore WITHOUT the bucket fails closed (never served unverified).
    {
        use cf_object_storage::ObjectStore;
        for o in bucket.list("", None, 10_000).await.unwrap() {
            bucket.delete(&o.key).await.unwrap();
        }
    }
    let missing = restored
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/attachments/{aid}/content"),
        )
        .await;
    assert_eq!(
        (missing.code(), missing.reason()),
        ("ATTACHMENT_NOT_READY", "OBJECT_MISSING"),
        "{}",
        missing.text
    );

    // SMS: only the queued message is submitted; the one the carrier already has is not.
    messaging::sms_bridge::run_due(&restored.state, 10)
        .await
        .unwrap();
    messaging::sms_bridge::run_due(&restored.state, 10)
        .await
        .unwrap();
    assert_eq!(
        *after.submissions.lock().unwrap(),
        1,
        "no duplicate carrier submission"
    );

    // E2EE: the restored ciphertext is intact — the recipient's device (holding its MLS
    // state) reads it — but a new device with no history transfer and no secure backup
    // cannot: the server never held anything that decrypts it.
    let ecid = econv["conversationId"].as_str().unwrap();
    let listed = restored
        .get(
            &e,
            Some(bob.principal),
            &format!("/v1/conversations/{ecid}/messages"),
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert!(!listed.text.contains(E2EE_PHRASE));
    let msg = listed.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["kind"] == "APPLICATION")
        .unwrap()
        .clone();
    let ciphertext = unb64(&msg["e2ee"]["ciphertext"]);
    let fresh = cf_messaging_crypto::device::Device::generate(Uuid::new_v4()).unwrap();
    let gid = unb64(&econv["encryption"]["mlsGroupId"]);
    let mut fresh_group = fresh.create_group(&gid).unwrap();
    assert!(
        fresh.process(&mut fresh_group, &ciphertext).is_err(),
        "a device without the group state cannot decrypt restored content"
    );
    let read = restored.receive(&e, &econv, &mut bob).await;
    assert!(
        read.iter().any(|(_, t)| t == E2EE_PHRASE),
        "the device-held state still reads it: {read:?}"
    );
}

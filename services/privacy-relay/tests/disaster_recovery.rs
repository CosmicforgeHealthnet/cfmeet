//! §88 disaster recovery: a Privacy Relay database backed up with
//! `infra/deployment/backup/backup.sh --service privacy-relay` and restored with
//! `restore.sh` into a fresh database resumes safely.
//!
//! Seeded state: an ACTIVE relay assignment (callback mapping of a protected contact,
//! stored encrypted), a REVOKED context whose relay number is in COOLDOWN, an EXPIRED
//! context + session, PENDING enforcement jobs (Calling was down) and PENDING outbox
//! events — one of them claimed by a publisher that crashed mid-flight (its lease is in
//! the backup). After the restore: the migrator has nothing pending; the real worker
//! sweep (two instances at once) drains pending outbox / enforcement exactly once;
//! revoked / expired mappings stay unusable (new calls refused, a delayed inbound
//! callback cannot reach the former destination, no new assignment while the cooldown
//! lasts), the active mapping and the encrypted contact still resolve with the
//! keyring; the dump holds neither a plaintext protected number nor keyring material;
//! and a restore WITHOUT the keyring fails closed (startup guard refuses, resolution
//! refuses, CONTACT_KEY_UNAVAILABLE incident) — never a plaintext fallback.
//!
//! Requires `pg_dump`, `pg_restore` and `psql`: without them the test prints
//! `SKIPPED: disaster-recovery certification (pg_dump not found)` — unless
//! `DISASTER_RECOVERY_CERT_REQUIRED=1`, in which case it fails.

mod common;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use base64::Engine;
use cf_service_auth::ApplicationEnvironmentKind;
use common::*;
use privacy_relay::{
    clock::ManualClock,
    contact_vault::{
        keyring::{generate_keyring_json, FileKeyring, KeyPurpose, KeyVersion},
        VaultKeys,
    },
    enforcement::DeliveryFailure,
    events::EventPublisher,
};
use privacy_relay_migration::{Migrator, MigratorTrait};
use serde_json::{json, Value};
use uuid::Uuid;

const ACTIVE_RELAY: &str = "+44 20 7946 0500";
const COOLDOWN_RELAY: &str = "+44 20 7946 0501";
const CONTACT: &str = "+44 20 7946 0123";
const CONTACT_DIGITS: &str = "2079460123";
const OTHER_CONTACT: &str = "+44 20 7946 0124";
const OTHER_DIGITS: &str = "2079460124";

fn tool(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The skip / REQUIRED rule of the disaster-recovery certification.
fn tools_available() -> bool {
    if ["pg_dump", "pg_restore", "psql", "sha256sum"]
        .iter()
        .all(|t| tool(t))
    {
        return true;
    }
    assert!(
        std::env::var("DISASTER_RECOVERY_CERT_REQUIRED").as_deref() != Ok("1"),
        "disaster-recovery certification REQUIRED but pg_dump / pg_restore / psql are missing"
    );
    eprintln!("SKIPPED: disaster-recovery certification (pg_dump not found)");
    false
}

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/deployment/backup")
        .join(name)
}

fn run(script_name: &str, args: &[&str]) {
    let out = Command::new("bash")
        .arg(script(script_name))
        .args(args)
        .output()
        .expect("run backup script");
    assert!(
        out.status.success(),
        "{script_name} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A private scratch directory, removed on drop.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Keyrings generated here so the test knows the key material it must never find.
struct Keys {
    vault: VaultKeys,
    material: Vec<String>,
}

fn keys() -> Keys {
    let enc = generate_keyring_json(
        KeyPurpose::Encryption,
        &KeyVersion::parse("ck-dr-1").unwrap(),
    );
    let fp = generate_keyring_json(
        KeyPurpose::Fingerprint,
        &KeyVersion::parse("fk-dr-1").unwrap(),
    );
    let mut material = Vec::new();
    for doc in [&enc, &fp] {
        let v: Value = serde_json::from_str(doc).unwrap();
        for k in v["keys"].as_array().unwrap() {
            let b64 = k["key"].as_str().unwrap().to_string();
            let raw = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .unwrap();
            material.push(hex::encode(&raw));
            material.push(b64);
        }
    }
    let vault = VaultKeys::new(
        Arc::new(FileKeyring::from_json(&enc, KeyPurpose::Encryption).unwrap()),
        Arc::new(FileKeyring::from_json(&fp, KeyPurpose::Fingerprint).unwrap()),
    )
    .unwrap();
    Keys { vault, material }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

#[async_trait]
impl EventPublisher for Recorder {
    async fn publish(&self, _: &str, envelope: &Value) -> Result<(), &'static str> {
        self.0
            .lock()
            .unwrap()
            .push(envelope["event_id"].as_str().unwrap().to_string());
        Ok(())
    }
}

/// Claims rows and never returns (a publisher that crashed mid-flight).
struct Hanging(tokio::sync::Notify);

#[async_trait]
impl EventPublisher for Hanging {
    async fn publish(&self, _: &str, _: &Value) -> Result<(), &'static str> {
        self.0.notify_one();
        std::future::pending::<()>().await;
        Err("UNAVAILABLE")
    }
}

struct Seeded {
    c: Consumer,
    principal: Uuid,
    contact: String,
    active_session: String,
    revoked_session: String,
    expired_session: String,
    other_principal: Uuid,
}

/// A CALLBACK + PUBLIC relay context (principal + VERIFIED protected contact) valid for a
/// day, and its session; returns (context, session).
async fn relay_context(
    h: &Harness,
    c: &Consumer,
    principal: Uuid,
    contact: &str,
) -> (String, String) {
    let mut req = context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    req["expiresAt"] = json!(iso(at(24 * 60)));
    let ctx = h.create_context(c, req).await;
    let context = ctx["contextId"].as_str().unwrap().to_string();
    let session = h
        .create_session(
            c,
            &context,
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    (context, session)
}

async fn seed(h: &Harness) -> Seeded {
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    h.add_relay_number(ACTIVE_RELAY, "pb-dr-active").await;
    let principal = Uuid::new_v4();
    let contact = h.create_contact(&c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", None).await;
    let (_, active_session) = relay_context(h, &c, principal, &contact).await;
    assert_eq!(
        h.assign_relay_number(&c, &active_session).await.status,
        StatusCode::CREATED
    );
    // The second number goes to a context that is then REVOKED (number -> COOLDOWN).
    h.add_relay_number(COOLDOWN_RELAY, "pb-dr-cooldown").await;
    let other_principal = Uuid::new_v4();
    let other = h.create_contact(&c, OTHER_CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &other, "VERIFIED", None).await;
    let (revoked_context, revoked_session) = relay_context(h, &c, other_principal, &other).await;
    assert_eq!(
        h.assign_relay_number(&c, &revoked_session).await.status,
        StatusCode::CREATED
    );
    // An application context that expires at minute 20.
    let [x, y] = principals(2).try_into().unwrap();
    let mut req = context_request(&[x, y], "BIDIRECTIONAL", &[]);
    req["expiresAt"] = json!(iso(at(20)));
    let expiring = h.create_context(&c, req).await;
    let expired_session = h
        .app_session(&c, expiring["contextId"].as_str().unwrap())
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    // Calling is down from here on: enforcement stays PENDING.
    h.calling.fail_with(Some(DeliveryFailure::Unavailable));
    h.clock.set(at(5));
    assert_eq!(
        h.lifecycle(&c, &revoked_context, "revoke", 1).await.status,
        StatusCode::OK
    );
    h.clock.set(at(21));
    let report = h.relay.run_sweep().await.unwrap();
    assert_eq!((report.contexts_expired, report.events_published), (1, 0));
    Seeded {
        c,
        principal,
        contact,
        active_session,
        revoked_session,
        expired_session,
        other_principal,
    }
}

async fn ids(conn: &sea_orm::DatabaseConnection, sql: &str) -> Vec<String> {
    let mut v: Vec<String> = rows(conn, sql)
        .await
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    v.sort();
    v
}

/// The restored database (fresh, empty, restored by `restore.sh`).
async fn restore_into_fresh(dump: &Path) -> Arc<TestDb> {
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
    // The migrator of the same release: nothing pending, `up` is a no-op.
    assert!(Migrator::get_pending_migrations(&target.conn)
        .await
        .unwrap()
        .is_empty());
    Migrator::up(&target.conn, None).await.unwrap();
    Arc::new(target)
}

async fn inbound(h: &Harness, called: &str, started: i64) -> Response {
    h.telephony(
        "/internal/v1/relay-routing/resolve-inbound",
        json!({
            "calledRelayIdentity": called,
            "providerEventReference": format!("evt-{}", Uuid::new_v4().simple()),
            "providerObservedAt": iso(at(23)),
            "providerStartedAt": iso(at(started)),
        }),
    )
    .await
}

/// authorize (PUBLIC_RELAY) -> grant -> Telephony resolve-for-execution.
async fn resolve_contact(h: &Harness, s: &Seeded) -> Response {
    let d = h
        .post(
            "/internal/v1/communication/authorize",
            &s.c,
            json!({ "relaySessionId": s.active_session, "source": { "principalId": s.principal },
                    "targets": [{ "contactReferenceId": s.contact }], "channel": "VOICE",
                    "route": "CALLBACK_RELAY", "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(d.json["decision"], "AUTHORIZED", "{}", d.text);
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = h
        .post(
            &format!("/internal/v1/contacts/{}/resolution-grants", s.contact),
            &s.c,
            json!({ "relaySessionId": s.active_session, "decisionId": d.json["decisionId"],
                    "callId": call, "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
        )
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    let grant = &g.json["grant"];
    let telephony = h.ids.telephony.clone();
    h.send_as(
        &telephony,
        Method::POST,
        &format!("/internal/v1/contacts/{}/resolve-for-execution", s.contact),
        Some(&s.c),
        Some(
            json!({ "grantId": grant["grantId"], "grantSecret": grant["grantSecret"],
                     "callId": call, "callLegId": leg, "executionAttemptId": Uuid::new_v4() }),
        ),
    )
    .await
}

#[tokio::test]
async fn a_restored_privacy_relay_database_resumes_safely_and_fails_closed_without_keys() {
    if !tools_available() {
        return;
    }
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let keys = keys();
    h.set_vault_keys(keys.vault.clone());
    let s = seed(&h).await;

    // A publisher crashes mid-flight: some outbox rows are leased in the backup.
    let hanging = Arc::new(Hanging(tokio::sync::Notify::new()));
    let crashed = h.relay.clone().with_event_publisher(hanging.clone());
    let task = tokio::spawn(async move { crashed.publish_outbox(3).await });
    hanging.0.notified().await;
    task.abort();
    let _ = task.await;

    // Evidence of the seeded state.
    let conn = h.conn();
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'ACTIVE'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts WHERE status = 'REVOKED'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_sessions WHERE status = 'EXPIRED'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM protected_contacts WHERE ciphertext IS NOT NULL")
            .await,
        2
    );
    let cooldown = rows(
        conn,
        "SELECT operational_status, cooldown_until::text FROM relay_number_inventory \
         WHERE operational_status = 'COOLDOWN'",
    )
    .await;
    assert_eq!(cooldown.len(), 1);
    let pending_outbox = ids(
        conn,
        "SELECT event_id FROM privacy_relay_outbox WHERE status = 'PENDING'",
    )
    .await;
    assert!(pending_outbox.len() >= 10, "{}", pending_outbox.len());
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_outbox WHERE lease_until IS NOT NULL")
            .await,
        3,
        "rows in flight at backup time"
    );
    let pending_jobs = ids(
        conn,
        "SELECT enforcement_event_id::text FROM privacy_relay_enforcement_jobs WHERE status = 'PENDING'",
    )
    .await;
    assert_eq!(pending_jobs.len(), 2, "revocation + expiry");

    // Backup.
    let dir = Dir(std::env::temp_dir().join(format!("prl-dr-{}", Uuid::new_v4().simple())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let dump = dir.0.join("privacy-relay.dump");
    run(
        "backup.sh",
        &[
            "--service",
            "privacy-relay",
            "--database-url",
            &h.db.url,
            "--out",
            dump.to_str().unwrap(),
        ],
    );
    assert!(dir.0.join("privacy-relay.dump.sha256").exists());

    // The dump holds no plaintext protected number and no key material: scan the raw
    // archive bytes AND its fully decoded SQL (the archive is compressed).
    let raw = std::fs::read(&dump).unwrap();
    let decoded = Command::new("pg_restore")
        .args(["--no-owner", "--no-privileges", "-f", "-"])
        .arg(&dump)
        .output()
        .unwrap();
    assert!(decoded.status.success());
    assert!(decoded.stdout.len() > raw.len() / 2);
    let sql = String::from_utf8_lossy(&decoded.stdout).to_string();
    assert!(sql.contains("protected_contacts") && sql.contains("ck-dr-1"));
    for haystack in [String::from_utf8_lossy(&raw).to_string(), sql.clone()] {
        for needle in [
            CONTACT_DIGITS,
            OTHER_DIGITS,
            "+442079460123",
            "+442079460124",
        ] {
            assert!(!haystack.contains(needle), "plaintext number in the dump");
        }
        for k in &keys.material {
            assert!(!haystack.contains(k.as_str()), "key material in the dump");
        }
        assert!(!haystack.contains("\"keyring_version\""));
    }

    // ---- Restore WITH the keyring, two instances, resumed two minutes later. ----
    let restored = restore_into_fresh(&dump).await;
    let mut r = Harness::on(
        restored.clone(),
        ManualClock::new(at(23)),
        ALL_CALLING_PERMISSIONS,
    );
    r.set_vault_keys(keys.vault.clone());
    r.contacts
        .verify_referenced_key_versions()
        .await
        .expect("the keyring covers every restored contact");
    let recorder = Arc::new(Recorder::default());
    let one = r.relay.clone().with_event_publisher(recorder.clone());
    let two = r.sibling().with_event_publisher(recorder.clone());
    let (x, y) = tokio::join!(one.run_sweep(), two.run_sweep());
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!(x.enforcement_applied + y.enforcement_applied, 2);
    let again = one.run_sweep().await.unwrap();
    assert_eq!((again.enforcement_applied, again.events_published), (0, 0));
    // Exactly once: every pending enforcement job delivered once, every pending event
    // (including the three leased by the crashed publisher) published once.
    assert_eq!(r.calling.delivered_event_ids().len(), 2);
    let mut delivered = r.calling.delivered_event_ids();
    delivered.sort();
    assert_eq!(delivered, pending_jobs);
    let published = recorder.0.lock().unwrap().clone();
    let mut per_event: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &published {
        *per_event.entry(e.as_str()).or_default() += 1;
    }
    assert!(
        per_event.values().all(|n| *n == 1),
        "an event was published twice"
    );
    for e in &pending_outbox {
        assert_eq!(
            per_event.get(e.as_str()),
            Some(&1),
            "{e} not published once"
        );
    }
    assert_eq!(
        r.count("SELECT count(*) FROM privacy_relay_outbox WHERE status <> 'PUBLISHED'")
            .await,
        0
    );
    assert_eq!(
        r.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status <> 'APPLIED'")
            .await,
        0
    );

    // Revoked / expired authority stays revoked / expired: new calls are refused.
    for (session, source, expected) in [
        (&s.revoked_session, s.other_principal, "CONTEXT_REVOKED"),
        (&s.expired_session, s.principal, "CONTEXT_EXPIRED"),
    ] {
        let d = r
            .post(
                "/internal/v1/communication/authorize",
                &s.c,
                json!({ "relaySessionId": session, "source": { "principalId": source },
                        "targets": [{ "principalId": Uuid::new_v4() }], "channel": "VOICE",
                        "route": "APPLICATION", "purpose": "CALL_CREATE" }),
            )
            .await;
        assert_eq!(d.json["decision"], "DENIED", "{}", d.text);
        assert_eq!(d.json["reason"], expected, "{}", d.text);
    }
    // A delayed inbound callback to the revoked session's former number cannot reach
    // its former destination; the active mapping still resolves.
    let delayed = inbound(&r, COOLDOWN_RELAY, 3).await;
    assert_eq!(
        (delayed.status, delayed.code()),
        (StatusCode::CONFLICT, "RELAY_MAPPING_NOT_ACTIVE"),
        "{}",
        delayed.text
    );
    let live = inbound(&r, ACTIVE_RELAY, 22).await;
    assert_eq!(live.status, StatusCode::OK, "{}", live.text);
    assert_eq!(
        live.json["resolution"]["relaySessionId"].as_str(),
        Some(s.active_session.as_str())
    );
    // The cooldown survived: same deadline, and no new session can take the number.
    assert_eq!(
        rows(
            &restored.conn,
            "SELECT operational_status, cooldown_until::text FROM relay_number_inventory \
             WHERE operational_status = 'COOLDOWN'",
        )
        .await,
        cooldown
    );
    let (_, fresh_session) = relay_context(&r, &s.c, Uuid::new_v4(), &s.contact).await;
    let refused = r.assign_relay_number(&s.c, &fresh_session).await;
    assert_eq!(
        (refused.status, refused.code()),
        (StatusCode::CONFLICT, "RELAY_NUMBER_UNAVAILABLE"),
        "{}",
        refused.text
    );
    // The encrypted contact still resolves with the keyring.
    let resolved = resolve_contact(&r, &s).await;
    assert_eq!(resolved.status, StatusCode::OK, "{}", resolved.text);
    assert_eq!(resolved.json["resolution"]["e164"], "+442079460123");
    // Only once the cooldown ends does the number return (worker), never earlier.
    r.clock.set(at(5 + 61));
    one.run_sweep().await.unwrap();
    assert_eq!(
        r.assign_relay_number(&s.c, &fresh_session).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        r.count("SELECT count(*) FROM privacy_relay_incidents")
            .await,
        0,
        "a clean restore raises nothing"
    );

    // ---- Restore WITHOUT the keyring (the key version is missing): fail closed. ----
    let keyless = restore_into_fresh(&dump).await;
    let mut k = Harness::on(
        keyless.clone(),
        ManualClock::new(at(23)),
        ALL_CALLING_PERMISSIONS,
    );
    k.set_vault_keys(test_vault_keys("ck-replacement-1", "fk-dr-1"));
    assert!(
        k.contacts.verify_referenced_key_versions().await.is_err(),
        "the startup guard refuses to serve"
    );
    let refused = resolve_contact(&k, &s).await;
    assert_eq!(
        refused.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        refused.text
    );
    assert!(
        !refused.text.contains(CONTACT_DIGITS),
        "never a plaintext fallback"
    );
    let incident = rows(
        &keyless.conn,
        "SELECT category, status, dedupe_key FROM privacy_relay_incidents ORDER BY dedupe_key",
    )
    .await;
    assert_eq!(
        incident,
        vec![vec![
            "CONTACT_KEY_UNAVAILABLE".to_string(),
            "OPEN".into(),
            "CONTACT_KEY_UNAVAILABLE:ENCRYPTION:UNKNOWN_KEY_VERSION:ck-dr-1".into()
        ]]
    );
    assert!(!dump_database(&keyless.conn).await.contains(CONTACT_DIGITS));
}

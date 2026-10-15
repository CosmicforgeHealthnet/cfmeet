//! Disaster recovery of Telephony's database (Part I §88) and worker-backlog recovery.
//!
//! `restored_database_resumes_without_duplicate_or_unauthorized_paid_effects`: legs in
//! several states, PENDING commands, a command past the transmit fence whose worker died
//! (outcome unknown), provider events and undelivered outbox rows are backed up with
//! `infra/deployment/backup/backup.sh --service telephony` and restored with `restore.sh`
//! into a fresh database. The migrator has nothing pending; the real command worker,
//! reconciler and outbox passes (two instances) then run against the restored database
//! with the deterministic interconnect — the same "carrier", which outlived the
//! database. Every PENDING command originates exactly once, the post-fence command is
//! NEVER re-sent (UNKNOWN → reconciliation binds the existing call), no leg or provider
//! call appears that no command authorized, usage is never duplicated, undelivered
//! events reach Calling, and the dump carries no provider secret or destination number.
//!
//! Needs `pg_dump` / `pg_restore`: without them it prints
//! `SKIPPED: disaster-recovery certification (pg_dump not found)` — unless
//! `DISASTER_RECOVERY_CERT_REQUIRED=1`, which makes it fail instead.
//!
//! `a_stopped_worker_backlog_drains_exactly_once_in_per_leg_order` needs no tools: a
//! backlog of enterprise SIP commands queued while every worker was stopped is drained
//! by two concurrent workers — each command executed exactly once, each leg's commands
//! in creation order.

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Command,
    sync::Arc,
};

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::{
    all_on, edge::ScriptedEdge, fast_settings, FakeCalling, Harness, CALLBACK_SECRET,
    CALLING_PERMISSIONS, PREVIOUS_CALLBACK_SECRET,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use telephony::{
    clock::ManualClock,
    domain::{LegStatus, NormalizedEvent},
    providers::{deterministic::Script, ProviderRegistry},
    service::TelephonyService,
    sip::{EdgeEnterpriseDialog, EdgeEvent, EdgeEventKind},
};
use telephony_migration::MigratorTrait;
use uuid::Uuid;

const DESTINATION: &str = "+2348031234567";

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `true` when the backup tooling can run; otherwise SKIPPED (or a failure when the
/// certification is required).
fn backup_tools_present() -> bool {
    if tool_available("pg_dump") && tool_available("pg_restore") && tool_available("psql") {
        return true;
    }
    if std::env::var("DISASTER_RECOVERY_CERT_REQUIRED").as_deref() == Ok("1") {
        panic!(
            "DISASTER_RECOVERY_CERT_REQUIRED=1 but pg_dump / pg_restore / psql are not installed"
        );
    }
    eprintln!("SKIPPED: disaster-recovery certification (pg_dump not found)");
    false
}

fn backup_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/deployment/backup")
        .join(name)
}

fn run_script(name: &str, args: &[&str]) {
    let out = Command::new("bash")
        .arg(backup_script(name))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(
        out.status.success(),
        "{name} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A restore-target database (created empty, dropped on drop).
struct RestoreTarget {
    admin: String,
    name: String,
    url: String,
}

impl RestoreTarget {
    async fn create(source_url: &str) -> Self {
        let admin = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let (base, source) = source_url.rsplit_once('/').unwrap();
        let name = format!("{source}_r");
        let conn = Database::connect(&admin).await.unwrap();
        conn.execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .await
            .unwrap();
        conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        conn.close().await.ok();
        Self {
            admin,
            url: format!("{base}/{name}"),
            name,
        }
    }
}

impl Drop for RestoreTarget {
    fn drop(&mut self) {
        let (admin, name) = (self.admin.clone(), self.name.clone());
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                if let Ok(conn) = Database::connect(&admin).await {
                    let _ = conn
                        .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                        .await;
                    conn.close().await.ok();
                }
            });
        })
        .join();
    }
}

async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index::<i64>(0)
    .unwrap()
}

async fn texts(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}

async fn answer_leg(h: &Harness, leg: Uuid) {
    let correlation = h.correlation_of(leg).await;
    for event in [NormalizedEvent::LegRinging, NormalizedEvent::LegAnswered] {
        let e = h.provider_a.progress(&correlation, event, h.now());
        h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    }
}

#[tokio::test]
async fn restored_database_resumes_without_duplicate_or_unauthorized_paid_effects() {
    let Some(h) = Harness::new().await else {
        return;
    };
    if !backup_tools_present() {
        return;
    }
    // --- The pre-disaster state -----------------------------------------------------
    // Connected (answered) leg.
    let (answered, _) = h.originate().await;
    h.run().await;
    answer_leg(&h, answered).await;
    // Ended leg with its usage finalized.
    let (ended, _) = h.originate().await;
    h.run().await;
    answer_leg(&h, ended).await;
    h.advance(30);
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{ended}/terminate"),
            Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(),
                         "idempotencyKey": format!("term-{ended}"),
                         "correlationId": "corr-dr-terminate-0001", "reason": "CALL_ENDED" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    h.run().await;
    h.run().await;
    assert_eq!(h.leg_status(ended).await, "ENDED");
    // Busy leg (definite failure).
    h.provider_a.script(&[Script::Fail {
        transmitted: true,
        reason: telephony::domain::EndReason::Busy,
    }]);
    let (busy, _) = h.originate().await;
    h.run().await;
    assert_eq!(h.leg_status(busy).await, "FAILED");
    // Past the fence: the provider created the call, the worker died before recording
    // anything (RUNNING, lease held, outcome unknown).
    h.provider_a.script(&[Script::AcceptThenCrash]);
    let (sent, _) = h.originate().await;
    let svc = h.svc.clone();
    let crashed = tokio::spawn(async move { svc.run_commands_once(10).await }).await;
    assert!(crashed.unwrap_err().is_panic());
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_commands WHERE telephony_leg_id = '{sent}'"
        ))
        .await
        .unwrap(),
        "RUNNING"
    );
    // Calling goes down: PENDING commands (workers stopped) and undelivered events.
    h.calling.set_available(false);
    let mut pending = Vec::new();
    for _ in 0..3 {
        pending.push(h.originate().await.0);
    }
    let correlation = h.correlation_of(answered).await;
    let connected = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegConnected, h.now());
    h.svc
        .ingest_provider_event("dp-a", connected)
        .await
        .unwrap();
    let undelivered = texts(
        &h.db.conn,
        "SELECT id::text FROM telephony_outbox WHERE delivered_at IS NULL AND dead_lettered_at IS NULL",
    )
    .await;
    assert!(!undelivered.is_empty());
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_commands WHERE status = 'PENDING'")
            .await,
        3
    );
    let source_legs: BTreeSet<String> = texts(&h.db.conn, "SELECT id::text FROM telephony_legs")
        .await
        .into_iter()
        .collect();
    let source_usage = h
        .scalar("SELECT count(*) FROM telephony_usage_records")
        .await;
    assert!(source_usage >= 1);
    let requests_before = h.provider_a.requests().len() + h.provider_b.requests().len();
    let effects_before = h.provider_a.effects().len() + h.provider_b.effects().len();
    let sent_correlation = h.correlation_of(sent).await;

    // --- Backup, disaster, restore into a fresh database ------------------------------
    let dir = std::env::temp_dir().join(format!("tel-dr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dump = dir.join("telephony.dump");
    let dump_path = dump.to_str().unwrap();
    run_script(
        "backup.sh",
        &[
            "--service",
            "telephony",
            "--database-url",
            &h.db.url,
            "--out",
            dump_path,
        ],
    );
    assert!(dir.join("telephony.dump.sha256").exists());
    let target = RestoreTarget::create(&h.db.url).await;
    run_script(
        "restore.sh",
        &["--database-url", &target.url, "--in", dump_path],
    );

    // No provider secret material and no destination number in the dump: the archive
    // bytes and its decompressed SQL.
    let raw = std::fs::read(&dump).unwrap();
    let sql = Command::new("pg_restore")
        .args(["--file=-", dump_path])
        .output()
        .unwrap();
    assert!(sql.status.success());
    assert!(sql.stdout.len() > 1000, "the SQL script was produced");
    for bytes in [&raw, &sql.stdout] {
        for secret in [
            CALLBACK_SECRET,
            PREVIOUS_CALLBACK_SECRET,
            DESTINATION.as_bytes(),
        ] {
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret),
                "{} found in the backup",
                String::from_utf8_lossy(secret)
            );
        }
    }
    std::fs::remove_dir_all(&dir).ok();

    let restored = Database::connect(&target.url).await.unwrap();
    assert!(
        telephony_migration::Migrator::get_pending_migrations(&restored)
            .await
            .unwrap()
            .is_empty(),
        "the migrator has nothing pending after a restore of the same release"
    );

    // --- The restored deployment's workers (two instances) ------------------------------
    let clock = ManualClock::new(h.now() + Duration::seconds(60));
    let calling = Arc::new(FakeCalling::new());
    let svc = TelephonyService::new(restored.clone(), Arc::new(clock.clone()), all_on())
        .with_providers(
            ProviderRegistry::new()
                .with(Arc::new(h.provider_a.clone()))
                .with(Arc::new(h.provider_b.clone())),
        )
        .with_privacy_relay(h.privacy.clone())
        .with_calling(calling.clone())
        .with_settings(fast_settings());
    let other = svc.clone().as_instance("w-restored-2");
    for _ in 0..6 {
        let (a, b) = tokio::join!(svc.sweep_once(), other.sweep_once());
        a.unwrap();
        b.unwrap();
        clock.advance(Duration::seconds(10));
    }

    // PENDING commands: executed exactly once each (one origination per command).
    let requests = [h.provider_a.requests(), h.provider_b.requests()].concat();
    assert_eq!(requests.len(), requests_before + 3, "{requests:?}");
    for leg in &pending {
        let keys = texts(
            &restored,
            &format!(
                "SELECT correlation_key FROM provider_call_bindings WHERE telephony_leg_id = '{leg}'"
            ),
        )
        .await;
        assert_eq!(keys.len(), 1, "one binding for {leg}");
        assert_eq!(
            requests.iter().filter(|r| **r == keys[0]).count(),
            1,
            "one origination for {leg}"
        );
        assert_eq!(
            texts(
                &restored,
                &format!(
                    "SELECT status FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
                ),
            )
            .await,
            vec!["SUCCEEDED".to_string()]
        );
    }
    // The post-fence command was NEVER re-sent: it went UNKNOWN and reconciliation bound
    // the call the provider already had.
    assert_eq!(
        requests.iter().filter(|r| **r == sent_correlation).count(),
        1
    );
    let sent_command = texts(
        &restored,
        &format!(
            "SELECT status || ':' || coalesce(result_code, '') FROM telephony_commands \
             WHERE telephony_leg_id = '{sent}' AND kind = 'ORIGINATE'"
        ),
    )
    .await;
    assert_eq!(
        sent_command,
        vec!["SUCCEEDED:RESOLVED_ACCEPTED".to_string()]
    );
    assert_eq!(
        scalar(
            &restored,
            &format!(
                "SELECT count(*) FROM provider_call_bindings WHERE telephony_leg_id = '{sent}' AND status = 'BOUND'"
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &restored,
            &format!(
                "SELECT count(*) FROM telephony_reconciliation_runs WHERE telephony_leg_id = '{sent}' \
                 AND case_code = 'UNKNOWN_ORIGINATION'"
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &restored,
            "SELECT count(*) FROM telephony_commands WHERE status = 'DEAD_LETTER'"
        )
        .await,
        0
    );
    // No leg and no provider call that no command authorized.
    let restored_legs: BTreeSet<String> = texts(&restored, "SELECT id::text FROM telephony_legs")
        .await
        .into_iter()
        .collect();
    assert_eq!(restored_legs, source_legs);
    assert_eq!(
        h.provider_a.effects().len() + h.provider_b.effects().len(),
        effects_before + 3
    );
    assert_eq!(
        scalar(
            &restored,
            "SELECT count(*) FROM telephony_legs l WHERE l.status NOT IN ('ENDED', 'FAILED') \
             AND NOT EXISTS (SELECT 1 FROM telephony_commands c WHERE c.telephony_leg_id = l.id \
                             AND c.kind = 'ORIGINATE' AND c.status IN ('SUCCEEDED', 'UNKNOWN_OUTCOME', 'RECONCILING'))"
        )
        .await,
        0
    );
    // Usage: never duplicated (one record per leg at most; the ended leg's kept as is).
    assert_eq!(
        scalar(
            &restored,
            "SELECT count(*) - count(DISTINCT telephony_leg_id) FROM telephony_usage_records"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &restored,
            &format!(
                "SELECT count(*) FROM telephony_usage_records WHERE telephony_leg_id = '{ended}'"
            )
        )
        .await,
        1
    );
    // The undelivered events reached Calling, each once.
    let delivered: Vec<Value> = calling
        .events()
        .iter()
        .map(|e| e["eventId"].clone())
        .collect();
    for id in &undelivered {
        assert_eq!(
            delivered.iter().filter(|d| d.as_str() == Some(id)).count(),
            1,
            "event {id}"
        );
    }
    let unique: BTreeSet<String> = delivered.iter().map(|d| d.to_string()).collect();
    assert_eq!(unique.len(), delivered.len(), "no event delivered twice");
    restored.close().await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_worker_backlog_drains_exactly_once_in_per_leg_order() {
    let edge = ScriptedEdge::default();
    let mut permissions = CALLING_PERMISSIONS.to_vec();
    permissions.push("telephony-leg.media");
    let Some(h) = Harness::build(
        &permissions,
        all_on().with_enterprise_sip(true),
        Some(Arc::new(edge.clone())),
    )
    .await
    else {
        return;
    };
    let now = h.clock_now();
    h.sql(&format!(
        "INSERT INTO telephony_enterprise_devices (id, device_ref, edge_endpoint, tenant_id, application_id, \
           environment_id, status, created_at, updated_at) \
         VALUES ('{}', 'dev-front-desk-01', 'frontdesk', '{}', '{}', '{}', 'ENABLED', '{now}', '{now}')",
        Uuid::new_v4(),
        h.consumer.tenant_id,
        h.consumer.application_id,
        h.consumer.environment_id,
    ))
    .await;
    // Workers stopped: N legs, each with ANSWER then three PROMPTs queued.
    const LEGS: usize = 8;
    const PROMPTS: [&str; 3] = ["WELCOME", "MENU", "NO_INPUT"];
    let mut channels = Vec::new();
    for _ in 0..LEGS {
        let channel = format!("chan-{}", Uuid::new_v4().simple());
        edge.add_live(&channel, LegStatus::Ringing);
        h.svc
            .handle_edge_event(EdgeEvent {
                event_id: format!("t-{}", Uuid::new_v4().simple()),
                channel: channel.clone(),
                application_media: false,
                correlation_key: None,
                kind: EdgeEventKind::EnterpriseInbound(EdgeEnterpriseDialog {
                    endpoint: "frontdesk".into(),
                    extension: "6000".into(),
                }),
                at: h.now(),
            })
            .await
            .unwrap();
        channels.push(channel);
    }
    let mut legs = BTreeMap::new();
    for channel in &channels {
        let leg: Uuid = h
            .text(&format!(
                "SELECT id::text FROM telephony_legs WHERE edge_channel_ref = '{channel}'"
            ))
            .await
            .unwrap()
            .parse()
            .unwrap();
        legs.insert(channel.clone(), leg);
    }
    // Queued interleaved across legs (as concurrent calls would), 1 ms apart.
    let mut queued = 0;
    for step in 0..=PROMPTS.len() {
        for channel in &channels {
            let leg = legs[channel];
            let id = Uuid::new_v4();
            let (action, extra) = if step == 0 {
                ("answer", json!({}))
            } else {
                ("prompt", json!({ "prompt": PROMPTS[step - 1] }))
            };
            let mut body = json!({
                "schemaVersion": "1",
                "commandId": id,
                "idempotencyKey": format!("{action}-{}", id.simple()),
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            });
            for (k, v) in extra.as_object().unwrap() {
                body[k] = v.clone();
            }
            let r = h
                .calling(
                    Method::POST,
                    &format!("/internal/v1/legs/{leg}/{action}"),
                    Some(body),
                )
                .await;
            assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
            queued += 1;
            h.clock.advance(Duration::milliseconds(1));
        }
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_commands WHERE status = 'PENDING'")
            .await,
        queued
    );
    assert!(edge.plays().is_empty());

    // Two workers restart concurrently with small batches.
    let other = h.svc.clone().as_instance("w-backlog-2");
    let mut rounds = 0;
    while h
        .scalar("SELECT count(*) FROM telephony_commands WHERE status <> 'SUCCEEDED'")
        .await
        > 0
    {
        let (a, b) = tokio::join!(h.svc.run_commands_once(3), other.run_commands_once(3));
        a.unwrap();
        b.unwrap();
        rounds += 1;
        assert!(rounds < 100, "the backlog did not drain");
    }
    // Exactly once: every command SUCCEEDED on its first attempt; every prompt played
    // once; per leg in creation order.
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_commands WHERE status = 'SUCCEEDED' AND attempts = 1"
        )
        .await,
        queued
    );
    let plays = edge.plays();
    assert_eq!(plays.len(), LEGS * PROMPTS.len());
    let expected: Vec<String> = PROMPTS
        .iter()
        .map(|p| telephony::enterprise::prompt_sound(p).unwrap().to_string())
        .collect();
    for channel in &channels {
        let order: Vec<String> = plays
            .iter()
            .filter(|(c, _)| c == channel)
            .map(|(_, s)| s.clone())
            .collect();
        assert_eq!(order, expected, "per-leg order on {channel}");
        assert_eq!(h.leg_status(legs[channel]).await, "ANSWERED");
    }
}

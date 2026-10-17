//! Part K §88: disaster recovery of the Calling database with the shipped tooling
//! (`infra/deployment/backup/backup.sh` / `restore.sh`).
//!
//! A deployment is seeded with realistic in-flight state — a CONNECTED application call,
//! a public call whose durable TERMINATE is still pending (Telephony unreachable), pending
//! outbox events, pending webhook deliveries (one claimed by a worker that "crashed"
//! holding its lease), a READY voicemail artifact with its object key and a DELETED one —
//! then backed up, restored into a fresh database, migrated (nothing pending) and served by
//! a NEW Calling instance whose workers run against the restored rows. Proven:
//!
//! * the restore is complete (every table's row count equals the source);
//! * leases recover and pending work drains EXACTLY ONCE: one TERMINATE for the pending
//!   leg, no new originate (no second paid leg), no new usage fact, every webhook
//!   delivery received once, every outbox event published once — a second worker pass
//!   finds nothing to do;
//! * canonical state reconciles (the connected call is reconciled against the surviving
//!   media provider and stays CONNECTED);
//! * a restored artifact still requires authorization (its owner's credential reads it,
//!   another tenant gets 404; the deleted one stays deleted and unservable);
//! * nothing in the dump — raw archive bytes or the rendered SQL — contains a telephone
//!   number or a test secret.
//!
//! A second test recovers a WORKER BACKLOG: with every worker stopped (and Telephony
//! unreachable) N public calls leave their originate commands and outbox events pending;
//! two worker instances then start concurrently over the same database and drain it — no
//! command executed twice, every event published once, per-call event order intact.
//!
//! Tooling: without `pg_dump` / `pg_restore` / `psql` the DR test prints
//! `SKIPPED: disaster-recovery certification (pg_dump not found)` — unless
//! `DISASTER_RECOVERY_CERT_REQUIRED=1`, in which case it FAILS.

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

use axum::http::{Method, StatusCode};
use calling::{
    domain::{Clock, ManualClock, TenantClassification},
    platform::scopes::names,
    telephony::LegControl,
    webhooks::worker::{EventTransport, LocalWebhookTransport},
};
use calling_migration::MigratorTrait;
use common::{
    deployment::{Deployment, Receiver},
    enterprise::{allow_enterprise_scopes, credential, strings},
    public::number_like_runs,
    scalar_i64, secret_part, TestDb, TEST_SIGNALING_KEY,
};
use serde_json::{json, Value};
use uuid::Uuid;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The backup tooling, or `None` (printed SKIPPED) when PostgreSQL client tools are
/// missing — a hard failure when `DISASTER_RECOVERY_CERT_REQUIRED=1`.
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
    let mut out = BTreeMap::new();
    for t in strings(
        conn,
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
    )
    .await
    {
        let n = scalar_i64(conn, &format!("SELECT count(*) FROM \"{t}\"")).await;
        out.insert(t, n);
    }
    out
}

#[tokio::test]
async fn a_restored_backup_recovers_pending_work_exactly_once() {
    let Some(tools) = tooling() else {
        return;
    };
    let Some(d) = Deployment::new().await else {
        return;
    };
    let a = d.a();
    d.enable_artifacts().await;

    // Customer webhooks (the receiver survives the database disaster).
    let rx = Receiver::start().await;
    let manager = d
        .credential(&[
            names::WEBHOOKS_READ,
            names::WEBHOOKS_MANAGE,
            names::CALLS_READ,
        ])
        .await;
    let r =
        d.e.send(
            manager.secret.expose_once(),
            Method::POST,
            "/v1/webhooks",
            Some(
                json!({ "url": rx.url, "eventTypes": ["call.ringing", "call.cancelled",
                          "call.connected"] }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let signing_secret = r.json["signingSecret"].as_str().unwrap().to_string();

    // 1. An active application call.
    let (active, _, _) = a.connected_call("VOICE").await;
    // 2. A public call whose TERMINATE is durable and pending (Telephony unreachable).
    let r = d.call_contact("dr-public-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let public: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let (leg, _) = d.public_leg(public).await;
    let telephony_leg = d.x.telephony.telephony_leg_of(leg).expect("originated");
    d.x.gated_telephony.set_controls_down(true);
    let (_, v) = a.state(public).await;
    let r = a.command(public, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        strings(
            d.h().conn(),
            &format!(
                "SELECT kind || ':' || status FROM call_telephony_commands \
                  WHERE call_id = '{public}' AND kind = 'TERMINATE'"
            )
        )
        .await,
        vec!["TERMINATE:PENDING".to_string()]
    );
    // 3. Voicemail artifacts: one READY (object stored), one DELETED.
    let vm = d
        .credential(&[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ])
        .await;
    let vm_token = vm.secret.expose_once().to_string();
    let caller = d.desk("dr-caller").await;
    let owner = d.desk("dr-owner").await;
    let other_owner = d.desk("dr-owner-2").await;
    let (_, message, _) = d.voicemail(&vm_token, &owner.0, &caller, "4600").await;
    let (_, deleted, _) = d
        .voicemail(&vm_token, &other_owner.0, &caller, "4601")
        .await;
    let path = format!("/v1/enterprise/voicemail/messages/{deleted}");
    let version = d.e.send(&vm_token, Method::GET, &path, None).await.json["version"]
        .as_i64()
        .unwrap();
    let r =
        d.e.send(
            &vm_token,
            Method::DELETE,
            &format!(
                "{path}?expectedVersion={version}&principalId={}",
                other_owner.0.id()
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    d.h().state.app.expire_due(100).await.unwrap();
    let artifact = |m: Uuid| {
        let conn = d.h().conn().clone();
        async move {
            strings(
                &conn,
                &format!(
                    "SELECT state || ':' || COALESCE(object_key, '') FROM voicemail_messages WHERE id = '{m}'"
                ),
            )
            .await
            .pop()
            .unwrap()
        }
    };
    let ready = artifact(message).await;
    assert!(ready.starts_with("READY:") && ready.len() > 6, "{ready}");
    let object_key = ready.trim_start_matches("READY:").to_string();
    assert!(d.x.store.keys().contains(&object_key));
    assert_eq!(artifact(deleted).await, "DELETED:");
    // 4. Outbox: one publication pass (webhook events + PENDING deliveries), then more
    //    activity that stays unpublished; a worker claims one delivery and "crashes".
    let transport = LocalWebhookTransport::new((*d.h().state.webhooks).clone());
    d.h()
        .state
        .webhooks
        .publish_outbox_once(&transport, 10_000)
        .await
        .unwrap();
    let (ringing, _, _) = a.ringing_call("VIDEO").await;
    let claimed = d.h().state.webhooks.claim_due(1).await.unwrap();
    assert_eq!(claimed.len(), 1, "a delivery leased by the crashed worker");
    // A Telephony dispatcher that crashed holding the TERMINATE's lease.
    common::exec(
        d.h().conn(),
        &format!(
            "UPDATE call_telephony_commands SET lease_until = now() + interval '30 seconds' \
              WHERE call_id = '{public}' AND kind = 'TERMINATE'"
        ),
    )
    .await
    .unwrap();
    let pending_outbox = scalar_i64(
        d.h().conn(),
        "SELECT count(*) FROM calling_outbox_events WHERE status = 'PENDING'",
    )
    .await;
    assert!(pending_outbox > 0);
    assert!(
        scalar_i64(
            d.h().conn(),
            "SELECT count(*) FROM webhook_deliveries WHERE status IN ('PENDING', 'DELIVERING')"
        )
        .await
            > 1
    );
    let executions_before = d.x.telephony.executions().len();
    let originates_before = d.x.telephony.originates().len();
    let usage_sql =
        "SELECT call_id::text || ':' || idempotency_key FROM call_usage_records ORDER BY 1";
    let usage_before = strings(d.h().conn(), usage_sql).await;
    assert!(
        rx.seen().is_empty(),
        "nothing delivered before the disaster"
    );
    let source_counts = table_counts(d.h().conn()).await;

    // ---------------------------------------------------------------- backup
    let dir = std::env::temp_dir().join(format!("calling-dr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dump = dir.join("calling.dump");
    let dump_s = dump.to_str().unwrap();
    run(
        &tools.join("backup.sh"),
        &[
            "--service",
            "calling",
            "--database-url",
            &d.h().db.url,
            "--out",
            dump_s,
        ],
    );
    // No telephone number or secret in the archive — raw bytes and rendered SQL.
    let raw = std::fs::read(&dump).unwrap();
    let rendered = Command::new("pg_restore")
        .args(["--no-owner", "--no-privileges", "-f", "-", dump_s])
        .output()
        .unwrap();
    assert!(rendered.status.success());
    let sql = String::from_utf8_lossy(&rendered.stdout).to_string();
    assert!(
        sql.contains("voicemail_messages"),
        "the rendered dump has the data"
    );
    let raw_text = String::from_utf8_lossy(&raw).to_string();
    let mut forbidden: Vec<String> = vec![
        "+234".into(),
        common::TEST_PEPPER.into(),
        TEST_SIGNALING_KEY.into(),
        signing_secret.clone(),
        "grant-secret-".into(),
    ];
    for token in [
        d.e.a.w.token.secret.expose_once(),
        d.e.admin.secret.expose_once(),
        d.e.caller.secret.expose_once(),
        manager.secret.expose_once(),
        &vm_token,
    ] {
        forbidden.push(token.to_string());
        forbidden.push(secret_part(token).to_string());
    }
    for needle in &forbidden {
        assert!(
            !raw_text.contains(needle.as_str()),
            "the archive contains {needle:.12}…"
        );
        assert!(
            !sql.contains(needle.as_str()),
            "the rendered dump contains {needle:.12}…"
        );
    }
    assert!(
        number_like_runs(&sql).is_empty(),
        "{:?}",
        number_like_runs(&sql)
    );

    // ---------------------------------------------------------------- restore
    let mut target = TestDb::empty().await.unwrap();
    // Only Calling's database is restored: the identity authority (Platform Access, its
    // own database) is the same one.
    target.platform_access = Some(d.h().db.pa());
    run(
        &tools.join("restore.sh"),
        &["--database-url", &target.url, "--in", dump_s],
    );
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        calling_migration::Migrator::get_pending_migrations(&target.conn)
            .await
            .unwrap()
            .is_empty()
    );
    calling_migration::Migrator::up(&target.conn, None)
        .await
        .unwrap();
    assert_eq!(
        table_counts(&target.conn).await,
        source_counts,
        "the restore is complete"
    );

    // A new Calling instance over the restored database; time has moved on (every lease
    // of the crashed workers expired), Telephony is reachable again.
    d.x.gated_telephony.set_controls_down(false);
    let clock = ManualClock::new(d.h().clock.now() + chrono::Duration::minutes(5));
    let restored = d.x.instance(Arc::new(target), clock.clone());
    let transport = LocalWebhookTransport::new((*restored.state.webhooks).clone());
    let mut first_pass = None;
    for pass in 0..3 {
        // The worker passes of `main`: expiry and reconciliation, durable Telephony
        // commands, outbox publication, webhook delivery.
        restored.state.app.expire_due(100).await.unwrap();
        let reconciled = restored.state.app.reconcile_due(100).await.unwrap();
        let commands = restored
            .state
            .app
            .dispatch_telephony_commands(50)
            .await
            .unwrap();
        let published = restored
            .state
            .webhooks
            .publish_outbox_once(&transport, 10_000)
            .await
            .unwrap();
        let delivered = restored.state.webhooks.deliver_due_once(100).await.unwrap();
        if pass == 0 {
            first_pass = Some((commands, published, delivered, reconciled));
        } else {
            // Nothing is left: a second pass is a no-op.
            assert_eq!(commands, 0, "pass {pass}");
            assert_eq!(published.published, 0, "pass {pass}");
            assert_eq!(delivered.succeeded, 0, "pass {pass}");
        }
        clock.advance(chrono::Duration::minutes(1));
    }
    let (commands, published, delivered, reconciled) = first_pass.unwrap();
    println!(
        "DR first pass: commands={commands} published={} delivered={} reconciled={}",
        published.published, delivered.succeeded, reconciled.calls_reconciled
    );
    assert!(commands >= 1);
    // The restored backlog, plus what the recovered expiry produced (the ringing call's
    // offers expired during the outage).
    assert!(published.published as i64 >= pending_outbox);
    // Exactly one TERMINATE for the pending leg; no new originate / paid leg.
    let terminations: Vec<_> =
        d.x.telephony
            .controls()
            .into_iter()
            .filter(|(t, action, _)| *t == telephony_leg && *action == LegControl::Terminate)
            .collect();
    assert_eq!(terminations.len(), 1, "one TERMINATE after recovery");
    assert_eq!(d.x.telephony.executions().len(), executions_before);
    assert_eq!(d.x.telephony.originates().len(), originates_before);
    let conn = restored.conn();
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT count(*) FROM call_telephony_commands WHERE status = 'PENDING'"
        )
        .await,
        0
    );
    // No duplicate usage fact: every fact of before is still there once; the only new
    // facts belong to the Call whose ringing expired during the outage (its usage is
    // final now), and none is a paid-leg fact.
    let usage_after = strings(conn, usage_sql).await;
    let new_facts: Vec<&String> = usage_after
        .iter()
        .filter(|f| !usage_before.contains(f))
        .collect();
    assert!(usage_before.iter().all(|f| usage_after.contains(f)));
    assert!(
        new_facts
            .iter()
            .all(|f| f.starts_with(&ringing.to_string()) && !f.contains("leg-usage")),
        "{new_facts:?}"
    );
    assert_eq!(
        strings(
            conn,
            &format!("SELECT current_state FROM calls WHERE id = '{ringing}'")
        )
        .await,
        vec!["NO_ANSWER".to_string()]
    );
    // Every outbox event published once, every webhook delivery received once.
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT count(*) FROM calling_outbox_events WHERE status <> 'PUBLISHED'"
        )
        .await,
        0
    );
    let received = rx.event_ids();
    let unique: BTreeSet<&String> = received.iter().collect();
    assert_eq!(
        unique.len(),
        received.len(),
        "no duplicate webhook side effect"
    );
    assert_eq!(
        received.len() as i64,
        scalar_i64(conn, "SELECT count(*) FROM webhook_deliveries").await
    );
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT count(*) FROM webhook_deliveries WHERE status <> 'SUCCEEDED'"
        )
        .await,
        0
    );
    // Canonical state reconciled against the surviving media provider.
    assert!(reconciled.calls_reconciled >= 1);
    let state = strings(
        conn,
        &format!("SELECT current_state FROM calls WHERE id = '{active}'"),
    )
    .await;
    assert_eq!(state, vec!["CONNECTED".to_string()]);
    assert_eq!(
        strings(
            conn,
            &format!("SELECT current_state FROM calls WHERE id = '{public}'")
        )
        .await,
        vec!["CANCELLED".to_string()]
    );

    // Restored artifacts still require authorization.
    let send = |token: String, method: Method, path: String, body: Option<Value>| {
        let restored = &restored;
        async move { restored.request(method, &path, Some(&token), body).await }
    };
    let msg_path = format!("/v1/enterprise/voicemail/messages/{message}");
    let r = send(
        vm_token.clone(),
        Method::GET,
        format!("{msg_path}?principalId={}", owner.0.id()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "READY");
    let r = send(
        vm_token.clone(),
        Method::POST,
        format!("{msg_path}/media-access"),
        Some(json!({ "principalId": owner.0.id() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // A credential of the right application without the voicemail scope: refused.
    let r = send(
        d.e.admin.secret.expose_once().to_string(),
        Method::POST,
        format!("{msg_path}/media-access"),
        Some(json!({ "principalId": owner.0.id() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Another tenant: exactly like an unknown id.
    let other = restored
        .consumer("dr-other", TenantClassification::External)
        .await;
    allow_enterprise_scopes(&restored, &other).await;
    let other_token = credential(
        &restored,
        &other,
        d.e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    for (method, p, body) in [
        (Method::GET, msg_path.clone(), None),
        (
            Method::POST,
            format!("{msg_path}/media-access"),
            Some(json!({})),
        ),
    ] {
        let r = send(
            other_token.secret.expose_once().to_string(),
            method,
            p.clone(),
            body,
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text);
    }
    // The deleted artifact stays deleted and is never served.
    let r = send(
        vm_token.clone(),
        Method::POST,
        format!("/v1/enterprise/voicemail/messages/{deleted}/media-access"),
        Some(json!({ "principalId": other_owner.0.id() })),
    )
    .await;
    assert!(!r.status.is_success(), "{}", r.text);
    assert_eq!(
        strings(
            conn,
            &format!("SELECT state || ':' || COALESCE(object_key, '') FROM voicemail_messages WHERE id = '{deleted}'")
        )
        .await,
        vec!["DELETED:".to_string()]
    );
    assert!(!common::dump_database(conn).await.contains("+234"));
}

/// (instance, call, sequence, event id) of one publication.
type Published = (usize, Uuid, i64, String);

/// Records what one worker instance published.
struct RecordingTransport {
    instance: usize,
    seen: Arc<Mutex<Vec<Published>>>,
}

#[async_trait::async_trait]
impl EventTransport for RecordingTransport {
    async fn publish(&self, _producer: &str, envelope: &Value) -> Result<(), &'static str> {
        let call: Uuid = envelope["payload"]["call_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("MALFORMED_ENVELOPE")?;
        let sequence = envelope["payload"]["sequence"]
            .as_i64()
            .ok_or("MALFORMED_ENVELOPE")?;
        self.seen.lock().unwrap().push((
            self.instance,
            call,
            sequence,
            envelope["event_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
        Ok(())
    }
}

#[tokio::test]
async fn a_worker_backlog_drains_once_across_two_restarted_instances() {
    let Some(d) = Deployment::new().await else {
        return;
    };
    let n = 20;
    // Workers stopped and Telephony unreachable: every originate stays durable and
    // pending, every event stays in the outbox.
    d.x.gated_telephony.set_down(true);
    let mut calls = Vec::new();
    for i in 0..n {
        let r = d.call_contact(&format!("backlog-{i}")).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        calls.push(
            r.json["call"]["callId"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let conn = d.h().conn();
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT count(*) FROM call_telephony_commands WHERE kind = 'ORIGINATE' AND status = 'PENDING'"
        )
        .await,
        n as i64
    );
    let backlog = scalar_i64(
        conn,
        "SELECT count(*) FROM calling_outbox_events WHERE status = 'PENDING'",
    )
    .await;
    assert!(backlog >= n as i64);
    assert!(
        d.x.telephony.originates().is_empty(),
        "nothing reached Telephony"
    );

    // Restart: two instances, Telephony back, every retry due.
    d.x.gated_telephony.set_down(false);
    d.h().clock.advance(chrono::Duration::seconds(60));
    let instances = [d.instance(), d.instance()];
    let seen = Arc::new(Mutex::new(Vec::new()));
    let transports: Vec<RecordingTransport> = (0..2)
        .map(|i| RecordingTransport {
            instance: i,
            seen: seen.clone(),
        })
        .collect();
    let mut dispatched = [0usize; 2];
    let started = std::time::Instant::now();
    for _ in 0..1_000 {
        let pass = |i: usize| {
            let (h, t) = (&instances[i], &transports[i]);
            async move {
                let c = h.state.app.dispatch_telephony_commands(3).await.unwrap();
                let p = h.state.webhooks.publish_outbox_once(t, 7).await.unwrap();
                (c, p.published + p.retried)
            }
        };
        let ((c0, p0), (c1, p1)) = tokio::join!(pass(0), pass(1));
        dispatched[0] += c0;
        dispatched[1] += c1;
        if c0 + c1 + p0 + p1 == 0 {
            break;
        }
    }
    let seen = seen.lock().unwrap().clone();
    println!(
        "backlog drained in {} ms: commands {:?}, events per instance [{}, {}]",
        started.elapsed().as_millis(),
        dispatched,
        seen.iter().filter(|s| s.0 == 0).count(),
        seen.iter().filter(|s| s.0 == 1).count()
    );
    // Drained.
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT count(*) FROM call_telephony_commands WHERE status = 'PENDING'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT count(*) FROM calling_outbox_events WHERE status = 'PENDING'"
        )
        .await,
        0
    );
    // No command executed twice: one originate reached Telephony per leg, one execution.
    let originates = d.x.telephony.originates();
    let keys: BTreeSet<String> = originates
        .iter()
        .map(|(_, c)| c["idempotencyKey"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(originates.len(), n, "one submission per command");
    assert_eq!(keys.len(), n);
    assert_eq!(d.x.telephony.executions().len(), n);
    for call in &calls {
        let (leg, _) = d.public_leg(*call).await;
        let bound = strings(
            conn,
            &format!("SELECT COALESCE(execution_binding_ref::text, '') FROM call_legs WHERE id = '{leg}'"),
        )
        .await;
        assert_eq!(
            bound,
            vec![d.x.telephony.telephony_leg_of(leg).unwrap().to_string()],
            "the leg is bound to its single execution"
        );
    }
    // Every event published exactly once; per call, the published sequences are exactly
    // the outbox's, in the producer's order (sequence follows creation order).
    let mut ids: Vec<&String> = seen.iter().map(|s| &s.3).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), seen.len(), "no event published twice");
    // The backlog plus what draining it produced (the originated legs moved the Calls on).
    assert!(seen.len() as i64 >= backlog);
    assert_eq!(
        seen.len() as i64,
        scalar_i64(conn, "SELECT count(*) FROM calling_outbox_events").await
    );
    let mut per_call: BTreeMap<Uuid, Vec<i64>> = BTreeMap::new();
    for (_, call, sequence, _) in &seen {
        per_call.entry(*call).or_default().push(*sequence);
    }
    for (call, mut published) in per_call {
        published.sort();
        let produced: Vec<i64> = strings(
            conn,
            &format!(
                "SELECT (envelope->'payload'->>'sequence') FROM calling_outbox_events \
                  WHERE aggregate_id = '{call}' ORDER BY created_at, (envelope->'payload'->>'sequence')::bigint"
            ),
        )
        .await
        .into_iter()
        .map(|s| s.parse().unwrap())
        .collect();
        let mut sorted = produced.clone();
        sorted.sort();
        assert_eq!(
            produced, sorted,
            "{call}: outbox order follows the call's sequence"
        );
        assert_eq!(published, sorted, "{call}: every event once");
    }
}

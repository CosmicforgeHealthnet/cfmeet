//! Part VI §19.2 / §19.4 — usage / limits ownership migration rehearsal on the REAL
//! processes, with credential continuity through the switch and its rollback.
//!
//! 1. Calling's historical usage (records finalized days before the migration, every
//!    usage type) is the authoritative evidence; `GET /v1/usage` and `GET /v1/limits`
//!    are Calling's (gateway central usage OFF) — the LEGACY baseline.
//! 2. Calling's unmodified exporter backfills the unified ledger from those immutable
//!    records (source ids = Calling record ids); repeated backfills (every stream reset
//!    twice) never double count.
//! 3. Shadow comparison per environment / usage type / day window: legacy totals ==
//!    ledger totals for every metered type.
//! 4. Switch (`API_GATEWAY_CENTRAL_USAGE_ENABLED=true`): `/v1/usage` and `/v1/limits` are
//!    served by the Developer Platform in the PUBLISHED contract shape; the metered call
//!    records are field-for-field identical to Calling's; Calling's full detail stays at
//!    the preserved `/v1/calling/usage` / `/v1/calling/limits`, unchanged.
//! 5. Credentials rotated, suspended, revoked and expiring around the switch behave the
//!    same before, during and after it — no credential is invalidated by the switch and
//!    no revoked / suspended one comes back.
//! 6. Rollback (flag unset): Calling serves `/v1/usage` again, identical to the legacy
//!    baseline; no ledger fact is lost; Calling's records were never mutated by metering
//!    and metering created no customer webhook delivery.
//!
//! Same prerequisites and skip rules as `tests/journeys.rs`.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use cfmeet_certification::{
    db,
    provision::{self, Auth, Gateway, Resp},
    stack::{self, Stack},
    StackConfig, StackOptions,
};
use reqwest::Method;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

static BUSY: AtomicBool = AtomicBool::new(false);

struct Serial;

impl Serial {
    async fn acquire() -> Serial {
        while BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Serial
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

async fn until<T, F, Fut>(secs: u64, what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Calling's exporter runs every second (no settle delay) and backfills 5 days; the
/// gateway starts with central usage OFF (Calling serves `/v1/usage` / `/v1/limits`).
fn options() -> StackOptions {
    StackOptions::default()
        .set(stack::CALLING, "CFMEET_USAGE_EXPORT_ENABLED", "true")
        .set(stack::CALLING, "CFMEET_USAGE_EXPORT_INTERVAL_SECONDS", "1")
        .set(stack::CALLING, "CFMEET_USAGE_EXPORT_SETTLE_SECONDS", "0")
        .set(stack::CALLING, "CFMEET_USAGE_EXPORT_BACKFILL_DAYS", "5")
        .set(
            stack::API_GATEWAY,
            "API_GATEWAY_CENTRAL_USAGE_ENABLED",
            "false",
        )
}

async fn commit(stack: &Stack, service: &str, sql: &str) {
    let db = stack.db(service).await.expect("service database");
    db.execute_unprepared(&format!(
        "BEGIN; SET LOCAL session_replication_role = replica; {sql}; COMMIT;"
    ))
    .await
    .unwrap_or_else(|e| panic!("{service} evidence: {e}"));
}

/// Historical Calling usage (finalized 2 days ago, as the pre-migration service wrote
/// it): one app call, one public-network leg with cost evidence, one recording artifact
/// and one participant fact (Calling detail the unified ledger does not meter).
async fn seed_history(stack: &Stack, t: Uuid, a: Uuid, e: Uuid) {
    let call = Uuid::new_v4();
    let u = Uuid::new_v4;
    commit(
        stack,
        stack::CALLING,
        &format!(
            "INSERT INTO call_usage_records (id, call_id, leg_id, participant_id, tenant_id, application_id, \
               environment_id, usage_type, quantity, unit, idempotency_key, finalized_at, created_at, updated_at, \
               route_class, destination_country, billable_seconds, cost_status, cost_minor, cost_currency, size_bytes) VALUES \
             ('{}', '{call}', NULL, NULL, '{t}', '{a}', '{e}', 'CALL_CONNECTED_TIME', 180000, 'MILLISECONDS', 'mig-call-{call}', \
               now() - interval '2 days', now() - interval '2 days', now() - interval '2 days', NULL, NULL, NULL, NULL, NULL, NULL, NULL), \
             ('{}', '{call}', '{}', NULL, '{t}', '{a}', '{e}', 'LEG_CONNECTED_TIME', 90000, 'MILLISECONDS', 'mig-leg-{call}', \
               now() - interval '2 days' + interval '1 second', now() - interval '2 days', now() - interval '2 days', \
               'PSTN', 'GB', 90, 'KNOWN', 31, 'USD', NULL), \
             ('{}', '{call}', NULL, NULL, '{t}', '{a}', '{e}', 'RECORDING_ARTIFACT', 60000, 'MILLISECONDS', 'mig-rec-{call}', \
               now() - interval '2 days' + interval '2 seconds', now() - interval '2 days', now() - interval '2 days', \
               NULL, NULL, NULL, NULL, NULL, NULL, 8192), \
             ('{}', '{call}', NULL, '{}', '{t}', '{a}', '{e}', 'PARTICIPANT_CONNECTED_TIME', 170000, 'MILLISECONDS', 'mig-part-{call}', \
               now() - interval '2 days' + interval '3 seconds', now() - interval '2 days', now() - interval '2 days', \
               NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
            u(),
            u(),
            u(),
            u(),
            u(),
            u()
        ),
    )
    .await;
}

fn days_ago(n: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(n))
        .date_naive()
        .to_string()
}

/// `(usage type -> (quantity, fact count))` of the environment's ledger, last week.
async fn ledger_totals(gw: &Gateway, session: Auth<'_>, env: Uuid) -> BTreeMap<String, (i64, i64)> {
    let r = gw
        .get(
            &format!("/v1/environments/{env}/usage?from={}", days_ago(7)),
            session,
        )
        .await;
    status(&r, 200, "usage summary");
    r.body["totals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["usageType"].as_str().unwrap().to_string(),
                (
                    t["quantity"].as_i64().unwrap(),
                    t["factCount"].as_i64().unwrap(),
                ),
            )
        })
        .collect()
}

/// `(usage type -> (quantity, records))` of a contract `UsageList` page set.
fn legacy_totals(records: &[Value]) -> BTreeMap<String, (i64, i64)> {
    let mut out: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for r in records {
        let e = out
            .entry(r["usageType"].as_str().unwrap().to_string())
            .or_default();
        e.0 += r["quantity"].as_i64().unwrap();
        e.1 += 1;
    }
    out
}

/// Every record of `path` (all pages).
async fn all_records(gw: &Gateway, path: &str, cred: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = match &cursor {
            Some(c) => format!("{path}{sep}limit=2&cursor={c}"),
            None => format!("{path}{sep}limit=2"),
        };
        let r = gw.get(&url, Auth::Credential(cred)).await;
        status(&r, 200, &url);
        out.extend(r.body["data"].as_array().unwrap().iter().cloned());
        match r.body["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => return out,
        }
    }
}

async fn central(stack: &Stack) -> bool {
    let r = Resp::from(
        stack
            .http
            .get(format!("{}/ready", stack.gateway_url()))
            .send()
            .await
            .unwrap(),
    )
    .await;
    status(&r, 200, "gateway /ready");
    r.body["centralUsage"].as_bool().unwrap()
}

async fn switch(stack: &mut Stack, on: bool) {
    stack
        .restart(
            stack::API_GATEWAY,
            &[(
                "API_GATEWAY_CENTRAL_USAGE_ENABLED",
                if on { "true" } else { "false" },
            )],
        )
        .await
        .expect("gateway restart");
    assert_eq!(central(stack).await, on);
}

/// The credential states around the switch.
struct Creds {
    untouched: String,
    rotated_successor: String,
    rotated_predecessor: String,
    suspended: String,
    revoked: String,
    expiring: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

async fn issue(
    gw: &Gateway,
    session: Auth<'_>,
    env: Uuid,
    sa: Uuid,
    body: Value,
) -> (Uuid, String) {
    let r = gw
        .post(
            &format!("/v1/environments/{env}/service-accounts/{sa}/credentials"),
            session,
            body,
        )
        .await;
    status(&r, 201, "create credential");
    (r.uuid("/credential/id"), r.str("/secret"))
}

async fn credentials(gw: &Gateway, session: Auth<'_>, env: Uuid, sa: Uuid) -> Creds {
    let scopes = json!(["usage.read", "calls.read"]);
    let (_, untouched) = issue(gw, session, env, sa, json!({ "scopes": scopes })).await;
    let (rot, rotated_predecessor) = issue(gw, session, env, sa, json!({ "scopes": scopes })).await;
    let r = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{rot}/rotate"),
            session,
            json!({ "overlapMinutes": 0 }),
        )
        .await;
    status(&r, 201, "rotate without overlap");
    let rotated_successor = r.str("/secret");
    let (sus, suspended) = issue(gw, session, env, sa, json!({ "scopes": scopes })).await;
    let r = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{sus}/suspend"),
            session,
            json!({ "reasonCode": "MIGRATION_REHEARSAL" }),
        )
        .await;
    status(&r, 200, "suspend");
    let (rev, revoked) = issue(gw, session, env, sa, json!({ "scopes": scopes })).await;
    let r = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{rev}/revoke"),
            session,
            json!({}),
        )
        .await;
    status(&r, 200, "revoke");
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(90);
    let (_, expiring) = issue(
        gw,
        session,
        env,
        sa,
        json!({ "scopes": scopes, "expiresAt": expires_at.to_rfc3339() }),
    )
    .await;
    Creds {
        untouched,
        rotated_successor,
        rotated_predecessor,
        suspended,
        revoked,
        expiring,
        expires_at,
    }
}

/// Every credential state holds on both routes families (the moved `/v1/usage` and a
/// Calling route that never moves).
async fn assert_credentials(gw: &Gateway, c: &Creds, phase: &str) {
    for path in ["/v1/usage", "/v1/calls"] {
        for (what, secret, expected) in [
            ("untouched", &c.untouched, 200),
            ("rotation successor", &c.rotated_successor, 200),
            (
                "rotated predecessor (no overlap)",
                &c.rotated_predecessor,
                401,
            ),
            ("suspended", &c.suspended, 401),
            ("revoked", &c.revoked, 401),
        ] {
            let r = gw.get(path, Auth::Credential(secret)).await;
            assert_eq!(
                r.status, expected,
                "{phase}: {what} credential on {path}: {}",
                r.text
            );
        }
        let expect = if chrono::Utc::now() < c.expires_at - chrono::Duration::seconds(5) {
            200
        } else if chrono::Utc::now() > c.expires_at + chrono::Duration::seconds(5) {
            401
        } else {
            continue;
        };
        let r = gw.get(path, Auth::Credential(&c.expiring)).await;
        assert_eq!(
            r.status, expect,
            "{phase}: expiring credential on {path}: {}",
            r.text
        );
    }
}

/// The stable part of a `Limits` view: limit, window, binding scope and ceilings (the
/// request-rate counter moves with every request; reset instants with the clock).
fn stable_limits(v: &Value) -> Vec<Value> {
    v["limits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            json!({
                "limit": l["limit"], "window": l["window"], "scope": l["scope"],
                "ceiling": l["ceiling"], "currency": l.get("currency"),
                "levels": l["levels"].as_array().unwrap().iter()
                    .map(|x| json!({ "scope": x["scope"], "ceiling": x["ceiling"] }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The comparable fields of a contract record (Calling's optional usage window aside).
fn comparable(r: &Value) -> Value {
    let mut v = r.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("usageStartedAt");
        o.remove("usageEndedAt");
    }
    v
}

#[tokio::test]
async fn usage_migration_rehearsal_journey() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    let _g = Serial::acquire().await;
    let mut stack = Stack::start_with(config, options())
        .await
        .expect("certification stack");
    let gw = stack.gateway();
    let operator = stack.operator();
    let sb = provision::developer_sandbox(
        &stack,
        &format!(
            "usage-migration-{}@cert.cfmeet.test",
            Uuid::new_v4().simple()
        ),
        &["usage.read", "calls.read", "credentials.manage"],
    )
    .await
    .expect("sandbox");
    let session = Auth::Session(&sb.user.session);
    let cred = sb.customer.secret.clone();
    let env = sb.environment_id;
    let sa = sb.customer.service_account_id;
    assert!(!central(&stack).await, "central usage starts OFF");

    // 1. Legacy baseline: Calling's own view of its historical usage.
    seed_history(&stack, sb.organization_id, sb.project_id, env).await;
    let legacy = all_records(&gw, "/v1/usage", &cred).await;
    assert_eq!(legacy.len(), 4, "every Calling usage type: {legacy:?}");
    let legacy_limits = gw.get("/v1/limits", Auth::Credential(&cred)).await;
    status(&legacy_limits, 200, "legacy /v1/limits");
    let calling_db = stack.db(stack::CALLING).await.unwrap();
    let calling_records = db::scalar_i64(
        &calling_db,
        &format!("SELECT count(*) FROM call_usage_records WHERE environment_id = '{env}'"),
    )
    .await
    .unwrap();

    // 2. Backfill from the immutable records (the exporter's backfill window).
    let ledger = until(60, "Calling history backfilled into the ledger", || async {
        let t = ledger_totals(&gw, session, env).await;
        (t.contains_key("CALL_MINUTE_APP")
            && t.contains_key("CALL_MINUTE_PUBLIC_NETWORK")
            && t.contains_key("RECORDING_MINUTE"))
        .then_some(t)
    })
    .await;
    // 3. Shadow comparison by environment / type / day window.
    let by_legacy = legacy_totals(&legacy);
    for (calling_type, ledger_type) in [
        ("CALL_CONNECTED_TIME", "CALL_MINUTE_APP"),
        ("LEG_CONNECTED_TIME", "CALL_MINUTE_PUBLIC_NETWORK"),
        ("RECORDING_ARTIFACT", "RECORDING_MINUTE"),
    ] {
        assert_eq!(
            by_legacy[calling_type], ledger[ledger_type],
            "shadow totals {calling_type} vs {ledger_type}"
        );
    }
    let day = days_ago(2);
    let window = gw
        .get(
            &format!("/v1/environments/{env}/usage?from={day}&to={day}"),
            session,
        )
        .await;
    status(&window, 200, "day window");
    let in_day: i64 = window.body["totals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["usageType"] == "CALL_MINUTE_APP")
        .map(|t| t["quantity"].as_i64().unwrap())
        .sum();
    assert_eq!(
        in_day, by_legacy["CALL_CONNECTED_TIME"].0,
        "the historical day window"
    );
    // Repeated backfills never double count.
    for round in 0..2 {
        let ov = operator
            .dp(Method::GET, "/operator/usage", None)
            .await
            .unwrap();
        for s in ov.body["streams"].as_array().unwrap() {
            if s["producer"] != "cfmeet.calling" {
                continue;
            }
            let r = operator
                .dp(
                    Method::POST,
                    &format!(
                        "/operator/usage/streams/cfmeet.calling/{}/reset",
                        s["stream"].as_str().unwrap()
                    ),
                    None,
                )
                .await
                .unwrap();
            status(&r, 200, "stream reset");
        }
        until(30, "Calling streams re-exported", || async {
            let ov = operator
                .dp(Method::GET, "/operator/usage", None)
                .await
                .unwrap();
            ov.body["streams"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["producer"] == "cfmeet.calling")
                .all(|s| !s["positionAt"].is_null())
                .then_some(())
        })
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let again = ledger_totals(&gw, session, env).await;
        for t in [
            "CALL_MINUTE_APP",
            "CALL_MINUTE_PUBLIC_NETWORK",
            "RECORDING_MINUTE",
        ] {
            assert_eq!(
                again[t], ledger[t],
                "backfill round {round}: {t} double counted"
            );
        }
    }
    let dp_db = stack.db(stack::DEVELOPER_PLATFORM).await.unwrap();
    let facts_before = db::scalar_i64(
        &dp_db,
        &format!("SELECT count(*) FROM devplat_usage_facts WHERE environment_id = '{env}' AND producer = 'cfmeet.calling'"),
    )
    .await
    .unwrap();
    assert_eq!(facts_before, 3, "one fact per metered Calling record");

    // 5a. Credential states set before the switch.
    let creds = credentials(&gw, session, env, sa).await;
    assert_credentials(&gw, &creds, "before the switch").await;

    // 4. Switch: the Developer Platform serves the published contract.
    switch(&mut stack, true).await;
    let gw = stack.gateway();
    let central_records = all_records(&gw, "/v1/usage", &cred).await;
    let legacy_metered: Vec<Value> = legacy
        .iter()
        .filter(|r| {
            matches!(
                r["usageType"].as_str(),
                Some("CALL_CONNECTED_TIME" | "LEG_CONNECTED_TIME")
            )
        })
        .map(comparable)
        .collect();
    assert_eq!(
        central_records.iter().map(comparable).collect::<Vec<_>>(),
        legacy_metered,
        "central /v1/usage == Calling's records for every metered type"
    );
    let rate = gw.get("/v1/limits", Auth::Credential(&cred)).await;
    status(&rate, 200, "central /v1/limits");
    assert_eq!(
        rate.body["limits"][0]["limit"], "REQUEST_RATE",
        "{}",
        rate.text
    );
    // The preserved service-specific views are Calling's, unchanged.
    assert_eq!(all_records(&gw, "/v1/calling/usage", &cred).await, legacy);
    let preserved = gw.get("/v1/calling/limits", Auth::Credential(&cred)).await;
    status(&preserved, 200, "/v1/calling/limits");
    assert_eq!(
        stable_limits(&preserved.body),
        stable_limits(&legacy_limits.body)
    );
    // 5b. Credential continuity through the switch.
    assert_credentials(&gw, &creds, "after the switch").await;

    // 6. Rollback: Calling serves /v1/usage again; nothing lost, nothing mutated.
    switch(&mut stack, false).await;
    let gw = stack.gateway();
    assert_eq!(
        all_records(&gw, "/v1/usage", &cred).await,
        legacy,
        "rollback restores the legacy view"
    );
    let r = gw.get("/v1/limits", Auth::Credential(&cred)).await;
    status(&r, 200, "legacy /v1/limits after rollback");
    assert_eq!(stable_limits(&r.body), stable_limits(&legacy_limits.body));
    assert_credentials(&gw, &creds, "after the rollback").await;
    let facts_after = db::scalar_i64(
        &dp_db,
        &format!("SELECT count(*) FROM devplat_usage_facts WHERE environment_id = '{env}' AND producer = 'cfmeet.calling'"),
    )
    .await
    .unwrap();
    assert_eq!(
        facts_after, facts_before,
        "no ledger fact lost by the rollback"
    );
    assert_eq!(
        db::scalar_i64(
            &calling_db,
            &format!("SELECT count(*) FROM call_usage_records WHERE environment_id = '{env}'"),
        )
        .await
        .unwrap(),
        calling_records,
        "metering never mutates Calling's authoritative records"
    );
    assert_eq!(
        db::scalar_i64(
            &dp_db,
            &format!(
                "SELECT count(*) FROM devplat_webhook_deliveries WHERE environment_id = '{env}'"
            ),
        )
        .await
        .unwrap(),
        0,
        "metering creates no customer webhook delivery"
    );
    // Expiry is enforced on its own schedule, independent of the switches.
    let wait = (creds.expires_at - chrono::Utc::now()).num_seconds() + 8;
    if wait > 0 {
        tokio::time::sleep(Duration::from_secs(wait as u64)).await;
    }
    assert_credentials(&gw, &creds, "after expiry").await;
    stack.teardown().await;
}

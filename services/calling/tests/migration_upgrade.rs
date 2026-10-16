//! Part K §94: an UPGRADE with data. The Calling migrator is applied up to the previous
//! head (N-1, every migration but the newest — derived from the migrator list, so it stays
//! correct as migrations are added), representative rows are seeded, then the newest
//! migration is applied on top of them. Proven:
//!
//! * the upgrade succeeds over real data and nothing is pending afterwards;
//! * every pre-existing row is intact (per-table row count AND a content fingerprint over
//!   the N-1 columns);
//! * the upgraded schema is IDENTICAL to a clean install at head (tables, columns, types,
//!   nullability, defaults, constraints with their validation state, indexes, triggers,
//!   functions) — so every new constraint / object of the newest migration is present;
//! * every pre-existing table the newest migration changes carried seeded rows (the
//!   upgrade was exercised against data, not against empty tables);
//! * one step down restores the N-1 schema exactly with the data intact, and re-applying
//!   the newest migration restores head, again with the data intact.
//!
//! Seeding: a deployment at head produces realistic rows through the real API and workers
//! (application, public, enterprise, voicemail and webhook activity); those rows are copied
//! into the N-1 database through each table's N-1 row type (`json_populate_recordset`), so
//! only columns that exist at N-1 are written and every N-1 CHECK constraint still applies.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::{Method, StatusCode};
use calling::platform::scopes::names;
use calling_migration::{MigratorTrait, MIGRATION_TABLE};
use common::{
    deployment::{Deployment, Receiver},
    telephony::leg_event,
    TestDb,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

async fn strings(conn: &impl ConnectionTrait, sql: &str) -> Vec<String> {
    conn.query_all(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

async fn tables(conn: &DatabaseConnection) -> Vec<String> {
    strings(
        conn,
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    )
    .await
}

/// The complete public schema as comparable lines, keyed by the table they belong to
/// (`""` for schema-level objects).
async fn schema(conn: &DatabaseConnection) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add = |table: String, line: String| {
        out.entry(table).or_default().insert(line);
    };
    for line in strings(
        conn,
        "SELECT table_name || E'\\t' || 'column ' || column_name || ' ' || data_type || ' ' || \
                COALESCE(character_maximum_length::text, '') || ' null=' || is_nullable || \
                ' default=' || COALESCE(column_default, '') || ' identity=' || is_identity \
           FROM information_schema.columns WHERE table_schema = 'public'",
    )
    .await
    {
        let (t, l) = line.split_once('\t').unwrap();
        add(t.into(), l.into());
    }
    for line in strings(
        conn,
        "SELECT c.relname || E'\\t' || 'constraint ' || k.conname || ' ' || \
                pg_get_constraintdef(k.oid) || ' valid=' || k.convalidated \
           FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid \
           JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public'",
    )
    .await
    {
        let (t, l) = line.split_once('\t').unwrap();
        add(t.into(), l.into());
    }
    for line in strings(
        conn,
        "SELECT tablename || E'\\t' || 'index ' || indexdef FROM pg_indexes WHERE schemaname = 'public'",
    )
    .await
    {
        let (t, l) = line.split_once('\t').unwrap();
        add(t.into(), l.into());
    }
    for line in strings(
        conn,
        "SELECT c.relname || E'\\t' || 'trigger ' || pg_get_triggerdef(t.oid) \
           FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'public' AND NOT t.tgisinternal",
    )
    .await
    {
        let (t, l) = line.split_once('\t').unwrap();
        add(t.into(), l.into());
    }
    for line in strings(
        conn,
        "SELECT 'function ' || p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ') ' || \
                md5(pg_get_functiondef(p.oid)) \
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname = 'public' AND p.prokind = 'f'",
    )
    .await
    {
        add(String::new(), line);
    }
    for line in strings(
        conn,
        "SELECT 'sequence ' || sequencename FROM pg_sequences WHERE schemaname = 'public'",
    )
    .await
    {
        add(String::new(), line);
    }
    for t in tables(conn).await {
        add(t.clone(), "table".into());
    }
    out
}

/// Lines present in `b` and not in `a`, and the tables they belong to.
fn added(
    a: &BTreeMap<String, BTreeSet<String>>,
    b: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (table, lines) in b {
        let before = a.get(table);
        for line in lines {
            if before.is_none_or(|s| !s.contains(line)) {
                out.push((table.clone(), line.clone()));
            }
        }
    }
    out
}

/// The schema with constraint validation state removed (definitions only).
fn normalized(s: &BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, BTreeSet<String>> {
    s.iter()
        .map(|(t, lines)| {
            (
                t.clone(),
                lines
                    .iter()
                    .map(|l| {
                        l.replace(" NOT VALID valid=false", "")
                            .replace(" valid=true", "")
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Constraints valid in `strict` that are present but NOT VALID in `relaxed`.
fn relaxed_constraints(
    strict: &BTreeMap<String, BTreeSet<String>>,
    relaxed: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (table, lines) in relaxed {
        for line in lines {
            let Some(rest) = line.strip_prefix("constraint ") else {
                continue;
            };
            let Some(def) = line.strip_suffix(" NOT VALID valid=false") else {
                continue;
            };
            if strict
                .get(table)
                .is_some_and(|s| s.contains(&format!("{def} valid=true")))
            {
                out.push((
                    table.clone(),
                    rest.split_whitespace().next().unwrap().to_string(),
                ));
            }
        }
    }
    out
}

/// Columns of each table (in table order) at the current schema.
async fn columns(conn: &DatabaseConnection) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in strings(
        conn,
        "SELECT table_name || E'\\t' || column_name FROM information_schema.columns \
          WHERE table_schema = 'public' ORDER BY table_name, ordinal_position",
    )
    .await
    {
        let (t, c) = line.split_once('\t').unwrap();
        out.entry(t.into()).or_default().push(c.into());
    }
    out
}

/// Per-table (row count, content fingerprint over `cols`) for every table in `cols`.
async fn fingerprint(
    conn: &DatabaseConnection,
    cols: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, (i64, String)> {
    let mut out = BTreeMap::new();
    for (table, columns) in cols {
        if table == MIGRATION_TABLE {
            continue;
        }
        let row = columns
            .iter()
            .map(|c| format!("t.\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let line = strings(
            conn,
            &format!(
                "SELECT count(*)::text || ':' || md5(COALESCE(string_agg(x, E'\\n' ORDER BY x), '')) \
                   FROM (SELECT ROW({row})::text AS x FROM \"{table}\" t) s"
            ),
        )
        .await
        .pop()
        .unwrap();
        let (n, hash) = line.split_once(':').unwrap();
        out.insert(table.clone(), (n.parse().unwrap(), hash.to_string()));
    }
    out
}

/// The donor rows of `table` as a JSON array. Credentials Platform Access issued carry no
/// verifier in Calling (000044); a Part III row always had one, so the N-1 copy gets a
/// stand-in verifier (never a real one: these credentials authenticate at Platform
/// Access only).
fn donor_rows(table: &str) -> String {
    if table == "platform_api_credentials" {
        return "SELECT COALESCE(json_agg(to_jsonb(t) || CASE WHEN t.verifier IS NULL THEN \
                  jsonb_build_object('verifier', '\\x' || encode(sha256(t.id::text::bytea), 'hex'), \
                    'verifier_algorithm', 'HMAC_SHA256', 'verifier_key_version', 1) \
                  ELSE '{}'::jsonb END), '[]')::text FROM platform_api_credentials t"
            .to_string();
    }
    format!("SELECT COALESCE(json_agg(t), '[]')::text FROM \"{table}\" t")
}

/// Copies every row of `donor` (head) into `target` (N-1) through each target table's own
/// row type (replacing rows N-1's own migrations seeded); foreign keys and guard triggers
/// are suspended for the copy only (replica role), CHECK / NOT NULL / unique constraints
/// of N-1 apply.
async fn copy_rows(donor: &DatabaseConnection, target: &DatabaseConnection) -> usize {
    let mut copied = 0;
    let txn = target.begin().await.unwrap();
    txn.execute_unprepared("SET LOCAL session_replication_role = replica")
        .await
        .unwrap();
    for table in tables(target).await {
        if table == MIGRATION_TABLE {
            continue;
        }
        let rows = strings(donor, &donor_rows(&table)).await.pop().unwrap();
        // Rows a migration seeded itself (singletons such as the projection cursor) are
        // replaced by head's copy (guard triggers are suspended by the replica role).
        txn.execute_unprepared(&format!("DELETE FROM \"{table}\""))
            .await
            .unwrap();
        let result = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "INSERT INTO \"{table}\" OVERRIDING SYSTEM VALUE \
                     SELECT * FROM json_populate_recordset(NULL::\"{table}\", $1::json)"
                ),
                vec![rows.into()],
            ))
            .await
            .unwrap_or_else(|e| {
                panic!("seeding {table} at the previous head failed (the newest migration changed a NOT NULL / CHECK shape the seed cannot express): {e}")
            });
        copied += result.rows_affected() as usize;
    }
    // Serial / identity sequences continue after the copied rows.
    for line in strings(
        &txn,
        "SELECT table_name || E'\\t' || column_name FROM information_schema.columns \
          WHERE table_schema = 'public' AND (column_default LIKE 'nextval(%' OR is_identity = 'YES')",
    )
    .await
    {
        let (t, c) = line.split_once('\t').unwrap();
        txn.execute_unprepared(&format!(
            "SELECT setval(pg_get_serial_sequence('\"{t}\"', '{c}'), \
                    GREATEST(COALESCE((SELECT max(\"{c}\") FROM \"{t}\"), 0), 1))"
        ))
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();
    copied
}

/// Realistic activity at head through the real API and workers.
async fn seed(d: &Deployment) {
    let a = d.a();
    let h = d.h();
    d.enable_artifacts().await;
    // Operator-audited administration: identity changes are audited by Platform Access
    // (projected here); Calling audits its own (limits).
    h.platform
        .set_live_telecom_allowed(a.w.consumer.env(a.w.kind).id, true)
        .await
        .unwrap();
    h.platform
        .operator_set_tenant_limits(
            &calling::platform::operator::OperatorContext::new("ops.upgrade-cert").unwrap(),
            a.w.consumer.tenant.id,
            calling::platform::limits::LimitValues {
                requests_per_minute: Some(1000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Webhooks: a subscription, published events, delivered attempts.
    let rx = Receiver::start().await;
    let manager = d
        .credential(&[
            names::WEBHOOKS_READ,
            names::WEBHOOKS_MANAGE,
            names::CALLS_READ,
        ])
        .await;
    let r = d
        .e
        .send(
            manager.secret.expose_once(),
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": rx.url, "eventTypes": ["call.ringing", "call.ended"], "description": "upgrade" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // A connected, then ended application call (sessions, intervals, provider events,
    // usage facts) and a ringing one.
    let (call, _, _) = a.connected_call("VIDEO").await;
    let (_, v) = a.state(call).await;
    let r = a.command(call, "end", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    a.ringing_call("VOICE").await;
    // A public call: originate, Telephony events, usage settlement.
    let r = d.call_contact("upgrade-public-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let public: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let (leg, attempt) = d.public_leg(public).await;
    let t = d.x.telephony.telephony_leg_of(leg).expect("originated");
    let apply = |v: Value| async move {
        let event: calling::telephony::events::TelephonyEvent = serde_json::from_value(v).unwrap();
        h.state.app.apply_telephony_event(&event).await.unwrap();
    };
    apply(leg_event("LEG_RINGING", public, leg, t, Some(attempt), 1)).await;
    let mut ended = leg_event("LEG_NO_ANSWER", public, leg, t, Some(attempt), 2);
    ended["endReason"] = json!("NO_ANSWER");
    ended["legStatus"] = json!("FAILED");
    ended["definite"] = json!(true);
    apply(ended).await;
    let mut usage = leg_event("USAGE_FINALIZED", public, leg, t, Some(attempt), 3);
    usage["usage"] = json!({ "connectedDurationMs": 0, "billableSeconds": 0, "costMinor": 0,
                             "costKnown": true, "currency": "NGN", "destinationCountry": "NG" });
    apply(usage).await;
    // Enterprise: configuration, an internal call, a READY voicemail.
    let caller = d.desk("upgrade-caller").await;
    let owner = d.desk("upgrade-owner").await;
    d.e.department("Upgrade desk").await;
    let vm = d
        .credential(&[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ])
        .await;
    d.voicemail(vm.secret.expose_once(), &owner.0, &caller, "4400")
        .await;
    // Blocks and abuse reports.
    let r = a
        .post(
            "/v1/blocks",
            json!({ "principalId": a.w.initiator.id(), "blockedPrincipalId": caller.0.id(),
                    "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = a
        .post(
            &format!("/v1/calls/{call}/abuse-reports"),
            json!({ "principalId": a.w.initiator.id(), "category": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Workers: publication, deliveries (attempts), expiry.
    let transport =
        calling::webhooks::worker::LocalWebhookTransport::new((*h.state.webhooks).clone());
    for _ in 0..3 {
        h.state
            .webhooks
            .publish_outbox_once(&transport, 1000)
            .await
            .unwrap();
        h.state.webhooks.deliver_due_once(100).await.unwrap();
    }
    assert!(!rx.seen().is_empty(), "webhooks were delivered");
    // An operational incident (the enterprise router's evidence class).
    let s = a.w.scope();
    common::exec(
        h.conn(),
        &format!(
            "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, dedupe_key, \
               category, severity, status, call_id, occurrences, restricted_evidence, first_seen_at, last_seen_at) \
             VALUES ('{}', '{}', '{}', '{}', 'forwarding_loop_repeated:upgrade', 'FORWARDING_LOOP_REPEATED', \
               'LOW', 'OPEN', '{call}', 1, '{{}}', now(), now())",
            Uuid::new_v4(),
            s.tenant_id(),
            s.application_id(),
            s.environment_id()
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn the_newest_migration_upgrades_seeded_data_and_rolls_back_one_step() {
    let Some(d) = Deployment::new().await else {
        return;
    };
    seed(&d).await;
    let donor = d.h().conn();
    let head_schema = schema(donor).await;

    let all: Vec<String> = <calling_migration::Migrator as MigratorTrait>::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let n = all.len();
    assert!(n >= 2);
    let newest = all[n - 1].clone();
    println!("previous head: {}, newest: {newest}", all[n - 2]);

    let target = TestDb::empty().await.unwrap();
    let conn = &target.conn;
    calling_migration::Migrator::up(conn, Some((n - 1) as u32))
        .await
        .unwrap();
    let pending: Vec<String> = calling_migration::Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    assert_eq!(
        pending,
        vec![newest.clone()],
        "exactly the newest is pending"
    );
    let previous_schema = schema(conn).await;
    let previous_columns = columns(conn).await;

    // What the newest migration adds or changes, and the pre-existing tables it touches.
    let delta = added(&previous_schema, &head_schema);
    assert!(
        !delta.is_empty(),
        "{newest} changes nothing observable in the schema"
    );
    let touched: BTreeSet<String> = delta
        .iter()
        .map(|(t, _)| t.clone())
        .filter(|t| previous_columns.contains_key(t))
        .collect();
    println!(
        "{newest}: {} new schema lines; pre-existing tables touched: {touched:?}",
        delta.len()
    );

    // Seed N-1 with the deployment's rows.
    let copied = copy_rows(donor, conn).await;
    assert!(copied > 100, "representative rows were seeded ({copied})");
    let seeded = fingerprint(conn, &previous_columns).await;
    let empty_touched: Vec<&String> = touched.iter().filter(|t| seeded[*t].0 == 0).collect();
    assert!(
        empty_touched.is_empty(),
        "seed rows for every pre-existing table {newest} changes: {empty_touched:?}"
    );

    // Upgrade to head over the data.
    calling_migration::Migrator::up(conn, None).await.unwrap();
    assert!(calling_migration::Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());
    let upgraded = schema(conn).await;
    assert_eq!(
        added(&head_schema, &upgraded),
        vec![],
        "upgraded schema has objects a clean install does not"
    );
    assert_eq!(
        added(&upgraded, &head_schema),
        vec![],
        "upgraded schema lacks objects of a clean install (new constraints / objects of {newest})"
    );
    assert_eq!(
        fingerprint(conn, &previous_columns).await,
        seeded,
        "pre-existing data changed by the upgrade"
    );
    // New CHECK constraints hold for the seeded rows: re-validate each one that a clean
    // install validates.
    for (table, line) in &delta {
        if let Some(rest) = line.strip_prefix("constraint ") {
            if line.ends_with("valid=true") && rest.contains("CHECK") {
                let name = rest.split_whitespace().next().unwrap();
                let ok = strings(
                    conn,
                    &format!(
                        "SELECT count(*)::text FROM \"{table}\" t WHERE NOT ({})",
                        rest.split_once("CHECK ")
                            .unwrap()
                            .1
                            .rsplit_once(" valid=")
                            .unwrap()
                            .0
                    ),
                )
                .await;
                assert_eq!(ok, vec!["0".to_string()], "{table}.{name} violated");
            }
        }
    }

    // One step down: N-1 again, data intact.
    calling_migration::Migrator::down(conn, Some(1))
        .await
        .unwrap();
    assert_eq!(
        calling_migration::Migrator::get_pending_migrations(conn)
            .await
            .unwrap()
            .iter()
            .map(|m| m.name().to_string())
            .collect::<Vec<_>>(),
        vec![newest.clone()]
    );
    let down_schema = schema(conn).await;
    // A rollback may only RELAX validation of a restored constraint (append-only audit
    // rows written under the newer vocabulary must not make the rollback fail — documented
    // by the migration); its definition is the N-1 one, and the seeded rows still satisfy
    // it (VALIDATE succeeds, inside a transaction that is rolled back).
    let relaxed = relaxed_constraints(&previous_schema, &down_schema);
    for (table, name) in &relaxed {
        let txn = conn.begin().await.unwrap();
        txn.execute_unprepared(&format!(
            "ALTER TABLE \"{table}\" VALIDATE CONSTRAINT \"{name}\""
        ))
        .await
        .unwrap_or_else(|e| {
            panic!("{table}.{name}: seeded rows violate the restored constraint: {e}")
        });
        txn.rollback().await.unwrap();
    }
    println!("rollback restored NOT VALID: {relaxed:?}");
    assert_eq!(
        added(&normalized(&previous_schema), &normalized(&down_schema)),
        vec![]
    );
    assert_eq!(
        added(&normalized(&down_schema), &normalized(&previous_schema)),
        vec![]
    );
    assert_eq!(
        fingerprint(conn, &previous_columns).await,
        seeded,
        "data changed by the rollback"
    );

    // And up again.
    calling_migration::Migrator::up(conn, None).await.unwrap();
    let again = schema(conn).await;
    assert_eq!(added(&head_schema, &again), vec![]);
    assert_eq!(added(&again, &head_schema), vec![]);
    assert_eq!(
        fingerprint(conn, &previous_columns).await,
        seeded,
        "data changed by the re-upgrade"
    );
}

//! Part VI §19.1 — schema upgrade from a representative pre-Part-VI database.
//!
//! Six existing services gained Part VI migrations (Calling, the Meeting Service,
//! Messaging, Platform Access, Privacy Relay, the SMS Gateway). For each one, with the
//! service's OWN migration binary:
//!
//! 1. migrate a fresh database to the last pre-Part-VI migration (the deployed `main`
//!    schema) and check the Part VI migrations are the pending ones, in order;
//! 2. seed representative rows into every table a Part VI migration alters (already
//!    published and still pending outbox events, a CF Meet-owned meeting with its
//!    invitation, a tenant → … → credential chain with audit history);
//! 3. upgrade: every pre-existing column of every seeded row is byte-for-byte unchanged
//!    (a digest over the pre-Part-VI columns), the new columns start empty, an already
//!    published / relayed event is not made eligible again (no historical re-delivery);
//! 4. re-run `up` (no-op) — the migrations are safely re-runnable;
//! 5. roll the Part VI migrations back while no Part VI data exists (pre-Part-VI data
//!    intact) and re-apply them;
//! 6. once Part VI data exists, the guarded down migrations refuse and keep it.
//!
//! No service talks to another service's database; every step is the service's own CLI.
//! Prerequisites: TEST_DATABASE_URL and the migration binaries (CFMEET_BIN_DIR, built by
//! `make certification-bins`); without them the test FAILS unless CFMEET_SKIP_DB_TESTS=1.

use std::collections::BTreeMap;

use cfmeet_certification::{db, StackConfig};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

struct Case {
    service: &'static str,
    bin: &'static str,
    /// The Part VI migrations, oldest first (the newest of the service).
    part_vi: &'static [&'static str],
    /// Rows written by the pre-Part-VI schema (FK triggers disabled: parents elsewhere).
    seed: &'static str,
    /// Tables whose pre-Part-VI columns must survive unchanged.
    tables: &'static [&'static str],
    /// Boolean SQL checks after the upgrade.
    after_upgrade: &'static [(&'static str, &'static str)],
    /// Part VI data that makes the guarded down refuse (`None`: the down is a pure
    /// column drop and always allowed).
    part_vi_data: Option<&'static str>,
}

const ENVELOPE: &str = r#"jsonb_build_object('event_id', e, 'event_type', 'call.ended', 'schema_version', 1,
    'producer_service', 'cfmeet.calling', 'payload', jsonb_build_object('callId', gen_random_uuid()))"#;

fn cases() -> Vec<Case> {
    vec![
        Case {
            service: "calling",
            bin: "calling-migration",
            part_vi: &["m20261201_000047_public_event_destinations"],
            seed: Box::leak(
                format!(
                    "INSERT INTO calling_outbox_events (id, event_id, source_event_id, aggregate_type, aggregate_id, \
                       tenant_id, application_id, environment_id, event_type, schema_version, envelope, status, \
                       attempt_count, next_attempt_at, published_at, created_at, updated_at) \
                     SELECT gen_random_uuid(), e, gen_random_uuid(), 'CALL', gen_random_uuid(), gen_random_uuid(), \
                       gen_random_uuid(), gen_random_uuid(), 'call.ended', 1, {ENVELOPE}, s, 1, now(), \
                       CASE WHEN s = 'PUBLISHED' THEN now() END, now() - interval '3 days', now() \
                     FROM (VALUES ('evt_' || md5('pub')::text, 'PUBLISHED'), ('evt_' || md5('pend')::text, 'PENDING')) v(e, s)"
                )
                .into_boxed_str(),
            ),
            tables: &["calling_outbox_events"],
            after_upgrade: &[
                (
                    "new outcome columns start empty",
                    "SELECT bool_and(local_outcome IS NULL AND developer_platform_outcome IS NULL) FROM calling_outbox_events",
                ),
                (
                    "a published event stays published (never re-sent)",
                    "SELECT count(*) = 1 FROM calling_outbox_events WHERE status = 'PUBLISHED'",
                ),
            ],
            part_vi_data: None,
        },
        Case {
            service: "messaging",
            bin: "messaging-migration",
            // + Blueprint Section 13 (session devices, inbox summary): additive columns,
            // new tables; its down only drops what it added.
            part_vi: &[
                "m20261201_000029_public_event_destinations",
                "m20261202_000030_session_devices_inbox",
            ],
            seed: "INSERT INTO messaging_webhook_relay (event_id, environment_id, status, attempts, next_attempt_at, \
                     created_at, relayed_at) VALUES \
                   (gen_random_uuid(), gen_random_uuid(), 'RELAYED', 1, now(), now() - interval '2 days', now() - interval '2 days'), \
                   (gen_random_uuid(), gen_random_uuid(), 'PENDING', 0, now(), now(), NULL)",
            tables: &["messaging_webhook_relay"],
            after_upgrade: &[
                (
                    "new outcome columns start empty",
                    "SELECT bool_and(calling_outcome IS NULL AND developer_platform_outcome IS NULL) FROM messaging_webhook_relay",
                ),
                (
                    "a relayed event stays relayed",
                    "SELECT count(*) = 1 FROM messaging_webhook_relay WHERE status = 'RELAYED'",
                ),
            ],
            part_vi_data: None,
        },
        Case {
            service: "privacy-relay",
            bin: "privacy-relay-migration",
            part_vi: &["m20261101_000017_public_event_destinations"],
            seed: "INSERT INTO privacy_relay_outbox (id, event_id, source_audit_event_id, aggregate_type, aggregate_id, \
                     tenant_id, application_id, environment_id, event_type, schema_version, envelope, status, \
                     attempt_count, next_attempt_at, published_at, created_at, updated_at) \
                   SELECT gen_random_uuid(), e, gen_random_uuid(), 'RELAY_SESSION', gen_random_uuid(), gen_random_uuid(), \
                     gen_random_uuid(), gen_random_uuid(), 'privacy-relay.session.closed', 1, \
                     jsonb_build_object('event_id', e, 'event_type', 'privacy-relay.session.closed'), s, 1, now(), \
                     CASE WHEN s = 'PUBLISHED' THEN now() END, now() - interval '1 day', now() \
                   FROM (VALUES ('evt_' || md5('relay-pub')::text, 'PUBLISHED'), ('evt_' || md5('relay-pend')::text, 'PENDING')) v(e, s)",
            tables: &["privacy_relay_outbox"],
            after_upgrade: &[
                (
                    "new outcome columns start empty",
                    "SELECT bool_and(calling_outcome IS NULL AND developer_platform_outcome IS NULL) FROM privacy_relay_outbox",
                ),
                (
                    "a published event stays published",
                    "SELECT count(*) = 1 FROM privacy_relay_outbox WHERE status = 'PUBLISHED'",
                ),
            ],
            part_vi_data: None,
        },
        Case {
            service: "sms-gateway",
            bin: "sms-gateway-migration",
            part_vi: &["m20261101_000004_public_event_destinations"],
            seed: "INSERT INTO sms_outbox_events (id, event_type, schema_version, aggregate_type, aggregate_id, tenant_id, \
                     application_id, environment_id, correlation_id, payload, occurred_at, status, attempts, \
                     next_attempt_at, published_at) VALUES \
                   (gen_random_uuid(), 'sms-gateway.message.delivered', 1, 'SMS_MESSAGE', gen_random_uuid(), \
                     gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'corr-upgrade-1', '{\"segments\": 1}', \
                     now() - interval '1 day', 'PUBLISHED', 1, now(), now()), \
                   (gen_random_uuid(), 'sms-gateway.message.failed', 1, 'SMS_MESSAGE', gen_random_uuid(), \
                     gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'corr-upgrade-2', '{\"segments\": 2}', \
                     now(), 'PENDING', 0, now(), NULL)",
            tables: &["sms_outbox_events"],
            after_upgrade: &[
                (
                    "new outcome columns start empty",
                    "SELECT bool_and(calling_outcome IS NULL AND developer_platform_outcome IS NULL) FROM sms_outbox_events",
                ),
                (
                    "a published event stays published",
                    "SELECT count(*) = 1 FROM sms_outbox_events WHERE status = 'PUBLISHED'",
                ),
            ],
            part_vi_data: None,
        },
        Case {
            service: "meeting-service",
            bin: "migration",
            part_vi: &["m20261101_000001_platform_environment_meetings"],
            seed: "INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES \
                     ('0d9a8f6e-0000-4000-8000-000000000001', gen_random_uuid(), 'upg-abc-def', 'Weekly sync'); \
                   INSERT INTO meeting_invitations (id, meeting_space_id, inviter_user_id, invitee_email, token_hash, expires_at) \
                     VALUES (gen_random_uuid(), '0d9a8f6e-0000-4000-8000-000000000001', gen_random_uuid(), \
                       'guest@example.com', md5('invite-token'), now() + interval '7 days'); \
                   INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, status, published_at, channel) VALUES \
                     (gen_random_uuid(), 'MEETING', '0d9a8f6e-0000-4000-8000-000000000001', 'meeting.updated', 'PUBLISHED', now(), 'REALTIME'), \
                     (gen_random_uuid(), 'MEETING', '0d9a8f6e-0000-4000-8000-000000000001', 'meeting.updated', 'PENDING', NULL, 'PROVIDER_COMMAND')",
            tables: &["meeting_spaces", "meeting_invitations", "outbox_events"],
            after_upgrade: &[
                (
                    "a CF Meet-owned meeting keeps its owner and has no environment scope",
                    "SELECT owner_user_id IS NOT NULL AND platform_environment_id IS NULL FROM meeting_spaces",
                ),
                (
                    "existing channels stay valid",
                    "SELECT count(*) = 2 FROM outbox_events WHERE channel IN ('REALTIME', 'PROVIDER_COMMAND')",
                ),
            ],
            part_vi_data: Some(
                "INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, platform_tenant_id, \
                   platform_application_id, platform_environment_id, created_by_service_account_id) \
                 VALUES (gen_random_uuid(), NULL, 'upg-env-own', 'Environment meeting', gen_random_uuid(), \
                   gen_random_uuid(), gen_random_uuid(), gen_random_uuid())",
            ),
        },
        Case {
            service: "platform-access",
            bin: "platform-access-migration",
            // Part VI §54 / §55, then Blueprint Section 7 (ADR 0058) and Section 15
            // (credential last use, TENANT_RENAMED): all newer than the deployed schema and
            // upgraded / rolled back together.
            part_vi: &[
                "m20261120_000004_credential_suspension_production_activation",
                "m20261121_000005_first_party_organization_scopes",
                "m20261122_000006_credential_usage_tenant_rename",
            ],
            seed: "INSERT INTO platform_tenants (id, slug, display_name, classification) VALUES \
                     ('7e000000-0000-4000-8000-000000000001', 'acme-upgrade', 'Acme', 'EXTERNAL'); \
                   INSERT INTO platform_applications (id, tenant_id, slug, display_name) VALUES \
                     ('7e000000-0000-4000-8000-000000000002', '7e000000-0000-4000-8000-000000000001', 'backend', 'Backend'); \
                   INSERT INTO platform_environments (id, tenant_id, application_id, kind) VALUES \
                     ('7e000000-0000-4000-8000-000000000003', '7e000000-0000-4000-8000-000000000001', \
                      '7e000000-0000-4000-8000-000000000002', 'PRODUCTION'); \
                   INSERT INTO platform_service_accounts (id, tenant_id, application_id, environment_id, name) VALUES \
                     ('7e000000-0000-4000-8000-000000000004', '7e000000-0000-4000-8000-000000000001', \
                      '7e000000-0000-4000-8000-000000000002', '7e000000-0000-4000-8000-000000000003', 'worker'); \
                   INSERT INTO platform_api_credentials (id, public_id, tenant_id, application_id, environment_id, \
                     service_account_id, verifier, verifier_algorithm, verifier_key_version, last_four, status, \
                     not_before, expires_at, revoked_at) VALUES \
                     ('7e000000-0000-4000-8000-000000000005', substr(md5('active'), 1, 24), '7e000000-0000-4000-8000-000000000001', \
                      '7e000000-0000-4000-8000-000000000002', '7e000000-0000-4000-8000-000000000003', \
                      '7e000000-0000-4000-8000-000000000004', decode(md5('v1') || md5('v2'), 'hex'), 'HMAC_SHA256', 1, 'a1b2', \
                      'ACTIVE', now() - interval '1 day', now() + interval '90 days', NULL), \
                     ('7e000000-0000-4000-8000-000000000006', substr(md5('revoked'), 1, 24), '7e000000-0000-4000-8000-000000000001', \
                      '7e000000-0000-4000-8000-000000000002', '7e000000-0000-4000-8000-000000000003', \
                      '7e000000-0000-4000-8000-000000000004', decode(md5('v3') || md5('v4'), 'hex'), 'HMAC_SHA256', 1, 'c3d4', \
                      'REVOKED', now() - interval '2 days', now() + interval '90 days', now() - interval '1 day'); \
                   INSERT INTO platform_access_audit (id, actor, correlation_id, action, target_kind, target_id, tenant_id) VALUES \
                     (gen_random_uuid(), 'operator@cfmeet', 'corr-upgrade-pa', 'CREDENTIAL_REVOKED', 'CREDENTIAL', \
                      '7e000000-0000-4000-8000-000000000006', '7e000000-0000-4000-8000-000000000001')",
            tables: &[
                "platform_tenants",
                "platform_applications",
                "platform_environments",
                "platform_service_accounts",
                "platform_api_credentials",
                "platform_access_audit",
            ],
            after_upgrade: &[
                (
                    "no credential is suspended by the upgrade",
                    "SELECT count(*) = 0 FROM platform_credential_suspensions",
                ),
                (
                    "no environment is activated by the upgrade",
                    "SELECT count(*) = 0 FROM platform_production_activations",
                ),
                (
                    "no organization scope is created by the upgrade",
                    "SELECT count(*) = 0 FROM platform_organization_scopes",
                ),
                (
                    "no credential use is invented by the upgrade",
                    "SELECT count(*) = 0 FROM platform_credential_usage",
                ),
                (
                    "credential verifiers, status and validity are untouched",
                    "SELECT count(*) = 1 FROM platform_api_credentials WHERE status = 'ACTIVE' \
                       AND verifier = decode(md5('v1') || md5('v2'), 'hex') AND expires_at > now()",
                ),
            ],
            // Data of the newest migration first refuses its own down (so nothing is rolled
            // back), and the Part VI audit row would refuse 000004's.
            part_vi_data: Some(
                "INSERT INTO platform_access_audit (id, actor, correlation_id, action, target_kind, target_id, tenant_id) VALUES \
                   (gen_random_uuid(), 'operator@cfmeet', 'corr-upgrade-sus', 'CREDENTIAL_SUSPENDED', 'CREDENTIAL', \
                    '7e000000-0000-4000-8000-000000000005', '7e000000-0000-4000-8000-000000000001'); \
                 INSERT INTO platform_organization_scopes (organization_ref, capability, tenant_id, application_id, \
                   environment_id, requested_by, created_at) VALUES \
                   ('7e000000-0000-4000-8000-0000000000aa', 'primary', '7e000000-0000-4000-8000-000000000001', \
                    '7e000000-0000-4000-8000-000000000002', '7e000000-0000-4000-8000-000000000003', \
                    'cfmeet.organizations', now()); \
                 INSERT INTO platform_access_audit (id, actor, correlation_id, action, target_kind, target_id, tenant_id) VALUES \
                   (gen_random_uuid(), 'devplat.u-0123456789abcdef0123456789abcdef', 'corr-upgrade-ren', 'TENANT_RENAMED', \
                    'TENANT', '7e000000-0000-4000-8000-000000000001', '7e000000-0000-4000-8000-000000000001')",
            ),
        },
    ]
}

/// Migration names of `status` output with their state (`Applied` / `Pending`), in order.
fn status(bin: &std::path::Path, url: &str) -> Vec<(String, String)> {
    db::migration(bin, url, &["status"])
        .expect("migration status")
        .lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Migration '")?;
            let (name, state) = rest.split_once("'... ")?;
            Some((name.to_string(), state.trim().to_string()))
        })
        .collect()
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> anyhow::Result<()> {
    conn.execute_unprepared(sql).await?;
    Ok(())
}

async fn boolean(conn: &DatabaseConnection, sql: &str) -> bool {
    conn.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap_or_else(|e| panic!("{sql}: {e}"))
    .expect("a row")
    .try_get_by_index::<Option<bool>>(0)
    .unwrap()
    .unwrap_or(false)
}

async fn text(conn: &DatabaseConnection, sql: &str) -> String {
    conn.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap_or_else(|e| panic!("{sql}: {e}"))
    .expect("a row")
    .try_get_by_index::<Option<String>>(0)
    .unwrap()
    .unwrap_or_default()
}

/// `(row count, digest)` of `table` over `columns` (every row, canonical order).
async fn digest(conn: &DatabaseConnection, table: &str, columns: &[String]) -> String {
    let cols = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    text(
        conn,
        &format!(
            "SELECT count(*)::text || ':' || coalesce(md5(string_agg(r::text, '|' ORDER BY r::text)), '') \
             FROM (SELECT {cols} FROM {table}) r"
        ),
    )
    .await
}

async fn columns(conn: &DatabaseConnection, table: &str) -> Vec<String> {
    let rows = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT column_name::text FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = '{table}' ORDER BY ordinal_position"
            ),
        ))
        .await
        .unwrap();
    rows.iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

async fn digests(
    conn: &DatabaseConnection,
    pre: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (t, cols) in pre {
        out.insert(t.clone(), digest(conn, t, cols).await);
    }
    out
}

#[tokio::test]
async fn part_vi_migrations_upgrade_a_pre_part_vi_schema_without_data_loss() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    for case in cases() {
        let bin = config.bin_dir.join(case.bin);
        let name = format!(
            "cert_upgrade_{}_{}",
            case.service.replace('-', "_"),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let url = db::create(&config.admin_url, &name).await.unwrap();
        let result = run_case(&case, &bin, &url).await;
        db::drop_all(&config.admin_url, std::slice::from_ref(&name)).await;
        result.unwrap_or_else(|e| panic!("{}: {e:#}", case.service));
        eprintln!(
            "{}: upgrade, re-run, rollback and guarded down OK",
            case.service
        );
    }
}

async fn run_case(case: &Case, bin: &std::path::Path, url: &str) -> anyhow::Result<()> {
    let all = status(bin, url);
    let total = all.len();
    let k = case.part_vi.len();
    anyhow::ensure!(total > k, "{} lists {total} migrations", case.service);
    // The Part VI migrations are the newest ones, in order.
    let newest: Vec<&str> = all[total - k..].iter().map(|(n, _)| n.as_str()).collect();
    anyhow::ensure!(
        newest == case.part_vi,
        "{}: newest migrations {newest:?}, expected {:?} (a later migration needs this test extended)",
        case.service,
        case.part_vi
    );
    // 1. The deployed (pre-Part-VI) schema.
    db::migration(bin, url, &["up", "-n", &(total - k).to_string()])?;
    let st = status(bin, url);
    anyhow::ensure!(
        st[..total - k].iter().all(|(_, s)| s == "Applied")
            && st[total - k..].iter().all(|(_, s)| s == "Pending"),
        "pre-Part-VI status {st:?}"
    );
    let conn = db::connect(url).await?;
    // 2. Representative data written by the pre-Part-VI schema.
    exec(
        &conn,
        &format!(
            "BEGIN; SET LOCAL session_replication_role = replica; {}; COMMIT;",
            case.seed
        ),
    )
    .await?;
    let mut pre = BTreeMap::new();
    for t in case.tables {
        pre.insert(t.to_string(), columns(&conn, t).await);
    }
    let before = digests(&conn, &pre).await;
    for (t, d) in &before {
        anyhow::ensure!(!d.starts_with("0:"), "{t} was not seeded");
    }
    // 3. Upgrade.
    db::migration(bin, url, &["up"])?;
    anyhow::ensure!(
        status(bin, url).iter().all(|(_, s)| s == "Applied"),
        "everything applied"
    );
    anyhow::ensure!(
        digests(&conn, &pre).await == before,
        "pre-Part-VI data changed by the upgrade"
    );
    for (what, sql) in case.after_upgrade {
        anyhow::ensure!(boolean(&conn, sql).await, "after upgrade: {what}");
    }
    // 4. Re-running is a no-op.
    db::migration(bin, url, &["up"])?;
    anyhow::ensure!(digests(&conn, &pre).await == before, "re-run changed data");
    // 5. Rollback of the Part VI migrations (no Part VI data yet), then re-apply.
    db::migration(bin, url, &["down", "-n", &k.to_string()])?;
    anyhow::ensure!(
        status(bin, url)[total - k..]
            .iter()
            .all(|(_, s)| s == "Pending"),
        "rolled back"
    );
    anyhow::ensure!(
        digests(&conn, &pre).await == before,
        "rollback lost pre-Part-VI data"
    );
    db::migration(bin, url, &["up"])?;
    anyhow::ensure!(
        digests(&conn, &pre).await == before,
        "re-upgrade changed data"
    );
    // 6. Part VI data makes the guarded down refuse and stay intact.
    if let Some(sql) = case.part_vi_data {
        exec(
            &conn,
            &format!("BEGIN; SET LOCAL session_replication_role = replica; {sql}; COMMIT;"),
        )
        .await?;
        let with_part_vi = digests(&conn, &pre).await;
        let refused = db::migration(bin, url, &["down", "-n", &k.to_string()]);
        anyhow::ensure!(
            refused.is_err(),
            "down must refuse once Part VI data exists"
        );
        anyhow::ensure!(
            status(bin, url).iter().all(|(_, s)| s == "Applied"),
            "a refused down leaves the schema applied"
        );
        anyhow::ensure!(
            digests(&conn, &pre).await == with_part_vi,
            "a refused down changed data"
        );
    }
    conn.close().await.ok();
    Ok(())
}

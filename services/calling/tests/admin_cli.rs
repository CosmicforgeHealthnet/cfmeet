//! Operational CLI (`platform_admin`) end to end. Identity administration moved to
//! Platform Access (`platform-access-admin`, ADR 0032; its CLI tests prove the credential
//! lifecycle there): Calling's CLI reads the identity PROJECTION (masked, never a secret)
//! and administers what Calling owns — limits, routing / spend policy, compliance,
//! abuse, retention, incidents — auditing every mutation (Part H §72.8): an operator is
//! required, and exactly one append-only `platform_operator_audit` row is written with the
//! change.

mod common;

use std::{process::Command, sync::Arc};

use calling::domain::{ManualClock, TenantClassification};
use chrono::Utc;
use common::{secret_part, Harness, TestDb};
use serde_json::Value;

struct Cli {
    database_url: String,
    /// Service authentication towards Platform Access (only some commands reach it).
    env: Vec<(String, String)>,
}

/// The operator every mutating command in these tests acts as.
const OPERATOR: &str = "ops.alice";

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Output {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}): {}\nstderr: {}",
                self.stdout, self.stderr
            )
        })
    }
}

impl Cli {
    /// The CLI of a Calling deployment over `db`, able to reach its Platform Access.
    fn of(db: &TestDb) -> Self {
        Self {
            database_url: db.url.clone(),
            env: db.pa().calling_process_env(),
        }
    }

    /// Runs as [`OPERATOR`] (`--operator`).
    fn run(&self, args: &[&str]) -> Output {
        let mut all = vec!["--operator", OPERATOR];
        all.extend_from_slice(args);
        self.run_raw(&all, &[])
    }

    /// Runs exactly `args` with extra environment variables (no implicit operator).
    fn run_raw(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_platform_admin"))
            .args(args)
            .current_dir(std::env::temp_dir())
            .env_clear()
            .env("DATABASE_URL", &self.database_url)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .env("RUST_LOG", "info")
            .envs(env.iter().copied())
            .output()
            .expect("run platform_admin");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert_eq!(out.code, 0, "{args:?} failed: {}", out.stderr);
        out
    }
}

fn with<'a>(base: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
    base.iter().chain(extra.iter()).copied().collect()
}

#[tokio::test]
async fn cli_reads_the_identity_projection_and_manages_calling_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = Harness::on(Arc::new(db), ManualClock::new(Utc::now()));
    let c = h
        .consumer("cosmicforge", TenantClassification::FirstParty)
        .await;
    let issued = h
        .issue(
            c.sa(calling::domain::EnvironmentKind::Production),
            &["principals.read"],
        )
        .await;
    let secret = issued.secret.expose_once().to_string();
    let public_id = issued.credential.public_id.clone();
    let credential_id = issued.credential.id.to_string();
    let cli = Cli::of(&h.db);
    let app = ["--tenant", "cosmicforge", "--application", "app"];

    // Identity administration is not Calling's: the mutating commands do not exist.
    for args in [
        vec![
            "tenant",
            "create",
            "--slug",
            "x",
            "--name",
            "X",
            "--classification",
            "external",
        ],
        with(
            &["credential", "create"],
            &with(
                &app,
                &[
                    "--environment",
                    "production",
                    "--service-account",
                    "backend",
                ],
            ),
        ),
        vec!["credential", "revoke", "--credential", public_id.as_str()],
        with(&["scope", "grant"], &with(&app, &["--scope", "calls.read"])),
    ] {
        let out = cli.run(&args);
        assert_eq!(out.code, 2, "{args:?} exists: {}", out.stdout);
        assert!(out.stdout.is_empty());
    }

    // Reads of the projection: masked, never the secret.
    let tenants = cli.ok(&["tenant", "list"]).json();
    assert!(tenants
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["slug"] == "cosmicforge" && t["classification"] == "FIRST_PARTY"));
    for args in [
        vec!["credential", "show", "--credential", public_id.as_str()],
        vec!["credential", "show", "--credential", credential_id.as_str()],
        with(&["credential", "list"], &app),
    ] {
        let out = cli.ok(&args);
        assert!(out.stdout.contains(&public_id), "{args:?}");
        assert!(
            !out.stdout.contains(secret_part(&secret)),
            "{args:?} printed the secret"
        );
        assert!(!out.stderr.contains(secret_part(&secret)));
    }

    // Limits are Calling's, and the effective policy resolves through the projection.
    cli.ok(&with(
        &["limits", "set-application"],
        &with(&app, &["--requests-per-minute", "100"]),
    ));
    cli.ok(&[
        "limits",
        "set-credential",
        "--credential",
        &public_id,
        "--requests-per-minute",
        "20",
    ]);
    let effective = cli
        .ok(&["limits", "effective", "--credential", &public_id])
        .json();
    assert_eq!(effective["requests_per_minute"]["value"], 20);
    assert_eq!(effective["requests_per_minute"]["source"], "CREDENTIAL");
    // The credential still authenticates (through Platform Access).
    h.ctx(&secret).await;

    // Calling's audit has exactly the Calling mutations.
    assert_eq!(
        audit_actions(h.conn()).await,
        vec![("LIMITS_SET".to_string(), 2)]
    );
    let rows =
        common::enterprise::strings(h.conn(), "SELECT t::text FROM platform_operator_audit t")
            .await
            .join("\n");
    assert!(!rows.contains(secret_part(&secret)));
}

/// `(action, rows)` of the operator audit, by action.
async fn audit_actions(conn: &sea_orm::DatabaseConnection) -> Vec<(String, i64)> {
    use sea_orm::{ConnectionTrait, Statement};
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT action, count(*) FROM platform_operator_audit GROUP BY action ORDER BY action",
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| {
        (
            r.try_get_by_index::<String>(0).unwrap(),
            r.try_get_by_index::<i64>(1).unwrap(),
        )
    })
    .collect()
}

/// Rows of one action: `(actor, correlation_id, target_kind, target_id, detail)`.
async fn audit_rows(
    conn: &sea_orm::DatabaseConnection,
    action: &str,
) -> Vec<(String, String, String, Option<uuid::Uuid>, Value)> {
    use sea_orm::{ConnectionTrait, Statement};
    conn.query_all(Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "SELECT actor, correlation_id, target_kind, target_id, detail FROM platform_operator_audit \
         WHERE action = $1 ORDER BY occurred_at, id",
        [action.into()],
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| {
        (
            r.try_get_by_index(0).unwrap(),
            r.try_get_by_index(1).unwrap(),
            r.try_get_by_index(2).unwrap(),
            r.try_get_by_index(3).unwrap(),
            r.try_get_by_index(4).unwrap(),
        )
    })
    .collect()
}

#[tokio::test]
async fn every_mutation_needs_an_operator_and_writes_one_append_only_audit_row() {
    use axum::http::StatusCode;
    let Some(w) = common::app::AppWorld::new().await else {
        return;
    };
    let conn = w.h().conn();
    let cli = Cli::of(&w.h().db);
    let env = [
        "--tenant",
        "acme",
        "--application",
        "app",
        "--environment",
        "production",
    ];
    let tenant_limit = [
        "limits",
        "set-tenant",
        "--tenant",
        "acme",
        "--requests-per-minute",
        "1000",
    ];

    // No operator: refused before anything is touched (exit 2), whatever the command.
    for args in [tenant_limit.to_vec(), with(&["compliance", "revoke"], &env)] {
        let out = cli.run_raw(&args, &[]);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert!(out.stderr.contains("--operator"), "{}", out.stderr);
    }
    // A malformed operator id is no operator.
    let out = cli.run_raw(&with(&["--operator", "bad op"], &tenant_limit), &[]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM platform_tenant_limits").await,
        0
    );
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM platform_operator_audit").await,
        0
    );
    // Reads need no operator.
    assert_eq!(cli.run_raw(&["tenant", "list"], &[]).code, 0);
    // CF_OPERATOR_ID is the operator when --operator is absent.
    let out = cli.run_raw(&tenant_limit, &[("CF_OPERATOR_ID", "ops.env")]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    cli.ok(&tenant_limit);
    let limits = audit_rows(conn, "LIMITS_SET").await;
    assert_eq!(limits.len(), 2);
    assert_eq!(limits[0].0, "ops.env");
    assert_eq!(limits[1].0, OPERATOR);
    assert_ne!(limits[0].1, limits[1].1);
    assert_eq!(limits[0].3, Some(w.w.consumer.tenant.id.as_uuid()));

    // An abuse report and a block rule to administer.
    let (call, _, _) = w.connected_call("VOICE").await;
    let r = w
        .post(
            &format!("/v1/calls/{call}/abuse-reports"),
            serde_json::json!({ "principalId": w.w.initiator.id(), "category": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let report = r.json["abuseReportId"].as_str().unwrap().to_string();
    let r = w
        .post(
            "/v1/blocks",
            serde_json::json!({ "principalId": w.w.initiator.id(),
                                "blockedPrincipalId": w.w.recipient.id(), "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let block = r.json["blockId"].as_str().unwrap().to_string();

    // Every other administered resource, once each.
    let policy_file = std::env::temp_dir().join(format!("routing-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &policy_file,
        serde_json::to_string(&calling::routing::policy::RoutingPolicy::default()).unwrap(),
    )
    .unwrap();
    let credential = w.w.token.credential.id.as_uuid().to_string();
    let principal = w.w.recipient.id().as_uuid().to_string();
    let expires = (Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let sa = with(&env, &["--service-account", "backend"]);
    for args in [
        with(
            &["limits", "set-service-account"],
            &with(&sa, &["--requests-per-minute", "900"]),
        ),
        vec![
            "limits",
            "set-principal",
            "--principal",
            principal.as_str(),
            "--requests-per-minute",
            "50",
        ],
        with(
            &["routing-policy", "set"],
            &with(&env, &["--file", policy_file.to_str().unwrap()]),
        ),
        with(
            &["spend-policy", "set"],
            &with(
                &env,
                &[
                    "--currency",
                    "NGN",
                    "--leg-allowance-minor",
                    "500",
                    "--daily-limit-minor",
                    "5000",
                    "--monthly-limit-minor",
                    "50000",
                ],
            ),
        ),
        with(
            &["inbound-owner", "set"],
            &with(&env, &["--credential", credential.as_str()]),
        ),
        with(
            &["enterprise-capabilities", "set"],
            &with(&env, &["--capability", "manage-routing"]),
        ),
        with(
            &["compliance", "set"],
            &with(
                &env,
                &[
                    "--status",
                    "eligible",
                    "--countries",
                    "NG,GB",
                    "--expires-at",
                    expires.as_str(),
                    "--evidence-ref",
                    "KYC-2026:0042",
                ],
            ),
        ),
        with(&["compliance", "revoke"], &env),
        vec![
            "abuse-report",
            "set-status",
            "--report",
            report.as_str(),
            "--status",
            "reviewed",
        ],
        vec!["block", "release", "--block", block.as_str()],
    ] {
        cli.ok(&args);
    }
    std::fs::remove_file(&policy_file).ok();
    let actions: Vec<String> = audit_actions(conn)
        .await
        .into_iter()
        .map(|(a, _)| a)
        .collect();
    for action in [
        "ABUSE_REPORT_STATUS_SET",
        "BLOCK_RULE_RELEASED",
        "COMPLIANCE_ELIGIBILITY_REVOKED",
        "COMPLIANCE_ELIGIBILITY_SET",
        "ENTERPRISE_CAPABILITIES_SET",
        "INBOUND_OWNER_SET",
        "LIMITS_SET",
        "ROUTING_POLICY_SET",
        "SPEND_POLICY_SET",
    ] {
        let rows = audit_rows(conn, action).await;
        assert!(!rows.is_empty(), "{action} was not audited");
        for (actor, correlation, ..) in &rows {
            // The first tenant limit was set through CF_OPERATOR_ID (checked above).
            if action == "LIMITS_SET" && actor == "ops.env" {
                continue;
            }
            assert_eq!(actor, OPERATOR, "{action}");
            assert!(
                correlation.starts_with("op_") && correlation.len() >= 8,
                "{action}"
            );
        }
        assert!(actions.contains(&action.to_string()));
    }
    // 2 tenant limit rows + 10 commands above (limits: one row per level).
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM platform_operator_audit").await,
        12
    );
    let block_row = &audit_rows(conn, "BLOCK_RULE_RELEASED").await[0];
    assert_eq!(block_row.2, "BLOCK_RULE");
    assert_eq!(block_row.3.unwrap().to_string(), block);
    assert_eq!(
        common::scalar_i64(conn, &format!("SELECT count(*) FROM calling_block_rules WHERE id = '{block}' AND status = 'RELEASED'")).await,
        1
    );
    assert_eq!(
        common::scalar_i64(conn, &format!("SELECT count(*) FROM calling_abuse_reports WHERE id = '{report}' AND status = 'REVIEWED'")).await,
        1
    );
    // The opaque evidence reference is stored for the record, but never echoed by the
    // audit or by `compliance show`.
    let set = &audit_rows(conn, "COMPLIANCE_ELIGIBILITY_SET").await[0];
    assert_eq!(set.4["evidenceRefPresent"], true);
    assert_eq!(set.4["countries"], serde_json::json!(["GB", "NG"]));
    let audit_text =
        common::enterprise::strings(conn, "SELECT t::text FROM platform_operator_audit t")
            .await
            .join("\n");
    assert!(!audit_text.contains("KYC-2026"));
    let shown = cli
        .run_raw(&with(&["compliance", "show"], &env), &[])
        .json();
    assert_eq!(shown["eligibility"]["status"], "INELIGIBLE");
    assert_eq!(shown["eligibility"]["evidenceRefPresent"], false);

    // The audit table is append-only.
    for statement in [
        "UPDATE platform_operator_audit SET actor = 'someone-else'",
        "DELETE FROM platform_operator_audit",
        "TRUNCATE platform_operator_audit",
    ] {
        assert!(
            common::exec(conn, statement).await.is_err(),
            "{statement} was accepted"
        );
    }
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM platform_operator_audit").await,
        12
    );
}

#[tokio::test]
async fn compliance_commands_validate_bounded_opaque_input_and_show_a_safe_view() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = Harness::on(Arc::new(db), ManualClock::new(Utc::now()));
    h.consumer("acme", TenantClassification::External).await;
    let cli = Cli::of(&h.db);
    let env = [
        "--tenant",
        "acme",
        "--application",
        "app",
        "--environment",
        "production",
    ];
    let set = |extra: &[&str]| cli.run(&with(&["compliance", "set"], &with(&env, extra)));
    let past = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    // Refused input: nothing stored, nothing audited.
    for bad in [
        vec![
            "--status",
            "eligible",
            "--evidence-ref",
            "https://docs.example/kyc.pdf",
        ],
        vec!["--status", "eligible", "--evidence-ref", "+2348012345678"],
        vec!["--status", "eligible", "--countries", "Nigeria"],
        vec!["--status", "eligible", "--expires-at", past.as_str()],
        vec!["--status", "eligible", "--expires-at", "tomorrow"],
    ] {
        let out = set(&bad);
        assert_ne!(out.code, 0, "{bad:?} accepted");
        assert!(!out.stderr.contains("+2348012345678"));
    }
    for table in ["platform_compliance_eligibility", "platform_operator_audit"] {
        assert_eq!(
            common::scalar_i64(h.conn(), &format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    // Revoking nothing is not found.
    assert_ne!(cli.run(&with(&["compliance", "revoke"], &env)).code, 0);
    let shown = set(&[
        "--status",
        "eligible",
        "--countries",
        "NG",
        "--evidence-ref",
        "case.17",
    ]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    let view = shown.json();
    assert_eq!(view["status"], "ELIGIBLE");
    assert_eq!(view["countries"], serde_json::json!(["NG"]));
    assert_eq!(view["evidenceRefPresent"], true);
    assert_eq!(view["decidedBy"], OPERATOR);
    assert!(
        !shown.stdout.contains("case.17"),
        "the evidence reference was echoed"
    );
    let not_assessed = set(&["--status", "not-assessed"]).json();
    assert_eq!(not_assessed["status"], "NOT_ASSESSED");
    assert_eq!(not_assessed["version"], 2);
    assert_eq!(
        common::scalar_i64(h.conn(), "SELECT count(*) FROM platform_operator_audit WHERE action = 'COMPLIANCE_ELIGIBILITY_SET'").await,
        2
    );
}

/// The credential pepper is Platform Access's: a Calling process that still has one in its
/// environment refuses to start (no second verifier authority, no stray secret).
#[tokio::test]
async fn cli_refuses_to_start_with_a_leftover_pepper() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let cli = Cli::of(&db);
    for var in [
        "API_CREDENTIAL_PEPPER",
        "API_CREDENTIAL_PREVIOUS_PEPPER_FILE",
    ] {
        let out = cli.run_raw(
            &["tenant", "list"],
            &[(var, "leftover-pepper-value-0123456789abcdefghijkl")],
        );
        assert_eq!(out.code, 2, "{var} accepted");
        assert!(out.stderr.contains("API_CREDENTIAL_"), "{}", out.stderr);
        assert!(!out.stderr.contains("leftover-pepper-value"));
        assert!(out.stdout.is_empty());
    }
}

/// Part H §80 (ADR 0030) and §74.12: retention holds, principal tombstones and webhook
/// key reseals are operator commands — refused without an operator, audited with one
/// (one row per mutation, opaque ids and counts only); the read-only retention commands
/// need no operator.
#[tokio::test]
async fn retention_holds_tombstones_and_reseals_are_operator_audited() {
    use base64::Engine;
    let Some(w) = common::app::AppWorld::new().await else {
        return;
    };
    let conn = w.h().conn();
    let cli = Cli::of(&w.h().db);
    let env = [
        "--tenant",
        "acme",
        "--application",
        "app",
        "--environment",
        "production",
    ];
    let principal = w.w.recipient.id().to_string();
    let key: Vec<u8> = (0..32u8)
        .map(|i| i.wrapping_mul(7).wrapping_add(3))
        .collect();
    let key = base64::engine::general_purpose::STANDARD.encode(key);
    let key_env = [("CALLING_WEBHOOK_SECRET_KEY", key.as_str())];

    // No operator: refused before anything is touched.
    let some_hold = uuid::Uuid::new_v4().to_string();
    for args in [
        with(&["retention", "hold", "--reason", "dispute"], &env),
        vec!["retention", "release", "--hold", some_hold.as_str()],
        vec!["principal", "tombstone", "--principal", principal.as_str()],
    ] {
        let out = cli.run_raw(&args, &key_env);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert!(out.stderr.contains("--operator"), "{}", out.stderr);
    }
    let out = cli.run_raw(&["webhooks", "reseal"], &key_env);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM platform_operator_audit").await,
        0
    );
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM calling_retention_holds").await,
        0
    );

    // Read-only: no operator needed.
    let policies = cli.run_raw(&["retention", "policies"], &[]);
    assert_eq!(policies.code, 0, "{}", policies.stderr);
    let policies = policies.json();
    assert_eq!(policies["policies"].as_array().unwrap().len(), 14);
    assert!(policies["policies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["dataClass"] == "CALL_RECORD" && p["action"] == "MINIMIZE" && p["days"] == 365));
    let refused = cli.run_raw(
        &["retention", "policies"],
        &[("CALLING_RETENTION_CALL_EVENTS_DAYS", "30")],
    );
    assert_ne!(refused.code, 0);
    assert!(refused
        .stderr
        .contains("CALLING_RETENTION_CALL_EVENTS_DAYS"));

    // Holds: scoped, platform-wide; a partial scope or a classed platform-wide hold is
    // refused and leaves nothing behind.
    let held = cli
        .ok(&with(
            &[
                "retention",
                "hold",
                "--data-class",
                "CALL_RECORD",
                "--resource",
                principal.as_str(),
                "--reason",
                "dispute",
            ],
            &env,
        ))
        .json();
    assert_eq!(held["dataClass"], "CALL_RECORD");
    assert_eq!(held["reasonCode"], "DISPUTE");
    assert_eq!(held["createdBy"], OPERATOR);
    let everything = cli
        .ok(&[
            "retention",
            "hold",
            "--platform-wide",
            "--reason",
            "security-investigation",
        ])
        .json();
    assert!(everything["tenantId"].is_null());
    for args in [
        vec!["retention", "hold", "--tenant", "acme", "--reason", "other"],
        vec![
            "retention",
            "hold",
            "--platform-wide",
            "--data-class",
            "CALL_RECORD",
            "--reason",
            "other",
        ],
        with(
            &[
                "retention",
                "hold",
                "--data-class",
                "NOT_A_CLASS",
                "--reason",
                "other",
            ],
            &env,
        ),
    ] {
        let out = cli.run(&args);
        assert_ne!(out.code, 0, "{args:?} accepted");
    }
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM calling_retention_holds").await,
        2
    );
    let listed = cli.run_raw(&["retention", "list"], &[]).json();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    let hold_id = held["id"].as_str().unwrap().to_string();
    let released = cli
        .ok(&["retention", "release", "--hold", hold_id.as_str()])
        .json();
    assert!(released["releasedAt"].is_string());
    cli.ok(&["retention", "release", "--hold", hold_id.as_str()]);
    assert_eq!(
        cli.run_raw(&["retention", "list"], &[])
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        cli.run_raw(&["retention", "list", "--all"], &[])
            .json()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let placed = audit_rows(conn, "RETENTION_HOLD_PLACED").await;
    assert_eq!(placed.len(), 2);
    assert!(placed
        .iter()
        .all(|r| r.0 == OPERATOR && r.2 == "RETENTION_HOLD"));
    assert_eq!(placed[0].3.unwrap().to_string(), hold_id);
    assert_eq!(placed[0].4["reasonCode"], "DISPUTE");
    let released_rows = audit_rows(conn, "RETENTION_HOLD_RELEASED").await;
    assert_eq!(released_rows.len(), 2);
    assert_eq!(released_rows[0].4["wasActive"], true);
    assert_eq!(released_rows[1].4["wasActive"], false);
    // The platform-wide hold keeps the principal from being tombstoned.
    let out = cli.run(&["principal", "tombstone", "--principal", principal.as_str()]);
    assert_ne!(out.code, 0);
    let everything_id = everything["id"].as_str().unwrap().to_string();
    cli.ok(&["retention", "release", "--hold", everything_id.as_str()]);

    // Tombstone.
    let tomb = cli
        .ok(&["principal", "tombstone", "--principal", principal.as_str()])
        .json();
    assert_eq!(tomb["alreadyTombstoned"], false);
    assert_eq!(tomb["status"], "DEACTIVATED");
    let rows = audit_rows(conn, "PRINCIPAL_TOMBSTONED").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, "PRINCIPAL");
    assert_eq!(rows[0].3.unwrap().to_string(), principal);
    assert!(!rows[0].4.to_string().contains("recipient-1"));
    let shown = cli
        .run_raw(
            &["principal", "show", "--principal", principal.as_str()],
            &[],
        )
        .json();
    assert_eq!(shown["status"], "DEACTIVATED");
    assert_ne!(shown["externalReference"], "recipient-1");
    // Platform Access refuses to bring it back.
    assert!(w
        .h()
        .platform
        .set_principal_status(w.w.recipient.id(), calling::domain::PrincipalStatus::Active)
        .await
        .is_err());

    // Webhook key reseal: one audit row with counts and the batch size only.
    let out = cli.run_raw(
        &[
            "--operator",
            OPERATOR,
            "webhooks",
            "reseal",
            "--batch",
            "10",
        ],
        &key_env,
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["resealed"], 0);
    let rows = audit_rows(conn, "WEBHOOK_KEYS_RESEALED").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, "WEBHOOK_KEY_RING");
    assert!(rows[0].3.is_none());
    assert_eq!(
        rows[0].4,
        serde_json::json!({ "resealed": 0, "unopenable": 0, "batches": 1, "batchSize": 10 })
    );
    assert!(!rows[0].4.to_string().contains(&key));
}

/// §86: `platform_admin incidents list|resolve`. Listing is read-only (no operator);
/// resolving requires an operator and writes exactly one INCIDENT_RESOLVED audit row in
/// the same transaction (the audit rows are the resolution history). A later occurrence of
/// the same condition opens a NEW incident.
#[tokio::test]
async fn incidents_are_listed_and_resolved_by_an_audited_operator() {
    let Some(w) = common::app::AppWorld::new().await else {
        return;
    };
    let conn = w.h().conn();
    let cli = Cli::of(&w.h().db);
    let scope = w.w.scope();
    let scoped = uuid::Uuid::new_v4();
    let platform = uuid::Uuid::new_v4();
    common::exec(
        conn,
        &format!(
            "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, dedupe_key, \
               category, severity, status, resource_type, resource_id, occurrences, restricted_evidence, \
               first_seen_at, last_seen_at) VALUES \
             ('{scoped}', '{}', '{}', '{}', 'telephony_command_dead:telephony_command:{}', \
              'TELEPHONY_COMMAND_DEAD', 'HIGH', 'OPEN', 'TELEPHONY_COMMAND', '{}', 3, \
              '{{\"commandKind\": \"TERMINATE\"}}', now(), now()), \
             ('{platform}', NULL, NULL, NULL, 'worker_backlog:00000000000000000000000000000000:outbox', \
              'WORKER_BACKLOG', 'HIGH', 'OPEN', NULL, NULL, 1, '{{\"queue\": \"outbox\"}}', now(), now())",
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
            scoped.simple(),
            scoped,
        ),
    )
    .await
    .unwrap();

    // Read-only listing: no operator, filters by status / category, bounded.
    let all = cli.run_raw(&["incidents", "list"], &[]);
    assert_eq!(all.code, 0, "{}", all.stderr);
    assert_eq!(all.json().as_array().unwrap().len(), 2);
    let dead = cli.run_raw(
        &[
            "incidents",
            "list",
            "--category",
            "TELEPHONY_COMMAND_DEAD",
            "--status",
            "open",
        ],
        &[],
    );
    assert_eq!(dead.code, 0, "{}", dead.stderr);
    let dead = dead.json();
    assert_eq!(dead.as_array().unwrap().len(), 1);
    assert_eq!(dead[0]["id"], scoped.to_string());
    assert_eq!(dead[0]["occurrences"], 3);
    assert_ne!(
        cli.run_raw(&["incidents", "list", "--category", "NOPE"], &[])
            .code,
        0
    );

    // Resolving needs an operator: refused, nothing changed, nothing audited.
    let id = scoped.to_string();
    let out = cli.run_raw(&["incidents", "resolve", "--incident", &id], &[]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("--operator"), "{}", out.stderr);
    assert_eq!(
        common::scalar_i64(
            conn,
            "SELECT count(*) FROM calling_incidents WHERE status = 'OPEN'"
        )
        .await,
        2
    );
    assert_eq!(
        common::scalar_i64(conn, "SELECT count(*) FROM platform_operator_audit").await,
        0
    );

    // With an operator: RESOLVED + one audit row (scope, category, previous status).
    let resolved = cli.ok(&["incidents", "resolve", "--incident", &id]).json();
    assert_eq!(resolved["status"], "RESOLVED");
    assert!(resolved["resolvedAt"].is_string());
    let rows = audit_rows(conn, "INCIDENT_RESOLVED").await;
    assert_eq!(rows.len(), 1);
    let (actor, correlation, target_kind, target_id, detail) = &rows[0];
    assert_eq!(actor, OPERATOR);
    assert!(correlation.starts_with("op_"));
    assert_eq!(target_kind, "INCIDENT");
    assert_eq!(*target_id, Some(scoped));
    assert_eq!(detail["category"], "TELEPHONY_COMMAND_DEAD");
    assert_eq!(detail["previousStatus"], "OPEN");
    // The platform-wide incident resolves the same way (unscoped audit row).
    cli.ok(&["incidents", "resolve", "--incident", &platform.to_string()]);
    assert_eq!(
        common::scalar_i64(
            conn,
            &format!(
                "SELECT count(*) FROM platform_operator_audit WHERE action = 'INCIDENT_RESOLVED' \
                 AND target_id = '{platform}' AND tenant_id IS NULL"
            )
        )
        .await,
        1
    );
    // Idempotent: resolving again changes nothing (audited like every operator command).
    let again = cli.ok(&["incidents", "resolve", "--incident", &id]).json();
    assert_eq!(again["resolvedAt"], resolved["resolvedAt"]);
    assert_eq!(audit_rows(conn, "INCIDENT_RESOLVED").await.len(), 3);
    // Unknown incident: refused, nothing audited.
    let unknown = uuid::Uuid::new_v4().to_string();
    assert_ne!(
        cli.run(&["incidents", "resolve", "--incident", &unknown])
            .code,
        0
    );
    assert_eq!(audit_rows(conn, "INCIDENT_RESOLVED").await.len(), 3);
    let open = cli.run_raw(&["incidents", "list", "--status", "open"], &[]);
    assert_eq!(open.json().as_array().unwrap().len(), 0);

    // A later occurrence of the same condition opens a NEW incident (the resolved one is
    // history): the open-key uniqueness only covers OPEN rows.
    common::exec(
        conn,
        &format!(
            "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, dedupe_key, \
               category, severity, status, resource_type, resource_id, occurrences, restricted_evidence, \
               first_seen_at, last_seen_at) VALUES \
             (gen_random_uuid(), '{}', '{}', '{}', 'telephony_command_dead:telephony_command:{}', \
              'TELEPHONY_COMMAND_DEAD', 'HIGH', 'OPEN', 'TELEPHONY_COMMAND', '{}', 1, '{{}}', now(), now())",
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
            scoped.simple(),
            scoped,
        ),
    )
    .await
    .unwrap();
}

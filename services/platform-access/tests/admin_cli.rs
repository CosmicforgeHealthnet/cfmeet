//! Operator CLI (`platform-access-admin`) end to end (moved from Calling's `platform_admin`
//! with the authority): it drives the same domain services, prints the raw credential
//! exactly once (create / rotate) and never otherwise, refuses every mutation without a
//! named operator, and writes exactly one append-only `platform_access_audit` row per
//! successful mutation (none for refused ones).

mod common;

use std::{process::Command, sync::Arc};

use chrono::Utc;
use common::{keyring, Harness, TestDb, TEST_PEPPER};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::Value;

/// The operator every mutating command in these tests acts as.
const OPERATOR: &str = "ops.alice";

struct Cli {
    database_url: String,
    pepper: String,
}

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
    fn new(db: &TestDb) -> Self {
        Self {
            database_url: db.url.clone(),
            pepper: TEST_PEPPER.to_string(),
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
        let output = Command::new(env!("CARGO_BIN_EXE_platform-access-admin"))
            .args(args)
            .current_dir(std::env::temp_dir())
            .env_clear()
            .env("DATABASE_URL", &self.database_url)
            .env("API_CREDENTIAL_PEPPER", &self.pepper)
            .env("RUST_LOG", "info")
            .envs(env.iter().copied())
            .output()
            .expect("run platform-access-admin");
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

fn secret_part(token: &str) -> &str {
    token.rsplit('_').next().unwrap()
}

fn with<'a>(base: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
    base.iter().chain(extra.iter()).copied().collect()
}

const APP: &[&str] = &["--tenant", "cosmicforge", "--application", "dispatch"];

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

async fn audit_actions(conn: &DatabaseConnection) -> Vec<(String, i64)> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT action, count(*) FROM platform_access_audit GROUP BY action ORDER BY action",
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

/// A Platform Access deployment over the CLI's database, on the wall clock.
fn service_over(db: TestDb) -> Harness {
    let h = Harness::on(Arc::new(db), keyring(TEST_PEPPER));
    h.clock.set(Utc::now());
    h
}

#[tokio::test]
async fn cli_manages_the_full_credential_lifecycle() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let cli = Cli::new(&db);

    cli.ok(&[
        "tenant",
        "create",
        "--slug",
        "cosmicforge",
        "--name",
        "CosmicForge",
        "--classification",
        "first-party",
    ]);
    cli.ok(&[
        "application",
        "create",
        "--tenant",
        "cosmicforge",
        "--slug",
        "dispatch",
        "--name",
        "Dispatch",
    ]);
    for env in ["production", "sandbox"] {
        cli.ok(&with(
            &["environment", "create"],
            &with(APP, &["--environment", env]),
        ));
        cli.ok(&with(
            &["service-account", "create"],
            &with(APP, &["--environment", env, "--name", "backend"]),
        ));
    }
    cli.ok(&with(
        &["scope", "grant"],
        &with(APP, &["--scope", "principals.read"]),
    ));
    let registry = cli.ok(&["scope", "registry"]).json();
    for scope in [
        "principals.read",
        "numbers.search",
        "numbers.admin",
        "sms.send",
    ] {
        assert!(
            registry
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == scope),
            "{scope} not in the registry"
        );
    }

    // Create: the secret is printed exactly once, on stdout only.
    let created = cli.ok(&with(
        &["credential", "create"],
        &with(
            APP,
            &[
                "--environment",
                "production",
                "--service-account",
                "backend",
                "--scope",
                "principals.read",
                "--label",
                "cli",
            ],
        ),
    ));
    let json = created.json();
    let secret = json["secret"].as_str().unwrap().to_string();
    let public_id = json["credential"]["publicId"].as_str().unwrap().to_string();
    let credential_id = json["credential"]["id"].as_str().unwrap().to_string();
    assert!(secret.starts_with("cfmk_live_"));
    assert_eq!(created.stdout.matches(secret_part(&secret)).count(), 1);
    assert!(
        !created.stderr.contains(secret_part(&secret)),
        "secret in CLI logs"
    );
    assert_eq!(
        json["credential"]["scopes"],
        serde_json::json!(["principals.read"])
    );

    // No other command can recover it.
    for args in [
        vec!["credential", "show", "--credential", public_id.as_str()],
        vec!["credential", "show", "--credential", credential_id.as_str()],
        with(&["credential", "list"], APP),
        with(
            &["credential", "list"],
            &with(
                APP,
                &[
                    "--environment",
                    "production",
                    "--service-account",
                    "backend",
                ],
            ),
        ),
    ] {
        let out = cli.ok(&args);
        assert!(
            !out.stdout.contains(secret_part(&secret)),
            "{args:?} printed the secret"
        );
        assert!(!out.stderr.contains(secret_part(&secret)));
    }

    // The issued credential authenticates through the service.
    let h = service_over(db);
    let auth = h.authenticate(&secret).await;
    assert_eq!(auth.body["outcome"], "AUTHENTICATED", "{}", auth.body);
    assert_eq!(
        auth.body["authority"]["tenant"]["classification"],
        "FIRST_PARTY"
    );
    assert_eq!(
        auth.body["authority"]["scopes"],
        serde_json::json!(["principals.read"])
    );

    // Rotate: new secret once; both valid during the overlap.
    let rotated = cli.ok(&[
        "credential",
        "rotate",
        "--credential",
        &public_id,
        "--overlap-minutes",
        "10",
    ]);
    let new_secret = rotated.json()["secret"].as_str().unwrap().to_string();
    assert_ne!(new_secret, secret);
    assert!(!rotated.stderr.contains(secret_part(&new_secret)));
    h.clock.set(Utc::now());
    for s in [&secret, &new_secret] {
        assert_eq!(h.authenticate(s).await.body["outcome"], "AUTHENTICATED");
    }

    // Revoke: immediate.
    let revoked = cli
        .ok(&["credential", "revoke", "--credential", &public_id])
        .json();
    assert_eq!(revoked["status"], "REVOKED");
    assert_eq!(
        h.authenticate(&secret).await.body["reason"],
        "credential_revoked"
    );
    assert_eq!(
        h.authenticate(&new_secret).await.body["outcome"],
        "AUTHENTICATED"
    );

    // Principals: opaque references only.
    let phone = cli.run(&with(
        &["principal", "register"],
        &with(
            APP,
            &[
                "--environment",
                "production",
                "--kind",
                "telephone-endpoint",
                "--reference",
                "+2348012345678",
            ],
        ),
    ));
    assert_eq!(phone.code, 1);
    assert!(phone.stderr.contains("telephone"), "{}", phone.stderr);
    let registered = cli
        .ok(&with(
            &["principal", "register"],
            &with(
                APP,
                &[
                    "--environment",
                    "production",
                    "--kind",
                    "external-application-principal",
                    "--reference",
                    "user-7",
                ],
            ),
        ))
        .json();
    assert_eq!(registered["created"], true);
    let sa_principal = cli
        .ok(&with(
            &["principal", "register-service-account"],
            &with(
                APP,
                &[
                    "--environment",
                    "production",
                    "--service-account",
                    "backend",
                ],
            ),
        ))
        .json();
    assert_eq!(sa_principal["principal"]["kind"], "SERVICE_ACCOUNT");

    // Policy violations surface as failures.
    let live_sandbox = cli.run(&with(
        &["environment", "set-live-telecom"],
        &with(APP, &["--environment", "sandbox", "--allowed", "true"]),
    ));
    assert_eq!(live_sandbox.code, 1);
    cli.ok(&[
        "tenant",
        "create",
        "--slug",
        "acme",
        "--name",
        "Acme",
        "--classification",
        "external",
    ]);
    cli.ok(&[
        "application",
        "create",
        "--tenant",
        "acme",
        "--slug",
        "app",
        "--name",
        "App",
    ]);
    let trusted = cli.run(&[
        "scope",
        "grant",
        "--tenant",
        "acme",
        "--application",
        "app",
        "--scope",
        "events.private_delivery",
    ]);
    assert_eq!(trusted.code, 1);
    assert!(trusted.stderr.contains("FIRST_PARTY"), "{}", trusted.stderr);
    // The privileged numbering scope is never an ordinary grant.
    let privileged = cli.run(&with(
        &["scope", "grant"],
        &with(APP, &["--scope", "numbers.admin"]),
    ));
    assert_eq!(privileged.code, 1, "{}", privileged.stdout);

    // Suspension leaves only the safety-only authority (Part H §72).
    cli.ok(&["tenant", "suspend", "--tenant", "cosmicforge"]);
    let suspended = h.authenticate(&new_secret).await;
    assert_eq!(
        suspended.body["authority"]["authorityMode"], "SAFETY_ONLY",
        "{}",
        suspended.body
    );
    cli.ok(&["tenant", "reactivate", "--tenant", "cosmicforge"]);
    assert_eq!(
        h.authenticate(&new_secret).await.body["authority"]["authorityMode"],
        "FULL"
    );

    // Operator audit: exactly one row per successful mutation, none for the refused ones
    // (telephone reference, live telecom in sandbox, first-party scope, privileged scope),
    // each attributed to the operator with its own correlation id.
    let actions = audit_actions(h.conn()).await;
    let expected = [
        ("APPLICATION_CREATED", 2),
        ("CREDENTIAL_CREATED", 1),
        ("CREDENTIAL_REVOKED", 1),
        ("CREDENTIAL_ROTATED", 1),
        ("ENVIRONMENT_CREATED", 2),
        ("PRINCIPAL_REGISTERED", 2),
        ("SCOPE_GRANTED", 1),
        ("SERVICE_ACCOUNT_CREATED", 2),
        ("TENANT_CREATED", 2),
        ("TENANT_STATUS_SET", 2),
    ];
    assert_eq!(
        actions,
        expected
            .iter()
            .map(|(a, n)| (a.to_string(), *n as i64))
            .collect::<Vec<_>>()
    );
    let total: i64 = expected.iter().map(|(_, n)| *n as i64).sum();
    assert_eq!(
        scalar(
            h.conn(),
            &format!("SELECT count(*) FROM platform_access_audit WHERE actor = '{OPERATOR}'")
        )
        .await,
        total
    );
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(DISTINCT correlation_id) FROM platform_access_audit"
        )
        .await,
        total,
        "one correlation id per command"
    );
    // The credential rows name the credential, never its secret.
    let rows: Vec<String> = h
        .conn()
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT t::text FROM platform_access_audit t",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect();
    let rows = rows.join("\n");
    for s in [&secret, &new_secret] {
        assert!(!rows.contains(secret_part(s)), "a secret reached the audit");
    }
    assert!(rows.contains(&credential_id));

    // The audit table is append-only.
    for statement in [
        "UPDATE platform_access_audit SET actor = 'someone-else'",
        "DELETE FROM platform_access_audit",
        "TRUNCATE platform_access_audit",
    ] {
        assert!(
            h.conn().execute_unprepared(statement).await.is_err(),
            "{statement} was accepted"
        );
    }
}

#[tokio::test]
async fn every_mutation_needs_an_operator() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let cli = Cli::new(&db);
    cli.ok(&[
        "tenant",
        "create",
        "--slug",
        "acme",
        "--name",
        "Acme",
        "--classification",
        "external",
    ]);
    // No operator: refused before anything is touched (exit 2), whatever the command.
    for args in [
        vec![
            "tenant",
            "create",
            "--slug",
            "nobody",
            "--name",
            "Nobody",
            "--classification",
            "external",
        ],
        vec!["tenant", "suspend", "--tenant", "acme"],
        vec![
            "application",
            "create",
            "--tenant",
            "acme",
            "--slug",
            "x",
            "--name",
            "X",
        ],
    ] {
        let out = cli.run_raw(&args, &[]);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert!(out.stderr.contains("--operator"), "{}", out.stderr);
    }
    // A malformed operator id is no operator.
    let out = cli.run_raw(
        &[
            "--operator",
            "bad op",
            "tenant",
            "suspend",
            "--tenant",
            "acme",
        ],
        &[],
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM platform_tenants WHERE slug = 'nobody' OR status <> 'ACTIVE'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&db.conn, "SELECT count(*) FROM platform_access_audit").await,
        1
    );
    // Reads need no operator.
    assert_eq!(cli.run_raw(&["tenant", "list"], &[]).code, 0);
    // CF_OPERATOR_ID is the operator when --operator is absent.
    let out = cli.run_raw(
        &["tenant", "suspend", "--tenant", "acme"],
        &[("CF_OPERATOR_ID", "ops.env")],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM platform_access_audit WHERE actor = 'ops.env' AND action = 'TENANT_STATUS_SET' \
             AND detail ->> 'status' = 'SUSPENDED'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn cli_refuses_to_start_without_a_strong_pepper() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    for pepper in ["", "changeme", "secret123", "development-secret"] {
        let cli = Cli {
            database_url: db.url.clone(),
            pepper: pepper.to_string(),
        };
        let out = cli.run(&["tenant", "list"]);
        assert_eq!(out.code, 2, "pepper `{pepper}` accepted");
        assert!(
            out.stderr.contains("API_CREDENTIAL_PEPPER"),
            "{}",
            out.stderr
        );
        if !pepper.is_empty() {
            assert!(!out.stderr.contains(pepper));
        }
        assert!(out.stdout.is_empty());
    }
}

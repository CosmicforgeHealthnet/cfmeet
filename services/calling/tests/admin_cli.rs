//! Operational CLI (`platform_admin`) end to end: it drives the same domain services,
//! prints the raw credential exactly once (create / rotate) and never otherwise.

mod common;

use std::{process::Command, sync::Arc};

use calling::domain::{ManualClock, TenantClassification};
use chrono::Utc;
use common::{secret_part, Harness, TestDb, TEST_PEPPER};
use serde_json::Value;

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
    fn run(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_platform_admin"))
            .args(args)
            .current_dir(std::env::temp_dir())
            .env_clear()
            .env("DATABASE_URL", &self.database_url)
            .env("API_CREDENTIAL_PEPPER", &self.pepper)
            .env("RUST_LOG", "info")
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

const APP: &[&str] = &["--tenant", "cosmicforge", "--application", "dispatch"];

fn with<'a>(base: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
    base.iter().chain(extra.iter()).copied().collect()
}

#[tokio::test]
async fn cli_manages_the_full_credential_lifecycle() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let cli = Cli {
        database_url: db.url.clone(),
        pepper: TEST_PEPPER.to_string(),
    };

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
    assert!(registry
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "principals.read"));

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
    let public_id = json["credential"]["public_id"]
        .as_str()
        .unwrap()
        .to_string();
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
    let h = Harness::on(Arc::new(db), TEST_PEPPER, ManualClock::new(Utc::now()));
    h.clock.set(Utc::now());
    let ctx = h.ctx(&secret).await;
    assert_eq!(
        ctx.tenant_classification(),
        TenantClassification::FirstParty
    );
    assert!(ctx.has_scope("principals.read"));

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
    h.ctx(&secret).await;
    h.ctx(&new_secret).await;

    // Revoke: immediate.
    let revoked = cli
        .ok(&["credential", "revoke", "--credential", &public_id])
        .json();
    assert_eq!(revoked["status"], "REVOKED");
    assert!(h.authenticate(&secret).await.is_err());
    h.ctx(&new_secret).await;

    // Limits and the effective policy.
    cli.ok(&with(
        &["limits", "set-application"],
        &with(APP, &["--requests-per-minute", "100"]),
    ));
    let new_public = rotated.json()["credential"]["public_id"]
        .as_str()
        .unwrap()
        .to_string();
    cli.ok(&[
        "limits",
        "set-credential",
        "--credential",
        &new_public,
        "--requests-per-minute",
        "20",
    ]);
    let effective = cli
        .ok(&["limits", "effective", "--credential", &new_public])
        .json();
    assert_eq!(effective["requests_per_minute"]["value"], 20);
    assert_eq!(effective["requests_per_minute"]["source"], "CREDENTIAL");

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
    assert!(trusted.stderr.contains("FIRST_PARTY"));

    // Suspension through the CLI leaves only the safety-only authority (Part H §72):
    // every ordinary route is refused, safety operations (end a call, stop a
    // recording, ...) remain possible.
    cli.ok(&["tenant", "suspend", "--tenant", "cosmicforge"]);
    assert!(h.authenticate(&new_secret).await.unwrap().is_safety_only());
    cli.ok(&["tenant", "reactivate", "--tenant", "cosmicforge"]);
    h.ctx(&new_secret).await;
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
        assert!(out.stderr.contains("API_CREDENTIAL_PEPPER"));
        if !pepper.is_empty() {
            assert!(!out.stderr.contains(pepper));
        }
        assert!(out.stdout.is_empty());
    }
}

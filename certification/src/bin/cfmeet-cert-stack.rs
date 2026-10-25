//! `cfmeet-cert-stack up --env-out <path>`: starts the certification stack, provisions a
//! developer sandbox (plus a limited-scope credential of it), a second tenant and a user
//! session, writes the environment file the TypeScript / Dart SDK integration tests read
//! (`CFMEET_STACK_ENV`), and serves until SIGINT / SIGTERM, then tears everything down.

use std::path::PathBuf;

use anyhow::{bail, Context};
use cfmeet_certification::{
    provision::{self, SANDBOX_SCOPES},
    Stack, StackConfig,
};
use serde_json::json;
use uuid::Uuid;

/// Principals per SDK in `tenants.sandbox.sdkPrincipals`.
const SDK_PRINCIPALS: usize = 5;

/// Scopes of `credentials.sandboxLimited` (deliberately without `calls.read`).
const LIMITED_SCOPES: &[&str] = &["organizations.read"];

fn usage() -> ! {
    eprintln!(
        "usage: cfmeet-cert-stack up --env-out <path>\n\n\
         Needs TEST_DATABASE_URL, TEST_REDIS_URL and the service binaries in CFMEET_BIN_DIR\n\
         (default $CARGO_TARGET_DIR/debug; `make certification-bins` builds them)."
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut env_out: Option<PathBuf> = None;
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("up") => {}
        _ => usage(),
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--env-out" => env_out = it.next().map(PathBuf::from),
            _ => usage(),
        }
    }
    let Some(env_out) = env_out else { usage() };
    let Some(config) = StackConfig::from_env()? else {
        bail!("prerequisites missing (CFMEET_SKIP_DB_TESTS=1 does not apply to `up`)");
    };
    eprintln!("cfmeet-cert-stack: starting the certification stack ...");
    let stack = Stack::start(config).await?;
    let result = provision_and_serve(&stack, &env_out).await;
    if let Err(e) = &result {
        eprintln!("cfmeet-cert-stack: {e:#}");
        stack.dump_logs(40);
    }
    eprintln!("cfmeet-cert-stack: tearing down ...");
    let _ = std::fs::remove_file(&env_out);
    stack.teardown().await;
    result
}

async fn provision_and_serve(stack: &Stack, env_out: &PathBuf) -> anyhow::Result<()> {
    let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
    let sandbox = provision::developer_sandbox(
        stack,
        &format!("developer-{tag}@cert.example"),
        SANDBOX_SCOPES,
    )
    .await
    .context("provisioning the developer sandbox")?;
    let operator = stack.operator();
    let other = operator
        .customer("EXTERNAL", "SANDBOX", SANDBOX_SCOPES)
        .await
        .context("provisioning the isolation tenant")?;
    let principals = [
        operator.principal(sandbox.environment_id).await?,
        operator.principal(sandbox.environment_id).await?,
    ];
    let other_principal = operator.principal(other.environment_id).await?;
    // Principals reserved for each SDK's parity suite (Messaging devices: Messaging allows a
    // principal a bounded number of device-registration challenges per hour, so every
    // E2EE / realtime role of a suite gets its own principal).
    let mut sdk_principals = serde_json::Map::new();
    for sdk in ["typescript", "dart"] {
        let mut ids = Vec::new();
        for _ in 0..SDK_PRINCIPALS {
            ids.push(operator.principal(sandbox.environment_id).await?);
        }
        sdk_principals.insert(sdk.to_string(), json!(ids));
    }
    // A second credential of the SANDBOX environment holding only LIMITED_SCOPES: the SDK
    // parity suites prove a missing scope is refused (403 INSUFFICIENT_SCOPE) by the gateway.
    let limited_sa = operator
        .pa(
            reqwest::Method::POST,
            &format!(
                "/admin/environments/{}/service-accounts",
                sandbox.environment_id
            ),
            Some(json!({ "name": "sdk-limited", "description": "certification (limited scopes)" })),
        )
        .await?
        .expect(201, "limited service account")?
        .uuid("/id");
    let (limited_credential_id, limited_secret) =
        operator.credential(limited_sa, LIMITED_SCOPES).await?;
    let services: serde_json::Map<String, serde_json::Value> = stack
        .urls
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let env = json!({
        "gatewayUrl": stack.gateway_url(),
        "credentials": {
            "sandbox": sandbox.customer.secret,
            "otherTenant": other.secret,
            "sandboxLimited": limited_secret,
        },
        "userSession": sandbox.user.session,
        "user": { "id": sandbox.user.id, "email": sandbox.user.email },
        "tenants": {
            "sandbox": {
                "organizationId": sandbox.organization_id,
                "tenantId": sandbox.customer.tenant_id,
                "projectId": sandbox.project_id,
                "applicationId": sandbox.customer.application_id,
                "environmentId": sandbox.environment_id,
                "environmentKind": "SANDBOX",
                "serviceAccountId": sandbox.customer.service_account_id,
                "credentialId": sandbox.customer.credential_id,
                "scopes": sandbox.customer.scopes,
                "principals": principals,
                "sdkPrincipals": sdk_principals,
                "limited": {
                    "serviceAccountId": limited_sa,
                    "credentialId": limited_credential_id,
                    "scopes": LIMITED_SCOPES,
                },
            },
            "otherTenant": {
                "tenantId": other.tenant_id,
                "applicationId": other.application_id,
                "environmentId": other.environment_id,
                "environmentKind": "SANDBOX",
                "serviceAccountId": other.service_account_id,
                "credentialId": other.credential_id,
                "scopes": other.scopes,
                "principals": [other_principal],
            },
        },
        "services": services,
        "limits": {
            "sandboxRequestsPerMinute": 120,
            "numberSearchesPerMinute": cfmeet_certification::stack::NUMBER_SEARCHES_PER_MINUTE,
        },
        "stack": { "id": stack.id, "workDir": stack.dir, "logs": stack.dir.join("logs") },
    });
    let tmp = env_out.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&env)?)?;
    std::fs::rename(&tmp, env_out)?;
    eprintln!(
        "cfmeet-cert-stack: ready — gateway {} — environment written to {} (export CFMEET_STACK_ENV={}); Ctrl-C to stop",
        stack.gateway_url(),
        env_out.display(),
        env_out.display()
    );
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    Ok(())
}

//! Blueprint Sections 6–7 (ADR 0056 §4, ADR 0058): the REAL Organizations Service and the
//! REAL Platform Access as processes, each with its own database migrated by its own
//! migration binary and real Ed25519 service authentication (trust bundles derived from
//! the permission registry: Platform Access grants `platform-identity.provision-first-party`
//! to `cfmeet.organizations` only).
//!
//! Journey: two organizations are created through the account relay identity → the
//! Organizations provisioning worker (enabled) asks Platform Access for each
//! organization's FIRST_PARTY tenant + first-party application + PRODUCTION environment,
//! verifies it server-side and records READY → the verified scopes resolve, are distinct
//! and match Platform Access's own records → suspending an organization suspends its
//! tenant in Platform Access, reactivating resumes it → scope resolution is refused while
//! suspended.

use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use cf_service_auth::{services, OutboundServiceRequest, ServiceId};
use cfmeet_certification::{
    db,
    keys::{self, ServiceKeys},
    process::{self, Launch, Service},
};
use serde_json::{json, Value};
use uuid::Uuid;

const STARTED: &[&str] = &[
    services::PLATFORM_ACCESS,
    services::ORGANIZATIONS,
    services::MEETING_SERVICE,
];
const BINARIES: &[&str] = &[
    "platform-access",
    "platform-access-migration",
    "organizations",
    "organizations-migration",
];

/// A free local port no other service of this test process was given (ports are
/// released before the services bind them, so parallel tests could be offered the same
/// one); the same guard as `stack::free_port`.
fn port() -> u16 {
    static GIVEN: std::sync::Mutex<std::collections::BTreeSet<u16>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    let mut given = GIVEN.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if given.insert(port) {
            return port;
        }
    }
}

struct MiniStack {
    keys: ServiceKeys,
    http: reqwest::Client,
    dir: PathBuf,
    admin: String,
    names: Vec<String>,
    procs: Vec<Service>,
    pa_url: String,
    org_url: String,
    pa_db: String,
}

impl MiniStack {
    fn write(&self, name: &str, value: &str) -> anyhow::Result<String> {
        let path = self.dir.join(name);
        std::fs::write(&path, value)?;
        Ok(path.display().to_string())
    }

    fn auth_env(&self, identity: &str) -> anyhow::Result<Vec<(String, String)>> {
        let short = keys::short(identity);
        Ok(vec![
            ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
            ("CF_SERVICE_AUTH_KEY_ID".into(), keys::kid(identity)),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.write(&format!("{short}.pem"), &self.keys.pem(identity))?,
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.write(
                    &format!("{short}.trust.json"),
                    &keys::trust_bundle(identity, STARTED, &self.keys).to_string(),
                )?,
            ),
        ])
    }

    async fn start(admin: String, bin: PathBuf) -> anyhow::Result<Self> {
        let dir = std::env::temp_dir().join(format!("cfmeet-orgs-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("logs"))?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()?;
        let (pa_port, org_port) = loop {
            let (a, b) = (port(), port());
            if a != b {
                break (a, b);
            }
        };
        let mut s = Self {
            keys: ServiceKeys::generate(STARTED),
            http,
            dir,
            admin,
            names: Vec::new(),
            procs: Vec::new(),
            pa_url: format!("http://127.0.0.1:{pa_port}"),
            org_url: format!("http://127.0.0.1:{org_port}"),
            pa_db: String::new(),
        };
        if let Err(e) = s.boot(&bin, pa_port, org_port).await {
            s.dump_logs();
            s.close().await;
            return Err(e);
        }
        Ok(s)
    }

    async fn boot(
        &mut self,
        bin: &std::path::Path,
        pa_port: u16,
        org_port: u16,
    ) -> anyhow::Result<()> {
        let mut urls = Vec::new();
        for (name, migration) in [
            ("pa", "platform-access-migration"),
            ("org", "organizations-migration"),
        ] {
            let db_name = format!("orgcert_{}_{name}", Uuid::new_v4().simple());
            let url = db::create(&self.admin, &db_name).await?;
            self.names.push(db_name);
            db::migrate(&bin.join(migration), &url)?;
            urls.push(url);
        }
        self.pa_db = urls[0].clone();
        let mut env = self.auth_env(services::PLATFORM_ACCESS)?;
        env.extend([
            ("DATABASE_URL".into(), urls[0].clone()),
            (
                "PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS".into(),
                "5".into(),
            ),
            (
                "PLATFORM_ACCESS_BIND_ADDRESS".into(),
                format!("127.0.0.1:{pa_port}"),
            ),
            (
                "API_CREDENTIAL_PEPPER".into(),
                format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            ),
        ]);
        self.launch("platform-access", bin, env, &self.pa_url.clone())
            .await?;
        let mut env = self.auth_env(services::ORGANIZATIONS)?;
        env.extend([
            ("DATABASE_URL".into(), urls[1].clone()),
            ("ORGANIZATIONS_DATABASE_MAX_CONNECTIONS".into(), "5".into()),
            (
                "ORGANIZATIONS_BIND_ADDRESS".into(),
                format!("127.0.0.1:{org_port}"),
            ),
            ("ORGANIZATIONS_WORKER_INTERVAL_MS".into(), "1000".into()),
            ("ORGANIZATIONS_PROVISIONING_ENABLED".into(), "true".into()),
            ("CF_PLATFORM_ACCESS_BASE_URL".into(), self.pa_url.clone()),
        ]);
        self.launch("organizations", bin, env, &self.org_url.clone())
            .await?;
        Ok(())
    }

    async fn launch(
        &mut self,
        name: &str,
        bin: &std::path::Path,
        env: Vec<(String, String)>,
        base: &str,
    ) -> anyhow::Result<()> {
        let p = process::spawn(
            Launch {
                name,
                bin: &bin.join(name),
                env,
                dir: &self.dir,
                log_dir: &self.dir.join("logs"),
                ready: Some(format!("{base}/ready")),
            },
            &self.http,
        )
        .await?;
        self.procs.push(p);
        Ok(())
    }

    fn dump_logs(&self) {
        for p in &self.procs {
            eprintln!("{}: {}", p.name, process::tail(&p.log, 40));
        }
    }

    async fn close(mut self) {
        for p in &mut self.procs {
            let _ = p.child.kill().await;
        }
        db::drop_all(&self.admin, &self.names).await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// A signed Organizations call from `issuer` (the Meeting Service relays for `actor`).
    async fn org(
        &self,
        issuer: &str,
        actor: Option<Uuid>,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(u16, Value)> {
        let signed = self
            .keys
            .signer(issuer)
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::organizations(),
            ))?;
        let mut req = self.http.request(method, format!("{}{path}", self.org_url));
        for (k, v) in signed.headers().iter() {
            req = req.header(k.as_str(), v.to_str()?);
        }
        if let Some(a) = actor {
            req = req.header("x-cf-acting-user", a.to_string());
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await?;
        let status = r.status().as_u16();
        let v = r.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, v))
    }

    async fn provisioning(&self, org: &str) -> anyhow::Result<Value> {
        let (status, v) = self
            .org(
                services::MEETING_SERVICE,
                None,
                reqwest::Method::GET,
                &format!("/internal/v1/organizations/{org}/provisioning"),
                None,
            )
            .await?;
        anyhow::ensure!(status == 200, "provisioning read {status}: {v}");
        Ok(v)
    }

    async fn wait_for<F, Fut>(&self, what: &str, mut check: F) -> anyhow::Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<bool>>,
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if check().await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        anyhow::bail!("timed out waiting for {what}")
    }

    async fn pa_scalar(&self, sql: &str) -> anyhow::Result<i64> {
        let conn = db::connect(&self.pa_db).await?;
        let n = db::scalar_i64(&conn, sql).await;
        conn.close().await.ok();
        n
    }
}

async fn journey(s: &MiniStack) -> anyhow::Result<()> {
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    let mut orgs = Vec::new();
    for (owner, slug) in [(alice, "cert-org-alpha"), (bob, "cert-org-bravo")] {
        let (status, v) = s
            .org(
                services::MEETING_SERVICE,
                Some(owner),
                reqwest::Method::POST,
                "/internal/v1/organizations",
                Some(json!({ "name": slug, "slug": slug })),
            )
            .await?;
        anyhow::ensure!(status == 201, "create {status}: {v}");
        orgs.push((owner, v["id"].as_str().context("id")?.to_string()));
    }
    let mut triples = Vec::new();
    for (owner, org) in &orgs {
        s.wait_for("READY", move || async move {
            Ok(s.provisioning(org).await?["provisioningState"] == "READY")
        })
        .await?;
        let p = s.provisioning(org).await?;
        anyhow::ensure!(p["provisioningEnabled"] == true, "{p}");
        let (status, scope) = s
            .org(
                services::MEETING_SERVICE,
                Some(*owner),
                reqwest::Method::POST,
                "/internal/v1/organizations/scopes/resolve",
                Some(json!({ "organizationId": org })),
            )
            .await?;
        anyhow::ensure!(status == 200, "resolve {status}: {scope}");
        let (t, a, e) = (
            scope["tenantId"].as_str().context("tenant")?.to_string(),
            scope["applicationId"].as_str().context("app")?.to_string(),
            scope["environmentId"].as_str().context("env")?.to_string(),
        );
        // Platform Access's own records: a FIRST_PARTY tenant with the organization's
        // reserved slug owning this application and a PRODUCTION environment, no
        // credential and no scope grant.
        let n = s
            .pa_scalar(&format!(
                "SELECT count(*) FROM platform_organization_scopes s \
                 JOIN platform_tenants t ON t.id = s.tenant_id \
                 JOIN platform_environments e ON e.id = s.environment_id \
                 WHERE s.organization_ref = '{org}' AND s.tenant_id = '{t}' AND s.application_id = '{a}' \
                   AND s.environment_id = '{e}' AND t.classification = 'FIRST_PARTY' AND e.kind = 'PRODUCTION' \
                   AND t.slug = 'org-' || replace('{org}', '-', '')"
            ))
            .await?;
        anyhow::ensure!(
            n == 1,
            "Platform Access does not confirm the triple of {org}"
        );
        triples.push((t, a, e));
    }
    anyhow::ensure!(
        triples[0].0 != triples[1].0
            && triples[0].1 != triples[1].1
            && triples[0].2 != triples[1].2,
        "two organizations share a scope level"
    );
    anyhow::ensure!(
        s.pa_scalar("SELECT count(*) FROM platform_api_credentials")
            .await?
            == 0
            && s.pa_scalar("SELECT count(*) FROM platform_application_scopes")
                .await?
                == 0,
        "the provisioning path issued a credential or a scope grant"
    );
    // Suspension follows the tenant in Platform Access; scope resolution refuses at once.
    let (owner, org) = &orgs[0];
    let tenant = triples[0].0.as_str();
    let (status, v) = s
        .org(
            services::MEETING_SERVICE,
            Some(*owner),
            reqwest::Method::POST,
            &format!("/internal/v1/organizations/{org}/suspend"),
            Some(json!({})),
        )
        .await?;
    anyhow::ensure!(status == 200, "suspend {status}: {v}");
    let (status, _) = s
        .org(
            services::MEETING_SERVICE,
            None,
            reqwest::Method::POST,
            "/internal/v1/organizations/scopes/resolve",
            Some(json!({ "organizationId": org })),
        )
        .await?;
    anyhow::ensure!(
        status == 409,
        "a suspended organization's scope resolved ({status})"
    );
    s.wait_for("tenant SUSPENDED", move || async move {
        Ok(s.pa_scalar(&format!(
            "SELECT count(*) FROM platform_tenants WHERE id = '{tenant}' AND status = 'SUSPENDED'"
        ))
        .await?
            == 1)
    })
    .await?;
    // The other organization's tenant is untouched.
    let other = triples[1].0.as_str();
    anyhow::ensure!(
        s.pa_scalar(&format!(
            "SELECT count(*) FROM platform_tenants WHERE id = '{other}' AND status = 'ACTIVE'"
        ))
        .await?
            == 1,
        "suspension leaked to another organization"
    );
    let (status, v) = s
        .org(
            services::MEETING_SERVICE,
            Some(*owner),
            reqwest::Method::POST,
            &format!("/internal/v1/organizations/{org}/reactivate"),
            Some(json!({})),
        )
        .await?;
    anyhow::ensure!(status == 200, "reactivate {status}: {v}");
    s.wait_for("tenant ACTIVE", move || async move {
        Ok(s.pa_scalar(&format!(
            "SELECT count(*) FROM platform_tenants WHERE id = '{tenant}' AND status = 'ACTIVE'"
        ))
        .await?
            == 1)
    })
    .await?;
    s.wait_for("scope resolvable again", move || async move {
        let (status, _) = s
            .org(
                services::MEETING_SERVICE,
                None,
                reqwest::Method::POST,
                "/internal/v1/organizations/scopes/resolve",
                Some(json!({ "organizationId": org })),
            )
            .await?;
        Ok(status == 200)
    })
    .await?;
    // Platform Access audited every provisioning with the organization as an opaque
    // reference, under the Organizations identity.
    anyhow::ensure!(
        s.pa_scalar("SELECT count(*) FROM platform_access_audit WHERE action = 'ORGANIZATION_SCOPE_PROVISIONED' AND actor = 'cfmeet.organizations'").await? == 2,
        "provisioning audit"
    );
    Ok(())
}

#[tokio::test]
async fn organizations_provision_first_party_scopes_through_the_real_platform_access() {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let bin = var("CFMEET_BIN_DIR").map(PathBuf::from);
    let admin = var("TEST_DATABASE_URL");
    let missing = admin.is_none()
        || bin
            .as_ref()
            .is_none_or(|b| BINARIES.iter().any(|n| !b.join(n).is_file()));
    if missing {
        if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
            eprintln!("SKIPPED (CFMEET_SKIP_DB_TESTS=1): Organizations / Platform Access binaries or TEST_DATABASE_URL missing");
            return;
        }
        panic!("organizations certification needs TEST_DATABASE_URL and CFMEET_BIN_DIR with {BINARIES:?}");
    }
    let stack = MiniStack::start(admin.unwrap(), bin.unwrap())
        .await
        .expect("mini stack starts");
    let outcome = journey(&stack).await;
    if outcome.is_err() {
        stack.dump_logs();
    }
    stack.close().await;
    outcome.expect("organizations provisioning journey");
}

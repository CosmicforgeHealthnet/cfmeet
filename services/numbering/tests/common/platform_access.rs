//! A REAL Platform Access process for Numbering's customer-facing tests
//! (`PLATFORM_ACCESS_BIN`, migrated with `PLATFORM_ACCESS_MIGRATION_BIN`), with its own
//! database. Numbering reaches it exactly as in production — over internal service
//! authentication as `cfmeet.numbering` — and the tests administer identity through it
//! as the operator console. Numbering never reads its database.

use std::{path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};
use sea_orm::{ConnectionTrait, Database};
use serde_json::{json, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
/// Deterministic test-only pepper of the test Platform Access (never used outside tests).
const TEST_PEPPER: &str = "numbering-integration-test-pepper-0b7e4c2a91d35f68ae20c7";

pub struct Binaries {
    pub server: PathBuf,
    pub migration: PathBuf,
}

pub fn binaries() -> Option<Binaries> {
    match (
        std::env::var_os("PLATFORM_ACCESS_BIN"),
        std::env::var_os("PLATFORM_ACCESS_MIGRATION_BIN"),
    ) {
        (Some(server), Some(migration)) => Some(Binaries {
            server: server.into(),
            migration: migration.into(),
        }),
        _ if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") => None,
        _ => panic!(
            "PLATFORM_ACCESS_BIN and PLATFORM_ACCESS_MIGRATION_BIN are not set. Numbering \
             authenticates every customer credential through Platform Access: build it \
             (cd services/platform-access && cargo build --bins) and point the variables at \
             the binaries (`make numbering-test` does both)."
        ),
    }
}

static TEMPLATE: OnceCell<String> = OnceCell::const_new();

async fn template(admin: &str, bins: &Binaries) -> String {
    TEMPLATE
        .get_or_init(|| async {
            let name = format!(
                "nb_tmpl_{}_pa{}",
                std::process::id(),
                Uuid::new_v4().simple()
            );
            let conn = Database::connect(admin).await.expect("connect admin db");
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let out = std::process::Command::new(&bins.migration)
                .arg("up")
                .env("DATABASE_URL", super::with_db(admin, &name))
                .output()
                .expect("platform-access-migration runs");
            assert!(
                out.status.success(),
                "platform-access-migration up failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            name
        })
        .await
        .clone()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A running Platform Access with its own database.
pub struct PlatformAccess {
    child: std::sync::Mutex<Option<tokio::process::Child>>,
    pub base_url: String,
    db_name: String,
    admin_url: String,
    dir: PathBuf,
    numbering_pem: String,
    operator: Arc<ServiceTokenSigner>,
    http: reqwest::Client,
}

impl PlatformAccess {
    pub async fn start(admin_url: &str) -> Option<Self> {
        let bins = binaries()?;
        let tmpl = template(admin_url, &bins).await;
        let db_name = format!("nb_t_{}_pa{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = super::CREATE_LOCK.lock().await;
            let conn = Database::connect(admin_url).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {db_name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        let dir = std::env::temp_dir().join(format!("cf-nb-pa-test-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let pa_key = ServiceSigningKey::generate(KeyId::parse("platform-access-test-1").unwrap());
        let numbering_key =
            ServiceSigningKey::generate(KeyId::parse("numbering-pa-test-1").unwrap());
        let operator_key = ServiceSigningKey::generate(KeyId::parse("operator-test-1").unwrap());
        std::fs::write(dir.join("pa-key.pem"), pa_key.to_pkcs8_pem().as_bytes()).unwrap();
        // Platform Access's bundle: Numbering may authenticate credentials and resolve
        // identity; the operator console administers.
        std::fs::write(
            dir.join("pa-trust.json"),
            json!({
                "bundle_version": 1,
                "receiver": cf_service_auth::services::PLATFORM_ACCESS,
                "runtime_environment": ENV,
                "trusted_issuers": [
                    {
                        "issuer": cf_service_auth::services::NUMBERING,
                        "audience": cf_service_auth::services::PLATFORM_ACCESS,
                        "runtime_environment": ENV,
                        "keys": [numbering_key.public_jwk()],
                        "permissions": [
                            "platform-credential.authenticate",
                            "platform-identity.read",
                            "platform-principal.resolve",
                        ],
                    },
                    {
                        "issuer": cf_service_auth::services::OPERATOR_CONSOLE,
                        "audience": cf_service_auth::services::PLATFORM_ACCESS,
                        "runtime_environment": ENV,
                        "keys": [operator_key.public_jwk()],
                        "permissions": ["platform-identity.administer"],
                    },
                ],
            })
            .to_string(),
        )
        .unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut started = None;
        for _ in 0..5 {
            let port = free_port();
            let mut child = tokio::process::Command::new(&bins.server)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("DATABASE_URL", super::with_db(admin_url, &db_name))
                .env("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "3")
                .env("PLATFORM_ACCESS_BIND_ADDRESS", format!("127.0.0.1:{port}"))
                .env("API_CREDENTIAL_PEPPER", TEST_PEPPER)
                .env("CF_RUNTIME_ENVIRONMENT", "test")
                .env("CF_SERVICE_AUTH_KEY_ID", "platform-access-test-1")
                .env("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", dir.join("pa-key.pem"))
                .env("CF_SERVICE_TRUST_BUNDLE_FILE", dir.join("pa-trust.json"))
                .env("RUST_LOG", "platform_access=error")
                .kill_on_drop(true)
                .spawn()
                .expect("platform-access starts");
            let url = format!("http://127.0.0.1:{port}");
            let mut ready = false;
            for _ in 0..400 {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if let Ok(r) = http.get(format!("{url}/ready")).send().await {
                    if r.status().is_success() {
                        ready = true;
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            if ready {
                started = Some((child, url));
                break;
            }
            let _ = child.kill().await;
        }
        let (child, base_url) = started.expect("platform-access became ready");
        let operator = Arc::new(ServiceTokenSigner::new(
            ServiceId::operator_console(),
            ENV,
            operator_key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ));
        Some(Self {
            child: std::sync::Mutex::new(Some(child)),
            base_url,
            db_name,
            admin_url: admin_url.to_string(),
            dir,
            numbering_pem: numbering_key.to_pkcs8_pem().to_string(),
            operator,
            http,
        })
    }

    /// Numbering's signing key, as trusted by this Platform Access.
    pub fn numbering_key(&self) -> ServiceSigningKey {
        ServiceSigningKey::from_pkcs8_pem(
            KeyId::parse("numbering-pa-test-1").unwrap(),
            &self.numbering_pem,
        )
        .unwrap()
    }

    /// Files and variables for a Numbering PROCESS reaching this Platform Access.
    pub fn numbering_process_env(&self) -> Vec<(String, String)> {
        let key = self.dir.join("numbering-key.pem");
        let bundle = self.dir.join("numbering-trust.json");
        std::fs::write(&key, self.numbering_pem.as_bytes()).unwrap();
        std::fs::write(
            &bundle,
            super::trust_bundle_json(cf_service_auth::services::NUMBERING, &[]),
        )
        .unwrap();
        vec![
            ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
            (
                "CF_SERVICE_AUTH_KEY_ID".into(),
                "numbering-pa-test-1".into(),
            ),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                key.display().to_string(),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                bundle.display().to_string(),
            ),
            ("CF_PLATFORM_ACCESS_BASE_URL".into(), self.base_url.clone()),
        ]
    }

    /// Stops the process (outage tests).
    pub async fn stop(&self) {
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
    }

    /// A signed operator-console administration call.
    pub async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let signed = self
            .operator
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::platform_access(),
            ))
            .unwrap();
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .header("x-cf-operator", "numbering-tests");
        for (name, value) in signed.headers().iter() {
            request = request.header(name.as_str(), value.as_bytes());
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("platform access answers");
        let status = response.status().as_u16();
        (
            status,
            response.json::<Value>().await.unwrap_or(Value::Null),
        )
    }

    async fn created(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.admin(reqwest::Method::POST, path, Some(body)).await;
        assert!(
            (200..300).contains(&status),
            "POST {path}: HTTP {status} {v}"
        );
        v
    }

    /// A tenant → application (scopes granted) → environment of `kind` → service account,
    /// and a credential with `scopes`. Returns the raw credential and the true chain.
    pub async fn customer(&self, kind: &str, granted: &[&str], scopes: &[&str]) -> Customer {
        self.customer_as(kind, "EXTERNAL", granted, scopes).await
    }

    /// As [`Self::customer`] with a tenant classification; `numbers.admin` is granted by
    /// Platform Access's explicit privileged operator grant (FIRST_PARTY only).
    pub async fn customer_as(
        &self,
        kind: &str,
        classification: &str,
        granted: &[&str],
        scopes: &[&str],
    ) -> Customer {
        let slug = format!("t{}", &Uuid::new_v4().simple().to_string()[..12]);
        let tenant = self
            .created(
                "/internal/v1/admin/tenants",
                json!({ "slug": slug, "displayName": "numbering test tenant", "classification": classification }),
            )
            .await;
        let tenant_id = id(&tenant["id"]);
        let application = self
            .created(
                &format!("/internal/v1/admin/tenants/{tenant_id}/applications"),
                json!({ "slug": "app", "displayName": "numbering test application" }),
            )
            .await;
        let application_id = id(&application["id"]);
        for scope in granted {
            self.created(
                &format!("/internal/v1/admin/applications/{application_id}/scopes"),
                json!({ "scope": scope, "privileged": *scope == "numbers.admin" }),
            )
            .await;
        }
        let environment = self
            .created(
                &format!("/internal/v1/admin/applications/{application_id}/environments"),
                json!({ "kind": kind }),
            )
            .await;
        let environment_id = id(&environment["id"]);
        let sa = self
            .created(
                &format!("/internal/v1/admin/environments/{environment_id}/service-accounts"),
                json!({ "name": "backend", "description": "numbering test backend" }),
            )
            .await;
        let service_account_id = id(&sa["id"]);
        let issued = self
            .created(
                &format!("/internal/v1/admin/service-accounts/{service_account_id}/credentials"),
                json!({ "scopes": scopes, "label": "numbering-tests" }),
            )
            .await;
        Customer {
            secret: issued["secret"].as_str().unwrap().to_string(),
            credential_id: id(&issued["credential"]["id"]),
            tenant_id,
            application_id,
            environment_id,
            service_account_id,
        }
    }

    /// Registers a communication principal (an enterprise member) in an environment.
    pub async fn principal(&self, environment_id: Uuid) -> Uuid {
        let v = self
            .created(
                &format!("/internal/v1/admin/environments/{environment_id}/principals"),
                json!({
                    "kind": "ENTERPRISE_MEMBER",
                    "externalReference": format!("member-{}", Uuid::new_v4().simple()),
                }),
            )
            .await;
        id(&v["principal"]["id"])
    }

    /// Sets a principal's status (ACTIVE / SUSPENDED / DEACTIVATED).
    pub async fn set_principal_status(&self, principal: Uuid, status: &str) {
        self.created(
            &format!("/internal/v1/admin/principals/{principal}/status"),
            json!({ "status": status }),
        )
        .await;
    }

    pub async fn revoke(&self, credential: Uuid) {
        self.created(
            &format!("/internal/v1/admin/credentials/{credential}/revoke"),
            json!({}),
        )
        .await;
    }
}

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap_or_else(|| panic!("not an id: {v}"))).unwrap()
}

/// An authenticated customer of Platform Access.
#[derive(Debug, Clone)]
pub struct Customer {
    pub secret: String,
    pub credential_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
}

impl Drop for PlatformAccess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        let admin = self.admin_url.clone();
        let name = self.db_name.clone();
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

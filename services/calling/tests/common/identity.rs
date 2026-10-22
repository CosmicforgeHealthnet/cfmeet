//! The identity authority of Calling's integration tests: a REAL Platform Access process
//! (`PLATFORM_ACCESS_BIN`, migrated with `PLATFORM_ACCESS_MIGRATION_BIN`) per test
//! database, reached exactly as in production — Calling's [`HttpIdentityAuthority`] over
//! internal service authentication — plus an operator-console client for the
//! administration the tests perform (`TestPlatform`, the old `PlatformService` surface).
//!
//! Without the binaries the tests FAIL with instructions (`make calling-test` builds and
//! passes them), unless `CFMEET_SKIP_DB_TESTS=1`. Platform Access runs on the wall clock;
//! Calling's manual clock never decides a credential's validity any more.

use std::{ops::Deref, path::PathBuf, sync::Arc, time::Duration};

use calling::{
    domain::{
        ApplicationId, ApplicationStatus, CredentialId, EnvironmentId, EnvironmentKind,
        EnvironmentStatus, PrincipalId, PrincipalKind, PrincipalStatus, ServiceAccountId,
        ServiceAccountStatus, TenantClassification, TenantId, TenantStatus,
    },
    platform::{
        access::{HttpIdentityAuthority, IdentityAuthority},
        applications::ApplicationView,
        credentials::CredentialView,
        environments::EnvironmentView,
        principals::PrincipalView,
        projection::IdentityProjection,
        service_accounts::ServiceAccountView,
        tenants::TenantView,
        PlatformError, PlatformResult, PlatformService,
    },
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, Database};
use serde_json::{json, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// Deterministic test-only pepper of the test Platform Access (never used outside tests).
pub const TEST_PEPPER: &str = "calling-integration-test-pepper-6c1f0e9b2d8a4f7e93b5d02a";
/// The operator the tests administer identity as.
pub const TEST_OPERATOR: &str = "calling-tests";

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

pub struct Binaries {
    pub server: PathBuf,
    pub migration: PathBuf,
}

/// The Platform Access binaries, or a clear failure.
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
            "PLATFORM_ACCESS_BIN and PLATFORM_ACCESS_MIGRATION_BIN are not set. Calling \
             authenticates every credential through Platform Access: build it \
             (cd services/platform-access && cargo build --bins) and point the variables at \
             the binaries (`make calling-test` does both)."
        ),
    }
}

static TEMPLATE: OnceCell<String> = OnceCell::const_new();

async fn template(admin: &str, bins: &Binaries) -> String {
    TEMPLATE
        .get_or_init(|| async {
            let name = format!(
                "clg_tmpl_{}_pa{}",
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

/// Whether the Platform Access at `url` is the one trusting `operator`: an operator-signed
/// read of a tenant that does not exist is 404 there, and 401 at any other instance.
async fn answers_to(http: &reqwest::Client, url: &str, operator: &ServiceTokenSigner) -> bool {
    let signed = operator
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::platform_access(),
        ))
        .unwrap();
    let mut request = http
        .get(format!(
            "{url}/internal/v1/admin/tenants/{}",
            Uuid::new_v4()
        ))
        .header("x-cf-operator", TEST_OPERATOR);
    for (name, value) in signed.headers().iter() {
        request = request.header(name.as_str(), value.as_bytes());
    }
    matches!(request.send().await, Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND)
}

fn trust_bundle(calling: &ServiceSigningKey, operator: &ServiceSigningKey) -> String {
    super::trust_bundle_json(
        cf_service_auth::services::PLATFORM_ACCESS,
        ENV,
        &[
            (
                cf_service_auth::services::CALLING,
                vec![calling.public_jwk()],
                &[
                    "platform-credential.authenticate",
                    "platform-principal.resolve",
                    "platform-principal.register",
                    "platform-identity.read",
                ],
            ),
            (
                cf_service_auth::services::OPERATOR_CONSOLE,
                vec![operator.public_jwk()],
                &["platform-identity.administer"],
            ),
        ],
    )
}

/// A running Platform Access with its own database.
pub struct PlatformAccess {
    child: std::sync::Mutex<Option<tokio::process::Child>>,
    pub base_url: String,
    db_name: String,
    admin_url: String,
    dir: PathBuf,
    operator: Arc<ServiceTokenSigner>,
    http: reqwest::Client,
    /// Calling's authenticated client of this Platform Access (wall-clock tokens).
    pub calling_client: Arc<InternalServiceClient>,
}

impl PlatformAccess {
    pub async fn start(admin_url: &str) -> Option<Self> {
        Self::start_with(admin_url, &[]).await
    }

    /// A Platform Access with extra environment (e.g. its failed-authentication policy).
    pub async fn start_with(admin_url: &str, extra_env: &[(&str, &str)]) -> Option<Self> {
        Self::start_with_ports(admin_url, extra_env, &mut free_port).await
    }

    /// [`start_with`](Self::start_with) taking candidate ports from `next_port`. A probed
    /// free port can be taken by another test's process before this one binds it; then
    /// this process exits while `/ready` on that port is answered by the OTHER instance.
    /// So an instance counts as started only once it authenticates a request signed with
    /// THIS instance's operator key (another instance has its own trust bundle and answers
    /// 401); otherwise it is stopped and the next port is tried.
    pub async fn start_with_ports(
        admin_url: &str,
        extra_env: &[(&str, &str)],
        next_port: &mut (dyn FnMut() -> u16 + Send),
    ) -> Option<Self> {
        let bins = binaries()?;
        let tmpl = template(admin_url, &bins).await;
        let db_name = format!("clg_t_{}_pa{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = super::CREATE_LOCK.lock().await;
            let conn = Database::connect(admin_url).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {db_name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        let dir = std::env::temp_dir().join(format!("cf-pa-test-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let pa_key = ServiceSigningKey::generate(KeyId::parse("platform-access-test-1").unwrap());
        let calling_key = ServiceSigningKey::generate(KeyId::parse("calling-pa-test-1").unwrap());
        let operator_key = ServiceSigningKey::generate(KeyId::parse("operator-test-1").unwrap());
        std::fs::write(dir.join("pa-key.pem"), pa_key.to_pkcs8_pem().as_bytes()).unwrap();
        std::fs::write(
            dir.join("pa-trust.json"),
            trust_bundle(&calling_key, &operator_key),
        )
        .unwrap();
        // Calling's side, for Calling processes (the `platform_admin` CLI) that reach this
        // Platform Access: the same key, and a Calling trust bundle with no peers.
        std::fs::write(
            dir.join("calling-key.pem"),
            calling_key.to_pkcs8_pem().as_bytes(),
        )
        .unwrap();
        std::fs::write(
            dir.join("calling-trust.json"),
            super::trust_bundle_json(cf_service_auth::services::CALLING, ENV, &[]),
        )
        .unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut started = None;
        // A free port can be taken between probing and binding: retry on a new one.
        let operator = Arc::new(ServiceTokenSigner::new(
            ServiceId::operator_console(),
            ENV,
            operator_key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ));
        for _ in 0..5 {
            let port = next_port();
            let mut command = tokio::process::Command::new(&bins.server);
            command.env_clear();
            for (k, v) in extra_env {
                command.env(k, v);
            }
            let mut child = command
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("DATABASE_URL", super::with_db(admin_url, &db_name))
                .env("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "3")
                .env("PLATFORM_ACCESS_BIND_ADDRESS", format!("127.0.0.1:{port}"))
                .env("API_CREDENTIAL_PEPPER", TEST_PEPPER)
                // Platform Access issues on the wall clock while Calling's tests run a manual
                // clock in 2031: ten-year lifetimes keep credentials valid on both (Calling's
                // signaling path checks the minting credential's projected validity window).
                .env("CREDENTIAL_DEFAULT_LIFETIME_DAYS", "3650")
                .env("CREDENTIAL_MAX_LIFETIME_DAYS", "3650")
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
            if ready && answers_to(&http, &url, &operator).await {
                started = Some((child, url));
                break;
            }
            let _ = child.kill().await;
        }
        let (child, base_url) = started.expect("platform-access became ready");
        let clock: cf_service_auth::SharedClock = Arc::new(SystemClock);
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::calling(),
            ENV,
            calling_key,
            TokenTtl::default(),
            clock.clone(),
        ));
        let mut targets = InternalTargets::new(ENV);
        targets
            .insert(ServiceId::platform_access(), &base_url)
            .unwrap();
        let calling_client =
            Arc::new(InternalServiceClient::new(signer, targets, Duration::from_secs(10)).unwrap());
        Some(Self {
            child: std::sync::Mutex::new(Some(child)),
            base_url,
            db_name,
            admin_url: admin_url.to_string(),
            dir,
            operator,
            http,
            calling_client,
        })
    }

    /// Environment for a Calling process (CLI) to reach this Platform Access as Calling.
    pub fn calling_process_env(&self) -> Vec<(String, String)> {
        vec![
            ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
            ("CF_SERVICE_AUTH_KEY_ID".into(), "calling-pa-test-1".into()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.dir.join("calling-key.pem").display().to_string(),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.dir.join("calling-trust.json").display().to_string(),
            ),
            ("CF_PLATFORM_ACCESS_BASE_URL".into(), self.base_url.clone()),
        ]
    }

    /// Platform Access's OWN database (for its operator CLI in hand-off tests).
    pub fn database_url(&self) -> String {
        super::with_db(&self.admin_url, &self.db_name)
    }

    /// The `platform-access-admin` operator CLI next to the server binary.
    pub fn admin_bin() -> PathBuf {
        let server = binaries().expect("platform access binaries").server;
        server.with_file_name("platform-access-admin")
    }

    /// One integer from Platform Access's OWN database (assertions only).
    pub async fn scalar_i64(&self, sql: &str) -> i64 {
        let conn = Database::connect(super::with_db(&self.admin_url, &self.db_name))
            .await
            .unwrap();
        let value = super::scalar_i64(&conn, sql).await;
        conn.close().await.ok();
        value
    }

    /// Stops the process (outage tests); the database stays.
    pub async fn stop(&self) {
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
    }

    /// Calling's identity authority adapter for this Platform Access.
    pub fn authority(&self) -> Arc<dyn IdentityAuthority> {
        Arc::new(HttpIdentityAuthority::new(self.calling_client.clone()))
    }

    /// The scope names registered by Platform Access (as Calling reads them).
    pub async fn registered_scopes(&self) -> Vec<String> {
        let response = self
            .calling_client
            .get(
                &OutboundServiceRequest::new_operation(ServiceId::platform_access()),
                "/internal/v1/scopes",
            )
            .await
            .expect("platform access answers");
        assert_eq!(response.status(), 200);
        let body: Value = response.json().unwrap();
        body["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect()
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
            .header("x-cf-operator", TEST_OPERATOR);
        for (name, value) in signed.headers().iter() {
            request = request.header(name.as_str(), value.as_bytes());
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("platform access answers");
        let status = response.status().as_u16();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        (status, body)
    }
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

// ---------------------------------------------------------------------------
// Administration requests (the shapes the tests build).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewTenant {
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    pub billing_currency: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewApplication {
    pub slug: String,
    pub display_name: String,
}

#[derive(Debug, Clone)]
pub struct NewServiceAccount {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IssueCredential {
    pub scopes: Vec<String>,
    pub label: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct RotateCredential {
    pub overlap: Option<chrono::Duration>,
    pub scopes: Option<Vec<String>>,
    pub label: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewPrincipal {
    External {
        kind: PrincipalKind,
        external_reference: String,
    },
    ServiceAccount(ServiceAccountId),
}

/// The raw credential, shown once by Platform Access (redacted Debug).
pub struct IssuedSecret(String);

impl IssuedSecret {
    pub fn expose_once(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for IssuedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IssuedSecret([REDACTED])")
    }
}

#[derive(Debug)]
pub struct IssuedCredential {
    pub credential: CredentialView,
    pub secret: IssuedSecret,
}

fn uuid_of(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap_or_else(|| panic!("not an id: {v}"))).unwrap()
}

fn to_error(status: u16, body: &Value) -> PlatformError {
    let code = body["error"]["code"].as_str().unwrap_or("");
    let message = body["error"]["message"].as_str().unwrap_or("").to_string();
    match code {
        "VALIDATION_ERROR" => PlatformError::Validation(message),
        "NOT_FOUND" => PlatformError::NotFound("resource"),
        "CONFLICT" => PlatformError::Conflict(message),
        "POLICY_VIOLATION" => PlatformError::PolicyViolation(message),
        "INVALID_STATE" => PlatformError::InvalidState(message),
        _ => PlatformError::Database {
            summary: format!("platform access answered HTTP {status} {code}"),
            connectivity: false,
        },
    }
}

/// The administration surface the tests use: identity changes go to Platform Access (and
/// the projection catches up before returning, as the sync worker would); everything
/// else — Calling-owned limits and reads of the projection — is Calling's own
/// [`PlatformService`] (`Deref`).
#[derive(Clone)]
pub struct TestPlatform {
    pub pa: Arc<PlatformAccess>,
    local: PlatformService,
    identity: Arc<IdentityProjection>,
}

impl Deref for TestPlatform {
    type Target = PlatformService;
    fn deref(&self) -> &PlatformService {
        &self.local
    }
}

impl TestPlatform {
    pub fn new(
        pa: Arc<PlatformAccess>,
        local: PlatformService,
        identity: Arc<IdentityProjection>,
    ) -> Self {
        Self {
            pa,
            local,
            identity,
        }
    }

    async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> PlatformResult<Value> {
        let (status, body) = self.pa.admin(method, path, body).await;
        if !(200..300).contains(&status) {
            return Err(to_error(status, &body));
        }
        self.sync().await;
        Ok(body)
    }

    /// Applies the identity change feed to Calling's projection now.
    pub async fn sync(&self) {
        self.identity
            .sync()
            .await
            .expect("identity projection syncs");
    }

    pub async fn create_tenant(&self, new: NewTenant) -> PlatformResult<TenantView> {
        let mut body = json!({
            "slug": new.slug,
            "displayName": new.display_name,
            "classification": new.classification.as_str(),
        });
        if let Some(c) = new.billing_currency {
            body["billingCurrency"] = json!(c);
        }
        let v = self
            .admin(
                reqwest::Method::POST,
                "/internal/v1/admin/tenants",
                Some(body),
            )
            .await?;
        self.local
            .tenant(TenantId::from_uuid(uuid_of(&v["id"])))
            .await
    }

    pub async fn create_application(
        &self,
        tenant: TenantId,
        new: NewApplication,
    ) -> PlatformResult<ApplicationView> {
        let v = self
            .admin(
                reqwest::Method::POST,
                &format!("/internal/v1/admin/tenants/{tenant}/applications"),
                Some(json!({ "slug": new.slug, "displayName": new.display_name })),
            )
            .await?;
        self.local
            .application(ApplicationId::from_uuid(uuid_of(&v["id"])))
            .await
    }

    pub async fn grant_application_scope(
        &self,
        application: ApplicationId,
        scope: &str,
    ) -> PlatformResult<()> {
        self.admin(
            reqwest::Method::POST,
            &format!("/internal/v1/admin/applications/{application}/scopes"),
            Some(json!({ "scope": scope })),
        )
        .await
        .map(|_| ())
    }

    pub async fn revoke_application_scope(
        &self,
        application: ApplicationId,
        scope: &str,
    ) -> PlatformResult<bool> {
        let v = self
            .admin(
                reqwest::Method::DELETE,
                &format!("/internal/v1/admin/applications/{application}/scopes/{scope}"),
                None,
            )
            .await?;
        Ok(v["removed"].as_bool().unwrap_or(false))
    }

    pub async fn create_environment(
        &self,
        application: ApplicationId,
        kind: EnvironmentKind,
    ) -> PlatformResult<EnvironmentView> {
        let v = self
            .admin(
                reqwest::Method::POST,
                &format!("/internal/v1/admin/applications/{application}/environments"),
                Some(json!({ "kind": kind.as_str() })),
            )
            .await?;
        self.local
            .environment(EnvironmentId::from_uuid(uuid_of(&v["id"])))
            .await
    }

    pub async fn create_service_account(
        &self,
        environment: EnvironmentId,
        new: NewServiceAccount,
    ) -> PlatformResult<ServiceAccountView> {
        let v = self
            .admin(
                reqwest::Method::POST,
                &format!("/internal/v1/admin/environments/{environment}/service-accounts"),
                Some(json!({ "name": new.name, "description": new.description })),
            )
            .await?;
        self.local
            .service_account(ServiceAccountId::from_uuid(uuid_of(&v["id"])))
            .await
    }

    async fn issued(&self, v: Value) -> PlatformResult<IssuedCredential> {
        let id = CredentialId::from_uuid(uuid_of(&v["credential"]["id"]));
        Ok(IssuedCredential {
            credential: self.local.credential(id).await?,
            secret: IssuedSecret(v["secret"].as_str().unwrap().to_string()),
        })
    }

    pub async fn issue_credential(
        &self,
        sa: ServiceAccountId,
        request: IssueCredential,
    ) -> PlatformResult<IssuedCredential> {
        let mut body = json!({ "scopes": request.scopes, "label": request.label });
        if let Some(t) = request.not_before {
            body["notBefore"] = json!(t);
        }
        if let Some(t) = request.expires_at {
            body["expiresAt"] = json!(t);
        }
        let v = self
            .admin(
                reqwest::Method::POST,
                &format!("/internal/v1/admin/service-accounts/{sa}/credentials"),
                Some(body),
            )
            .await?;
        self.issued(v).await
    }

    pub async fn rotate_credential(
        &self,
        id: CredentialId,
        request: RotateCredential,
    ) -> PlatformResult<IssuedCredential> {
        let mut body = json!({ "scopes": request.scopes, "label": request.label });
        if let Some(o) = request.overlap {
            body["overlapMinutes"] = json!(o.num_minutes());
        }
        if let Some(t) = request.expires_at {
            body["expiresAt"] = json!(t);
        }
        let v = self
            .admin(
                reqwest::Method::POST,
                &format!("/internal/v1/admin/credentials/{id}/rotate"),
                Some(body),
            )
            .await?;
        self.issued(v).await
    }

    pub async fn revoke_credential(&self, id: CredentialId) -> PlatformResult<CredentialView> {
        self.admin(
            reqwest::Method::POST,
            &format!("/internal/v1/admin/credentials/{id}/revoke"),
            None,
        )
        .await?;
        self.local.credential(id).await
    }

    pub async fn register_principal(
        &self,
        environment: EnvironmentId,
        new: NewPrincipal,
    ) -> PlatformResult<(PrincipalView, bool)> {
        let body = match new {
            NewPrincipal::External {
                kind,
                external_reference,
            } => json!({ "kind": kind.as_str(), "externalReference": external_reference }),
            NewPrincipal::ServiceAccount(sa) => json!({ "serviceAccountId": sa }),
        };
        let v = self
            .admin(
                reqwest::Method::POST,
                &format!("/internal/v1/admin/environments/{environment}/principals"),
                Some(body),
            )
            .await?;
        let principal = self
            .local
            .principal(PrincipalId::from_uuid(uuid_of(&v["principal"]["id"])))
            .await?;
        Ok((principal, v["created"].as_bool().unwrap_or(false)))
    }

    async fn status(&self, path: String, status: &str) -> PlatformResult<()> {
        self.admin(
            reqwest::Method::POST,
            &path,
            Some(json!({ "status": status })),
        )
        .await
        .map(|_| ())
    }

    pub async fn set_tenant_status(
        &self,
        id: TenantId,
        status: TenantStatus,
    ) -> PlatformResult<TenantView> {
        self.status(
            format!("/internal/v1/admin/tenants/{id}/status"),
            status.as_str(),
        )
        .await?;
        self.local.tenant(id).await
    }

    pub async fn set_application_status(
        &self,
        id: ApplicationId,
        status: ApplicationStatus,
    ) -> PlatformResult<ApplicationView> {
        self.status(
            format!("/internal/v1/admin/applications/{id}/status"),
            status.as_str(),
        )
        .await?;
        self.local.application(id).await
    }

    pub async fn set_environment_status(
        &self,
        id: EnvironmentId,
        status: EnvironmentStatus,
    ) -> PlatformResult<EnvironmentView> {
        self.status(
            format!("/internal/v1/admin/environments/{id}/status"),
            status.as_str(),
        )
        .await?;
        self.local.environment(id).await
    }

    pub async fn set_service_account_status(
        &self,
        id: ServiceAccountId,
        status: ServiceAccountStatus,
    ) -> PlatformResult<ServiceAccountView> {
        self.status(
            format!("/internal/v1/admin/service-accounts/{id}/status"),
            status.as_str(),
        )
        .await?;
        self.local.service_account(id).await
    }

    pub async fn set_principal_status(
        &self,
        id: PrincipalId,
        status: PrincipalStatus,
    ) -> PlatformResult<PrincipalView> {
        self.status(
            format!("/internal/v1/admin/principals/{id}/status"),
            status.as_str(),
        )
        .await?;
        self.local.principal(id).await
    }

    pub async fn set_live_telecom_allowed(
        &self,
        id: EnvironmentId,
        allowed: bool,
    ) -> PlatformResult<EnvironmentView> {
        self.admin(
            reqwest::Method::POST,
            &format!("/internal/v1/admin/environments/{id}/live-telecom"),
            Some(json!({ "allowed": allowed })),
        )
        .await?;
        self.local.environment(id).await
    }
}

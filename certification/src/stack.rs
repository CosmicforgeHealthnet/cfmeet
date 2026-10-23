//! The certification stack: every Part VI service as a real process.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context};
use base64::Engine;
use cf_service_auth::{services, RuntimeEnvironment};
use rand::{distributions::Alphanumeric, Rng, RngCore};
use uuid::Uuid;

use crate::{
    db,
    keys::{self, ServiceKeys},
    process::{self, Launch, Service},
    provision::{Gateway, Operator},
};

/// Service names (map keys of [`Stack::urls`]) and their binaries.
pub const PLATFORM_ACCESS: &str = "platform-access";
pub const DEVELOPER_PLATFORM: &str = "developer-platform";
pub const DEVELOPER_PLATFORM_WORKER: &str = "developer-platform-worker";
pub const MEETING_SERVICE: &str = "meeting-service";
pub const CALLING: &str = "calling";
pub const MESSAGING: &str = "messaging";
pub const NUMBERING: &str = "numbering";
pub const SMS_GATEWAY: &str = "sms-gateway";
pub const PRIVACY_RELAY: &str = "privacy-relay";
pub const API_GATEWAY: &str = "api-gateway";

/// Every binary the stack runs (server, then migration), as built by
/// `cargo build --workspace --bins` in each service workspace.
pub const BINARIES: &[&str] = &[
    "platform-access",
    "platform-access-migration",
    "developer-platform",
    "developer-platform-worker",
    "developer-platform-migration",
    "backend",
    "migration",
    "calling",
    "calling-migration",
    "messaging",
    "messaging-migration",
    "numbering",
    "numbering-migration",
    "sms-gateway",
    "sms-gateway-migration",
    "privacy-relay",
    "privacy-relay-migration",
    "api-gateway",
];

/// Numbering's own number-search limit per application environment in the stack.
pub const NUMBER_SEARCHES_PER_MINUTE: u32 = 5;

/// Where the stack gets its infrastructure and binaries.
#[derive(Debug, Clone)]
pub struct StackConfig {
    pub bin_dir: PathBuf,
    pub admin_url: String,
    pub redis_url: String,
    /// Keep the work directory (keys, bundles, logs) after teardown.
    pub keep_dir: bool,
}

impl StackConfig {
    /// From `TEST_DATABASE_URL`, `TEST_REDIS_URL` and `CFMEET_BIN_DIR` (default
    /// `$CARGO_TARGET_DIR/debug`, else `<repo>/target/debug`).
    ///
    /// `Ok(None)`: prerequisites are missing and `CFMEET_SKIP_DB_TESTS=1` explicitly skips.
    /// Missing prerequisites without that variable are an ERROR (certification never
    /// passes vacuously).
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let bin_dir = var("CFMEET_BIN_DIR")
            .map(PathBuf::from)
            .or_else(|| var("CARGO_TARGET_DIR").map(|d| PathBuf::from(d).join("debug")))
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("target")
                    .join("debug")
            });
        let mut missing: Vec<String> = Vec::new();
        let admin_url = var("TEST_DATABASE_URL");
        if admin_url.is_none() {
            missing.push("TEST_DATABASE_URL".into());
        }
        let redis_url = var("TEST_REDIS_URL");
        if redis_url.is_none() {
            missing.push("TEST_REDIS_URL".into());
        }
        for b in BINARIES {
            if !bin_dir.join(b).is_file() {
                missing.push(format!("{}", bin_dir.join(b).display()));
            }
        }
        if !missing.is_empty() {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!(
                    "SKIPPED (CFMEET_SKIP_DB_TESTS=1): certification prerequisites missing: {}",
                    missing.join(", ")
                );
                return Ok(None);
            }
            bail!(
                "certification prerequisites are missing: {}.\n`make certification` builds every \
                 service binary (CFMEET_BIN_DIR / CARGO_TARGET_DIR) and needs TEST_DATABASE_URL and \
                 TEST_REDIS_URL; set CFMEET_SKIP_DB_TESTS=1 to skip explicitly",
                missing.join(", ")
            );
        }
        Ok(Some(Self {
            bin_dir,
            admin_url: admin_url.unwrap_or_default(),
            redis_url: redis_url.unwrap_or_default(),
            keep_dir: std::env::var("CFMEET_KEEP_STACK_DIR").as_deref() == Ok("1"),
        }))
    }
}

/// A free local port that no other service of this test process was given. A stack
/// chooses every port before any service starts and releases each at once, so the kernel
/// can offer the same port twice (within one stack or across journeys); a duplicate let
/// one service answer for another, its readiness probe included.
fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<std::collections::BTreeSet<u16>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    let mut given = GIVEN.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("a free local port");
        if given.insert(port) {
            return port;
        }
    }
}

fn random_text(n: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(n)
        .map(char::from)
        .collect()
}

fn random_b64(standard: bool) -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    if standard {
        base64::engine::general_purpose::STANDARD.encode(b)
    } else {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }
}

/// How a stack deviates from the default certification stack (additive; the default is
/// exactly what [`Stack::start`] runs).
#[derive(Debug, Clone, Default)]
pub struct StackOptions {
    /// The runtime environment of EVERY service (keys, trust bundles, the operator console
    /// and `CF_RUNTIME_ENVIRONMENT`); default [`keys::RUNTIME`] (`test`). The webhook
    /// journeys run a `local` stack: only there may a deployment deliver to a loopback
    /// receiver (`*_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` is refused in every other runtime).
    pub runtime: Option<RuntimeEnvironment>,
    /// Extra variables per process (keyed by the names of [`Stack::urls`] plus
    /// [`DEVELOPER_PLATFORM_WORKER`]), applied after the stack's own variables (a variable
    /// set here replaces the stack's value).
    pub env: BTreeMap<String, Vec<(String, String)>>,
}

impl StackOptions {
    /// Adds `key=value` to the environment of `process`.
    pub fn set(mut self, process: &str, key: &str, value: impl Into<String>) -> Self {
        self.env
            .entry(process.to_string())
            .or_default()
            .push((key.to_string(), value.into()));
        self
    }
}

/// What a process was started with (for [`Stack::restart`]).
#[derive(Debug, Clone)]
struct LaunchSpec {
    bin: String,
    env: Vec<(String, String)>,
    ready: Option<String>,
}

/// Replaces (or adds) `key` in `env`.
fn put(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    env.retain(|(k, _)| k != key);
    env.push((key.to_string(), value.to_string()));
}

/// Secrets the stack generated (test material, never reused across stacks).
#[derive(Debug, Clone)]
pub struct Secrets {
    pub credential_pepper: String,
    pub meeting_jwt_secret: String,
}

/// A running certification stack. Dropping it kills every process, drops every database
/// and removes the work directory (kept, with logs printed, when a test is panicking).
pub struct Stack {
    pub id: String,
    pub dir: PathBuf,
    pub config: StackConfig,
    pub keys: ServiceKeys,
    pub secrets: Secrets,
    /// Service name -> base URL.
    pub urls: BTreeMap<String, String>,
    /// Service name -> database URL (evidence probes only; services never share one).
    pub db_urls: BTreeMap<String, String>,
    pub http: reqwest::Client,
    /// The runtime environment every service of this stack runs in.
    pub runtime: RuntimeEnvironment,
    options: StackOptions,
    specs: BTreeMap<String, LaunchSpec>,
    dbs: Vec<String>,
    procs: Vec<Service>,
    torn_down: bool,
}

/// The service identities started by every stack.
pub const STARTED: &[&str] = &[
    services::PLATFORM_ACCESS,
    services::DEVELOPER_PLATFORM,
    services::API_GATEWAY,
    services::MEETING_SERVICE,
    services::CALLING,
    services::MESSAGING,
    services::NUMBERING,
    services::SMS_GATEWAY,
    services::PRIVACY_RELAY,
];

impl Stack {
    /// Starts the whole stack (about half a minute, dominated by migrations).
    pub async fn start(config: StackConfig) -> anyhow::Result<Stack> {
        Self::start_with(config, StackOptions::default()).await
    }

    /// [`Stack::start`] with [`StackOptions`] (another runtime, extra variables).
    pub async fn start_with(config: StackConfig, options: StackOptions) -> anyhow::Result<Stack> {
        let id = Uuid::new_v4().simple().to_string()[..12].to_string();
        let dir = std::env::temp_dir().join(format!("cfmeet-cert-{id}"));
        std::fs::create_dir_all(dir.join("logs"))?;
        crate::redis::ping(&config.redis_url)
            .await
            .context("TEST_REDIS_URL is not reachable")?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()?;
        let mut stack = Stack {
            id,
            dir,
            keys: ServiceKeys::generate(STARTED),
            secrets: Secrets {
                credential_pepper: random_text(48),
                meeting_jwt_secret: random_text(64),
            },
            urls: BTreeMap::new(),
            db_urls: BTreeMap::new(),
            http,
            runtime: options.runtime.unwrap_or(keys::RUNTIME),
            options,
            specs: BTreeMap::new(),
            dbs: Vec::new(),
            procs: Vec::new(),
            torn_down: false,
            config,
        };
        if let Err(e) = stack.boot().await {
            let logs = stack.dir.join("logs");
            // Keep the logs of a failed startup for inspection.
            stack.config.keep_dir = true;
            stack.shutdown().await;
            return Err(e.context(format!("stack startup failed (logs in {})", logs.display())));
        }
        Ok(stack)
    }

    fn write(&self, name: &str, contents: &str) -> anyhow::Result<String> {
        let p = self.dir.join(name);
        std::fs::write(&p, contents)?;
        Ok(p.display().to_string())
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.config.bin_dir.join(name)
    }

    async fn database(&mut self, service: &str, migration: &str) -> anyhow::Result<String> {
        let name = format!("cert_{}_{}", service.replace('-', "_"), self.id);
        let url = db::create(&self.config.admin_url, &name).await?;
        self.dbs.push(name);
        db::migrate(&self.bin(migration), &url)?;
        self.db_urls.insert(service.to_string(), url.clone());
        Ok(url)
    }

    /// `CF_*` service-auth variables of `identity` (key file + derived trust bundle).
    fn service_auth_env(
        &self,
        identity: &str,
        with_bundle: bool,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let short = keys::short(identity);
        let mut env = vec![
            (
                "CF_RUNTIME_ENVIRONMENT".to_string(),
                self.runtime.as_str().to_string(),
            ),
            ("CF_SERVICE_AUTH_KEY_ID".to_string(), keys::kid(identity)),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".to_string(),
                self.write(&format!("{short}.pem"), &self.keys.pem(identity))?,
            ),
        ];
        if with_bundle {
            let bundle = keys::trust_bundle_in(identity, STARTED, &self.keys, self.runtime);
            env.push((
                "CF_SERVICE_TRUST_BUNDLE_FILE".to_string(),
                self.write(&format!("{short}-trust.json"), &bundle.to_string())?,
            ));
        }
        Ok(env)
    }

    async fn launch(
        &mut self,
        name: &str,
        bin: &str,
        mut env: Vec<(String, String)>,
        ready: Option<String>,
    ) -> anyhow::Result<()> {
        for (k, v) in self.options.env.get(name).cloned().unwrap_or_default() {
            put(&mut env, &k, &v);
        }
        let spec = LaunchSpec {
            bin: bin.to_string(),
            env,
            ready,
        };
        self.specs.insert(name.to_string(), spec.clone());
        self.spawn_spec(name, &spec).await
    }

    async fn spawn_spec(&mut self, name: &str, spec: &LaunchSpec) -> anyhow::Result<()> {
        let bin = self.bin(&spec.bin);
        let log_dir = self.dir.join("logs");
        let svc = process::spawn(
            Launch {
                name,
                bin: &bin,
                env: spec.env.clone(),
                dir: &self.dir,
                log_dir: &log_dir,
                ready: spec.ready.clone(),
            },
            &self.http,
        )
        .await?;
        self.procs.push(svc);
        Ok(())
    }

    /// Stops `process` (a name of [`Stack::urls`] or [`DEVELOPER_PLATFORM_WORKER`]) and
    /// starts it again — same binary, port, database and keys — with `overrides` replacing
    /// (or adding) variables; they stay in effect for later restarts. Fails (with the log)
    /// when the process refuses to start: it then stays stopped.
    pub async fn restart(
        &mut self,
        process: &str,
        overrides: &[(&str, &str)],
    ) -> anyhow::Result<()> {
        let mut spec = self
            .specs
            .get(process)
            .cloned()
            .with_context(|| format!("unknown process {process}"))?;
        for (k, v) in overrides {
            put(&mut spec.env, k, v);
        }
        if let Some(i) = self.procs.iter().position(|p| p.name == process) {
            let mut old = self.procs.remove(i);
            let _ = old.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(10), old.child.wait()).await;
            // Keep the previous log next to the new one.
            let _ = std::fs::rename(&old.log, old.log.with_extension("previous.log"));
        }
        self.specs.insert(process.to_string(), spec.clone());
        self.spawn_spec(process, &spec).await
    }

    async fn boot(&mut self) -> anyhow::Result<()> {
        // Ports first: every service learns its peers' URLs up front.
        for name in [
            PLATFORM_ACCESS,
            DEVELOPER_PLATFORM,
            MEETING_SERVICE,
            CALLING,
            MESSAGING,
            NUMBERING,
            SMS_GATEWAY,
            PRIVACY_RELAY,
            API_GATEWAY,
        ] {
            self.urls.insert(
                name.to_string(),
                format!("http://127.0.0.1:{}", free_port()),
            );
        }
        let url = |s: &Self, n: &str| s.urls[n].clone();
        let addr = |s: &Self, n: &str| s.urls[n].trim_start_matches("http://").to_string();
        let kv = |k: &str, v: String| (k.to_string(), v);

        // Platform Access: the identity authority.
        let db_url = self
            .database(PLATFORM_ACCESS, "platform-access-migration")
            .await?;
        let mut env = self.service_auth_env(services::PLATFORM_ACCESS, true)?;
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "5".into()),
            kv("PLATFORM_ACCESS_BIND_ADDRESS", addr(self, PLATFORM_ACCESS)),
            kv(
                "API_CREDENTIAL_PEPPER",
                self.secrets.credential_pepper.clone(),
            ),
        ]);
        let ready = format!("{}/ready", url(self, PLATFORM_ACCESS));
        self.launch(PLATFORM_ACCESS, "platform-access", env, Some(ready))
            .await?;

        // Meeting Service: CF Meet accounts / sessions and the Developer Console session
        // bridge. No RTC provider control (no LiveKit is contacted), no SMTP.
        let db_url = self.database(MEETING_SERVICE, "migration").await?;
        let mut env = self.service_auth_env(services::MEETING_SERVICE, true)?;
        let port = addr(self, MEETING_SERVICE)
            .rsplit_once(':')
            .map(|(_, p)| p.to_string())
            .unwrap_or_default();
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("HOST", "127.0.0.1".into()),
            kv("PORT", port),
            kv("APP_ENV", "test".into()),
            kv("APP_URL", "http://127.0.0.1:3000".into()),
            kv("FRONTEND_URLS", "http://127.0.0.1:3000".into()),
            kv("JWT_SECRET", self.secrets.meeting_jwt_secret.clone()),
            kv("REDIS_URL", self.config.redis_url.clone()),
            // Configuration only: FEATURE_RTC_PROVIDER_CONTROL_V2 is off, nothing dials it.
            kv("LIVEKIT_URL", "ws://127.0.0.1:9".into()),
            kv("LIVEKIT_API_KEY", "cert-livekit-unused".into()),
            kv("LIVEKIT_API_SECRET", random_text(40)),
            // Google sign-in is configured but never used (accounts are local).
            kv("GOOGLE_CLIENT_ID", "cert-google-client-unused".into()),
            kv("GOOGLE_CLIENT_SECRET", random_text(24)),
            kv(
                "GOOGLE_REDIRECT_URL",
                "http://127.0.0.1:3000/oauth/google/callback".into(),
            ),
            kv("EMAIL_VERIFICATION_REQUIRED", "true".into()),
            kv("FEATURE_MEETING_V2_ENABLED", "true".into()),
            kv("FEATURE_CONFERENCE_REALTIME_ENABLED", "true".into()),
            kv("DEVELOPER_SESSION_BRIDGE_ENABLED", "true".into()),
            // Part VI §51: gateway-forwarded platform credentials (re-verified live by
            // Platform Access). Public meeting events stay off (`none`, the default)
            // unless a journey sets MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION.
            kv("MEETING_PLATFORM_ACCESS_ENABLED", "true".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("MEETING_SERVICE_PUBLIC_EVENTS_INTERVAL_MS", "200".into()),
        ]);
        let ready = format!("{}/health", url(self, MEETING_SERVICE));
        self.launch(MEETING_SERVICE, "backend", env, Some(ready))
            .await?;

        // Developer Platform API + worker (same identity, same database).
        let db_url = self
            .database(DEVELOPER_PLATFORM, "developer-platform-migration")
            .await?;
        let mut dp_env = self.service_auth_env(services::DEVELOPER_PLATFORM, true)?;
        dp_env.extend([
            kv("DATABASE_URL", db_url),
            kv("DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS", "4".into()),
            kv("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true".into()),
            kv("DEVELOPER_PLATFORM_WORKER_INTERVAL_MS", "500".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv("CF_MEETING_SERVICE_BASE_URL", url(self, MEETING_SERVICE)),
            kv("CF_CALLING_BASE_URL", url(self, CALLING)),
        ]);
        let mut env = dp_env.clone();
        env.push(kv(
            "DEVELOPER_PLATFORM_BIND_ADDRESS",
            addr(self, DEVELOPER_PLATFORM),
        ));
        let ready = format!("{}/ready", url(self, DEVELOPER_PLATFORM));
        self.launch(DEVELOPER_PLATFORM, "developer-platform", env, Some(ready))
            .await?;
        let mut env = dp_env;
        env.push(kv(
            "DEVELOPER_PLATFORM_WORKER_INSTANCE_ID",
            "cert-worker-1".into(),
        ));
        env.push(kv(
            "DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS",
            "2".into(),
        ));
        self.launch(
            DEVELOPER_PLATFORM_WORKER,
            "developer-platform-worker",
            env,
            None,
        )
        .await?;

        // Numbering: synthetic provider and deterministic KYC only.
        let db_url = self.database(NUMBERING, "numbering-migration").await?;
        let grant_key = cf_service_auth::ServiceSigningKey::generate(
            cf_service_auth::KeyId::parse("numbering-grant-cert-1").expect("kid"),
        );
        let mut env = self.service_auth_env(services::NUMBERING, true)?;
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("NUMBERING_DATABASE_MAX_CONNECTIONS", "4".into()),
            kv("NUMBERING_BIND_ADDRESS", addr(self, NUMBERING)),
            kv("NUMBERING_WORKER_INTERVAL_MS", "200".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("NUMBERING_PROVIDERS", "synthetic".into()),
            kv(
                "NUMBERING_SYNTHETIC_PROVIDER_CALLBACK_SECRET",
                random_text(48),
            ),
            kv("NUMBERING_KYC_VERIFIER", "deterministic".into()),
            // A small domain-owned limit, so journeys can show it applying independently
            // of (and below) the gateway's platform quota.
            kv(
                "NUMBERING_SEARCH_REQUESTS_PER_MINUTE",
                NUMBER_SEARCHES_PER_MINUTE.to_string(),
            ),
            kv("NUMBERING_GRANT_KEY_ID", "numbering-grant-cert-1".into()),
            kv(
                "NUMBERING_GRANT_PRIVATE_KEY_FILE",
                self.write("numbering-grant.pem", &grant_key.to_pkcs8_pem())?,
            ),
        ]);
        let ready = format!("{}/ready", url(self, NUMBERING));
        self.launch(NUMBERING, "numbering", env, Some(ready))
            .await?;

        // Privacy Relay (communication contexts / relay sessions behind Calling's public
        // routes). No Contact Vault, no relay-number pool: no plaintext contact exists.
        let db_url = self
            .database(PRIVACY_RELAY, "privacy-relay-migration")
            .await?;
        let mut env = self.service_auth_env(services::PRIVACY_RELAY, true)?;
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS", "3".into()),
            kv("PRIVACY_RELAY_BIND_ADDRESS", addr(self, PRIVACY_RELAY)),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("PRIVACY_RELAY_ENABLED", "true".into()),
            kv("PRIVACY_RELAY_WORKER_INTERVAL_MS", "500".into()),
            kv("CALLING_INTERNAL_BASE_URL", url(self, CALLING)),
            kv("MESSAGING_INTERNAL_BASE_URL", url(self, MESSAGING)),
        ]);
        let ready = format!("{}/health", url(self, PRIVACY_RELAY));
        self.launch(PRIVACY_RELAY, "privacy-relay", env, Some(ready))
            .await?;

        // Calling: no media provider (signaling only), no Telephony (no PSTN).
        let db_url = self.database(CALLING, "calling-migration").await?;
        let mut env = self.service_auth_env(services::CALLING, true)?;
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("CALLING_BIND_ADDRESS", addr(self, CALLING)),
            kv("CALLING_DATABASE_MAX_CONNECTIONS", "5".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv("CF_NUMBERING_BASE_URL", url(self, NUMBERING)),
            kv("CF_PRIVACY_RELAY_BASE_URL", url(self, PRIVACY_RELAY)),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("CALLING_IDENTITY_SYNC_INTERVAL_MS", "200".into()),
            kv("CALLING_WEBHOOK_SECRET_KEY_ID", "webhooks-cert-1".into()),
            kv(
                "CALLING_WEBHOOK_SECRET_KEY_FILE",
                self.write("calling-webhook.key", &random_b64(true))?,
            ),
            kv(
                "CALLING_SIGNALING_SIGNING_KEY_FILE",
                self.write("calling-signaling.key", &random_text(48))?,
            ),
        ]);
        let ready = format!("{}/health", url(self, CALLING));
        self.launch(CALLING, "calling", env, Some(ready)).await?;

        // Messaging.
        let db_url = self.database(MESSAGING, "messaging-migration").await?;
        let kt = cf_service_auth::ServiceSigningKey::generate(
            cf_service_auth::KeyId::parse("messaging-kt-cert-1").expect("kid"),
        );
        let ct = cf_service_auth::ServiceSigningKey::generate(
            cf_service_auth::KeyId::parse("messaging-ct-cert-1").expect("kid"),
        );
        let mut env = self.service_auth_env(services::MESSAGING, true)?;
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("MESSAGING_BIND_ADDRESS", addr(self, MESSAGING)),
            kv("MESSAGING_DATABASE_MAX_CONNECTIONS", "5".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv("CF_SMS_GATEWAY_BASE_URL", url(self, SMS_GATEWAY)),
            kv("CF_CALLING_BASE_URL", url(self, CALLING)),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("MESSAGING_IDENTITY_FEED_INTERVAL_MS", "250".into()),
            kv("MESSAGING_WORKER_INTERVAL_MS", "200".into()),
            kv(
                "MESSAGING_KT_LOG_ID",
                format!("cfmeet-messaging:cert-{}", self.id),
            ),
            kv(
                "MESSAGING_KT_SIGNING_KEY_FILE",
                self.write("messaging-kt.pem", &kt.to_pkcs8_pem())?,
            ),
            kv(
                "MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE",
                self.write("messaging-ct.pem", &ct.to_pkcs8_pem())?,
            ),
            kv(
                "MESSAGING_MANAGED_KEYRING_FILE",
                self.write(
                    "messaging-managed.ring",
                    &format!("cert-1 {}\n", random_b64(false)),
                )?,
            ),
        ]);
        let ready = format!("{}/ready", url(self, MESSAGING));
        self.launch(MESSAGING, "messaging", env, Some(ready))
            .await?;

        // SMS Gateway: the deterministic provider (no carrier).
        let db_url = self.database(SMS_GATEWAY, "sms-gateway-migration").await?;
        let mut env = self.service_auth_env(services::SMS_GATEWAY, true)?;
        env.extend([
            kv("DATABASE_URL", db_url),
            kv("SMS_GATEWAY_BIND_ADDRESS", addr(self, SMS_GATEWAY)),
            kv("SMS_GATEWAY_DATABASE_MAX_CONNECTIONS", "4".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv("CF_NUMBERING_BASE_URL", url(self, NUMBERING)),
            kv("CF_CALLING_BASE_URL", url(self, CALLING)),
            kv("CF_MESSAGING_BASE_URL", url(self, MESSAGING)),
            kv("SMS_GATEWAY_CONTENT_KEY_ID", "content-cert-1".into()),
            kv(
                "SMS_GATEWAY_CONTENT_KEY_FILE",
                self.write("sms-content.key", &random_b64(true))?,
            ),
            kv(
                "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
                self.write("sms-fingerprint.key", &random_b64(true))?,
            ),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("SMS_GATEWAY_PROVIDER", "deterministic".into()),
            kv(
                "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
                self.write(
                    "sms-provider.secret",
                    &base64::engine::general_purpose::STANDARD.encode(random_text(48)),
                )?,
            ),
            kv("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES", "+1".into()),
            kv("SMS_GATEWAY_WORKER_INTERVAL_MS", "200".into()),
        ]);
        let ready = format!("{}/ready", url(self, SMS_GATEWAY));
        self.launch(SMS_GATEWAY, "sms-gateway", env, Some(ready))
            .await?;

        // The API Gateway: public routing + WebSockets, shared Redis quota counters.
        let mut env = self.service_auth_env(services::API_GATEWAY, false)?;
        env.extend([
            kv("API_GATEWAY_BIND_ADDRESS", addr(self, API_GATEWAY)),
            kv("API_GATEWAY_PUBLIC_ROUTING_ENABLED", "true".into()),
            kv("API_GATEWAY_WEBSOCKETS_ENABLED", "true".into()),
            kv("API_GATEWAY_REDIS_URL", self.config.redis_url.clone()),
            kv("API_GATEWAY_REDIS_KEY_PREFIX", format!("cert{}:", self.id)),
            kv("API_GATEWAY_QUOTA_FLUSH_INTERVAL_MS", "200".into()),
            kv("CF_PLATFORM_ACCESS_BASE_URL", url(self, PLATFORM_ACCESS)),
            kv(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                url(self, DEVELOPER_PLATFORM),
            ),
            kv("CF_CALLING_BASE_URL", url(self, CALLING)),
            kv("CF_MESSAGING_BASE_URL", url(self, MESSAGING)),
            kv("CF_NUMBERING_BASE_URL", url(self, NUMBERING)),
            kv("CF_SMS_GATEWAY_BASE_URL", url(self, SMS_GATEWAY)),
            kv("CF_MEETING_SERVICE_BASE_URL", url(self, MEETING_SERVICE)),
        ]);
        let ready = format!("{}/ready", url(self, API_GATEWAY));
        self.launch(API_GATEWAY, "api-gateway", env, Some(ready))
            .await?;
        Ok(())
    }

    pub fn url(&self, service: &str) -> &str {
        self.urls
            .get(service)
            .unwrap_or_else(|| panic!("unknown service {service}"))
    }

    pub fn gateway_url(&self) -> &str {
        self.url(API_GATEWAY)
    }

    /// The public API Gateway client.
    pub fn gateway(&self) -> Gateway {
        Gateway::new(self.gateway_url(), self.http.clone())
    }

    /// The operator console (signed operator calls to Platform Access / the Developer
    /// Platform), holding the harness's operator-console key.
    pub fn operator(&self) -> Operator {
        Operator::new(
            self.keys
                .signer_in(services::OPERATOR_CONSOLE, self.runtime),
            self.url(PLATFORM_ACCESS),
            self.url(DEVELOPER_PLATFORM),
            self.http.clone(),
        )
    }

    /// A signer holding `service`'s REAL key in this stack's runtime (a receiver-side
    /// proof calls a service directly as one of its peers).
    pub fn signer(&self, service: &str) -> cf_service_auth::ServiceTokenSigner {
        self.keys.signer_in(service, self.runtime)
    }

    /// A probe connection to `service`'s own database (evidence only).
    pub async fn db(&self, service: &str) -> anyhow::Result<sea_orm::DatabaseConnection> {
        db::connect(&self.db_urls[service]).await
    }

    /// Fails when a service process died since startup (with its log).
    pub fn assert_alive(&mut self) -> anyhow::Result<()> {
        for p in &mut self.procs {
            if let Ok(Some(status)) = p.child.try_wait() {
                bail!(
                    "{} exited ({status}); log {}:\n{}",
                    p.name,
                    p.log.display(),
                    process::tail(&p.log, 60)
                );
            }
        }
        Ok(())
    }

    /// Prints the tail of every service log (failure diagnostics).
    pub fn dump_logs(&self, lines: usize) {
        for p in &self.procs {
            eprintln!(
                "----- {} ({}) -----\n{}",
                p.name,
                p.log.display(),
                process::tail(&p.log, lines)
            );
        }
    }

    async fn shutdown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        for p in self.procs.iter_mut().rev() {
            let _ = p.child.start_kill();
        }
        for p in self.procs.iter_mut() {
            let _ = tokio::time::timeout(Duration::from_secs(5), p.child.wait()).await;
        }
        db::drop_all(&self.config.admin_url, &self.dbs).await;
        if !self.config.keep_dir {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Clean teardown: kill every child, drop every database, remove the work directory.
    pub async fn teardown(mut self) {
        self.shutdown().await;
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        let panicking = std::thread::panicking();
        if panicking {
            self.dump_logs(40);
        }
        for p in self.procs.iter_mut() {
            let _ = p.child.start_kill();
        }
        let admin = self.config.admin_url.clone();
        let dbs = std::mem::take(&mut self.dbs);
        let _ = std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async {
                    // Give the killed processes a moment to release their connections.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    db::drop_all(&admin, &dbs).await;
                });
            }
        })
        .join();
        if panicking || self.config.keep_dir {
            eprintln!("certification stack files kept in {}", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

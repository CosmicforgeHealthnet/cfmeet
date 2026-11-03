//! An isolated five-service backend stack for the Sections 4–7 acceptance journey.
//! Reuses certification's process, service-key, operator and database infrastructure.
use crate::{
    db,
    keys::{self, ServiceKeys},
    process::{self, Launch, Service},
    provision::{Customer, Operator, SANDBOX_SCOPES},
};
use anyhow::Context;
use base64::Engine;
use cf_service_auth::{services, KeyId, ServiceSigningKey};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use uuid::Uuid;

const OWNERS: &[&str] = &[
    services::PLATFORM_ACCESS,
    services::MEETING_SERVICE,
    services::CALLING,
    services::MESSAGING,
    services::COMMUNICATIONS,
];
pub struct PersonalStack {
    pub urls: BTreeMap<String, String>,
    pub owner_urls: BTreeMap<String, String>,
    pub keys: ServiceKeys,
    pub customer: Customer,
    pub http: reqwest::Client,
    pub dir: PathBuf,
    bin: PathBuf,
    admin: String,
    names: Vec<String>,
    roles: Vec<String>,
    procs: Vec<Service>,
    envs: BTreeMap<String, Vec<(String, String)>>,
}
fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn random() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
impl PersonalStack {
    pub async fn start() -> anyhow::Result<Self> {
        let admin = std::env::var("TEST_DATABASE_URL").context("TEST_DATABASE_URL required")?;
        let bin = std::env::var_os("CFMEET_BIN_DIR")
            .map(PathBuf::from)
            .context("CFMEET_BIN_DIR required")?;
        let redis = std::env::var("TEST_REDIS_URL").context("TEST_REDIS_URL required")?;
        let dir = std::env::temp_dir().join(format!("cfmeet-personal-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("logs"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let keys = ServiceKeys::generate(OWNERS);
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()?;
        let mut urls = BTreeMap::new();
        let mut allocated = std::collections::BTreeSet::new();
        for owner in OWNERS {
            let mut p = port();
            while !allocated.insert(p) {
                p = port();
            }
            urls.insert(keys::short(owner).into(), format!("http://127.0.0.1:{p}"));
        }
        let blank = Customer {
            tenant_id: Uuid::nil(),
            application_id: Uuid::nil(),
            environment_id: Uuid::nil(),
            service_account_id: Uuid::nil(),
            credential_id: Uuid::nil(),
            secret: String::new(),
            scopes: Vec::new(),
        };
        let mut s = Self {
            urls,
            owner_urls: BTreeMap::new(),
            keys,
            customer: blank,
            http,
            dir,
            bin,
            admin,
            names: Vec::new(),
            roles: Vec::new(),
            procs: Vec::new(),
            envs: BTreeMap::new(),
        };
        let result = s.boot(&redis).await;
        if let Err(e) = result {
            s.dump_logs();
            s.close().await;
            return Err(e);
        }
        Ok(s)
    }
    pub fn write(&self, name: &str, value: &str) -> anyhow::Result<String> {
        let path = self.dir.join(name);
        std::fs::write(&path, value)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(path.display().to_string())
    }
    fn env(&self, owner: &str) -> anyhow::Result<Vec<(String, String)>> {
        let name = keys::short(owner);
        Ok(vec![
            ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
            ("CF_SERVICE_AUTH_KEY_ID".into(), keys::kid(owner)),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.write(&format!("{name}.pem"), &self.keys.pem(owner))?,
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.write(
                    &format!("{name}.trust.json"),
                    &keys::trust_bundle(owner, OWNERS, &self.keys).to_string(),
                )?,
            ),
            (
                "CF_PLATFORM_ACCESS_BASE_URL".into(),
                self.urls["platform-access"].clone(),
            ),
            (
                "DATABASE_URL".into(),
                self.envs[name]
                    .iter()
                    .find(|(k, _)| k == "DATABASE_URL")
                    .unwrap()
                    .1
                    .clone(),
            ),
        ])
    }
    async fn launch(
        &mut self,
        name: &str,
        bin: &str,
        env: Vec<(String, String)>,
        ready: &str,
    ) -> anyhow::Result<()> {
        let process = process::spawn(
            Launch {
                name,
                bin: &self.bin.join(bin),
                env: env.clone(),
                dir: &self.dir,
                log_dir: &self.dir.join("logs"),
                ready: Some(format!("{}{ready}", self.urls[name])),
            },
            &self.http,
        )
        .await?;
        self.envs.insert(name.into(), env);
        self.procs.push(process);
        Ok(())
    }
    async fn boot(&mut self, redis: &str) -> anyhow::Result<()> {
        for (name, base, feature) in [
            ("platform-access", "platform-access-migration", None),
            (
                "meeting-service",
                "migration",
                Some("account-communications-migration"),
            ),
            (
                "calling",
                "calling-migration",
                Some("calling-communications-migration"),
            ),
            (
                "messaging",
                "messaging-migration",
                Some("messaging-communications-migration"),
            ),
            ("communications", "communications-migration", None),
        ] {
            let db_name = format!(
                "comm_{}_{}",
                Uuid::new_v4().simple(),
                name.replace('-', "_")
            );
            let owner = db::create(&self.admin, &db_name).await?;
            self.names.push(db_name.clone());
            db::migrate(&self.bin.join(base), &owner)?;
            if let Some(feature) = feature {
                db::migrate(&self.bin.join(feature), &owner)?;
            }
            let (role, runtime) = db::runtime_role(&self.admin, &db_name).await?;
            self.roles.push(role);
            self.owner_urls.insert(name.into(), owner);
            self.envs
                .insert(name.into(), vec![("DATABASE_URL".into(), runtime)]);
        }
        let mut pa = self.env(services::PLATFORM_ACCESS)?;
        pa.extend([
            (
                "PLATFORM_ACCESS_BIND_ADDRESS".into(),
                self.urls["platform-access"]
                    .trim_start_matches("http://")
                    .into(),
            ),
            ("API_CREDENTIAL_PEPPER".into(), random()),
            (
                "PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS".into(),
                "5".into(),
            ),
        ]);
        self.launch("platform-access", "platform-access", pa, "/ready")
            .await?;
        self.customer = Operator::new(
            self.keys.signer(services::OPERATOR_CONSOLE),
            &self.urls["platform-access"],
            "http://127.0.0.1:9",
            self.http.clone(),
        )
        .customer("FIRST_PARTY", "TEST", SANDBOX_SCOPES)
        .await?;
        let personal = [
            (
                "CF_COMMUNICATIONS_BASE_URL",
                self.urls["communications"].clone(),
            ),
            (
                "CF_COMMUNICATIONS_PERSONAL_TENANT_ID",
                self.customer.tenant_id.to_string(),
            ),
            (
                "CF_COMMUNICATIONS_PERSONAL_APPLICATION_ID",
                self.customer.application_id.to_string(),
            ),
            (
                "CF_COMMUNICATIONS_PERSONAL_ENVIRONMENT_ID",
                self.customer.environment_id.to_string(),
            ),
        ];
        let mut account = self.env(services::MEETING_SERVICE)?;
        account.extend([
            ("HOST".into(), "127.0.0.1".into()),
            (
                "PORT".into(),
                self.urls["meeting-service"]
                    .rsplit(':')
                    .next()
                    .unwrap()
                    .into(),
            ),
            ("APP_ENV".into(), "test".into()),
            ("APP_URL".into(), "http://127.0.0.1:3000".into()),
            ("FRONTEND_URLS".into(), "http://127.0.0.1:3000".into()),
            ("JWT_SECRET".into(), random()),
            ("REDIS_URL".into(), redis.into()),
            ("SMTP_HOST".into(), String::new()),
            ("EMAIL_VERIFICATION_REQUIRED".into(), "false".into()),
            ("LIVEKIT_URL".into(), "ws://127.0.0.1:9".into()),
            ("LIVEKIT_API_KEY".into(), "personal-unused".into()),
            ("LIVEKIT_API_SECRET".into(), random()),
            ("GOOGLE_CLIENT_ID".into(), "personal-unused".into()),
            ("GOOGLE_CLIENT_SECRET".into(), random()),
            (
                "GOOGLE_REDIRECT_URL".into(),
                "http://127.0.0.1:3000/oauth/google/callback".into(),
            ),
            ("FEATURE_MEETING_V2_ENABLED".into(), "true".into()),
            (
                "CF_COMMUNICATIONS_BASE_URL".into(),
                self.urls["communications"].clone(),
            ),
            (
                "CFMEET_COMMUNICATION_TENANT_ID".into(),
                self.customer.tenant_id.to_string(),
            ),
            (
                "CFMEET_COMMUNICATION_APPLICATION_ID".into(),
                self.customer.application_id.to_string(),
            ),
            (
                "CFMEET_COMMUNICATION_ENVIRONMENT_ID".into(),
                self.customer.environment_id.to_string(),
            ),
            (
                "CFMEET_COMMUNICATION_ENVIRONMENT_KIND".into(),
                "TEST".into(),
            ),
            (
                "CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID".into(),
                self.customer.service_account_id.to_string(),
            ),
        ]);
        self.launch("meeting-service", "backend", account, "/health")
            .await?;
        let mut calling = self.env(services::CALLING)?;
        calling.extend(personal.iter().map(|(k, v)| (k.to_string(), v.clone())));
        calling.extend([
            (
                "CALLING_BIND_ADDRESS".into(),
                self.urls["calling"].trim_start_matches("http://").into(),
            ),
            ("CALLING_DATABASE_MAX_CONNECTIONS".into(), "5".into()),
            (
                "CALLING_SIGNALING_SIGNING_KEY_FILE".into(),
                self.write("calling.signaling.key", &random())?,
            ),
            ("CALLING_NEW_CALLS_ENABLED".into(), "true".into()),
            ("CALLING_APP_CALLING_ENABLED".into(), "true".into()),
            ("CALLING_MEDIA_PROVIDER".into(), "livekit".into()),
            (
                "CALLING_LIVEKIT_API_URL".into(),
                std::env::var("COMMUNICATIONS_E2E_LIVEKIT_API_URL")
                    .context("real LiveKit API URL is required")?,
            ),
            (
                "CALLING_LIVEKIT_URL".into(),
                std::env::var("COMMUNICATIONS_E2E_LIVEKIT_URL")
                    .context("real LiveKit signaling URL is required")?,
            ),
            (
                "CALLING_LIVEKIT_API_KEY".into(),
                std::env::var("COMMUNICATIONS_E2E_LIVEKIT_KEY")
                    .context("LiveKit test key required")?,
            ),
            (
                "CALLING_LIVEKIT_API_SECRET".into(),
                std::env::var("COMMUNICATIONS_E2E_LIVEKIT_SECRET")
                    .context("LiveKit test secret required")?,
            ),
            (
                "CALLING_MEDIA_GATEWAY_URL".into(),
                self.urls["calling"].replace("http://", "ws://") + "/v1/media/rtc",
            ),
            (
                "CALLING_MEDIA_EDGE_SIGNING_KEY_FILE".into(),
                self.write("calling.media.key", &random())?,
            ),
        ]);
        self.launch("calling", "calling", calling, "/health")
            .await?;
        let mut messaging = self.env(services::MESSAGING)?;
        messaging.extend(personal.iter().map(|(k, v)| (k.to_string(), v.clone())));
        let kt = ServiceSigningKey::generate(KeyId::parse("personal-kt").unwrap());
        let ct = ServiceSigningKey::generate(KeyId::parse("personal-ct").unwrap());
        messaging.push((
            "MESSAGING_MANAGED_KEYRING_FILE".into(),
            self.write(
                "messaging.managed.ring",
                &format!(
                    "personal-1 {}\n",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode([Uuid::new_v4().as_bytes(), Uuid::new_v4().as_bytes()].concat())
                ),
            )?,
        ));
        messaging.extend([
            (
                "MESSAGING_BIND_ADDRESS".into(),
                self.urls["messaging"].trim_start_matches("http://").into(),
            ),
            ("MESSAGING_DATABASE_MAX_CONNECTIONS".into(), "5".into()),
            (
                "MESSAGING_KT_LOG_ID".into(),
                format!("personal:{}", Uuid::new_v4()),
            ),
            (
                "MESSAGING_KT_SIGNING_KEY_FILE".into(),
                self.write("messaging.kt.pem", &kt.to_pkcs8_pem())?,
            ),
            (
                "MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE".into(),
                self.write("messaging.ct.pem", &ct.to_pkcs8_pem())?,
            ),
        ]);
        self.launch("messaging", "messaging", messaging, "/ready")
            .await?;
        let mut hub = self.env(services::COMMUNICATIONS)?;
        hub.extend([
            (
                "COMMUNICATIONS_BIND_ADDRESS".into(),
                self.urls["communications"]
                    .trim_start_matches("http://")
                    .into(),
            ),
            (
                "COMMUNICATIONS_INVITE_DIGEST_KEY_FILE".into(),
                self.write("invite.key", &random())?,
            ),
            ("CFMEET_PUBLIC_APP_URL".into(), "https://cfmeet.com".into()),
            (
                "CF_MEETING_SERVICE_BASE_URL".into(),
                self.urls["meeting-service"].clone(),
            ),
            ("CF_CALLING_BASE_URL".into(), self.urls["calling"].clone()),
            (
                "CF_MESSAGING_BASE_URL".into(),
                self.urls["messaging"].clone(),
            ),
        ]);
        self.launch("communications", "communications", hub, "/ready")
            .await?;
        Ok(())
    }
    pub fn dump_logs(&self) {
        for p in &self.procs {
            eprintln!("{}: {}", p.name, process::tail(&p.log, 30));
        }
    }
    pub async fn stop(&mut self, name: &str) {
        if let Some(p) = self.procs.iter_mut().find(|p| p.name == name) {
            let _ = p.child.kill().await;
        }
    }
    pub async fn close(mut self) {
        for p in &mut self.procs {
            let _ = p.child.kill().await;
        }
        db::drop_all(&self.admin, &self.names).await;
        db::drop_roles(&self.admin, &self.roles).await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

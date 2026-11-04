use axum::{
    body::to_bytes,
    extract::{Request, State as HttpState},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    *,
};
use communications::{
    config::Config,
    error::Error,
    model::{Profile, VerifiedAccount},
    store::Store,
    Metrics, State,
};
use communications_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
pub struct Account {
    pub principal: Uuid,
    pub reference: String,
    pub active: bool,
    pub version: i64,
    pub cutoff: Option<chrono::DateTime<chrono::Utc>>,
}
#[derive(Clone)]
struct Owners {
    available: Arc<AtomicBool>,
    accounts: Arc<RwLock<BTreeMap<Uuid, Account>>>,
    verifiers: Arc<Vec<ServiceTokenVerifier>>,
    blocked: Arc<RwLock<bool>>,
}
async fn owner(HttpState(s): HttpState<Owners>, request: Request) -> Response {
    if !s.available.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !s
        .verifiers
        .iter()
        .any(|v| v.verify_headers(request.headers()).is_ok())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let path = request.uri().path().to_owned();
    let b = to_bytes(request.into_body(), 16384).await.unwrap();
    let body: Value = if b.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&b).unwrap()
    };
    let accounts = s.accounts.read().unwrap();
    let value = match path.as_str() {
        "/internal/v1/identity/context" => {
            json!({"authorityMode":"FULL","tenant":{"classification":"FIRST_PARTY"}})
        }
        "/internal/v1/principals/resolve" => {
            let results:Vec<_>=body["references"].as_array().unwrap().iter().map(|v| {
                let id=v["principalId"].as_str().and_then(|v|Uuid::parse_str(v).ok());
                match id.and_then(|i|accounts.get(&i)) {Some(a)=>json!({"outcome":"RESOLVED","principal":{"id":a.principal,"kind":"NATIVE_CF_USER","externalReference":format!("legacy-native-{}",a.principal),"status":"ACTIVE"}}),None=>json!({"outcome":"NOT_FOUND"})}
            }).collect();
            json!({"results":results})
        }
        "/internal/v1/accounts/communications/status" => match accounts
            .values()
            .find(|a| Some(a.reference.as_str()) == body["accountReference"].as_str())
        {
            Some(a) => {
                json!({"active":a.active,"version":a.version,"credentialsNotBefore":a.cutoff,"principalId":a.principal})
            }
            None => json!({"active":false,"version":0}),
        },
        "/internal/v1/communications/blocks/check" => json!({"blocked":*s.blocked.read().unwrap()}),
        "/internal/v1/communications/capabilities" => {
            json!({"available":false,"reason":"DEVICE_SECURITY_SETUP_REQUIRED","securityReady":false,"privateField":"MUST_NOT_REACH_CONSUMER"})
        }
        "/internal/v1/communications/common-groups" => json!({"groups":[]}),
        "/internal/v1/communications/events" => json!({"applied":true}),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Json(value).into_response()
}
fn key(name: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(name).unwrap())
}
fn signer(service: ServiceId, key: ServiceSigningKey) -> Arc<ServiceTokenSigner> {
    Arc::new(ServiceTokenSigner::new(
        service,
        RuntimeEnvironment::Test,
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ))
}
fn verifier(
    receiver: ServiceId,
    grants: Vec<(ServiceId, PublicJwk, Vec<&str>)>,
) -> ServiceTokenVerifier {
    let bundle = json!({"bundle_version":1,"receiver":receiver,"runtime_environment":"test","trusted_issuers":grants.into_iter().map(|(issuer,key,permissions)|json!({"issuer":issuer,"audience":receiver,"runtime_environment":"test","keys":[key],"permissions":permissions})).collect::<Vec<_>>()});
    ServiceTokenVerifier::new(
        TrustPolicy::from_json(&bundle.to_string(), &receiver, RuntimeEnvironment::Test).unwrap(),
        Arc::new(SystemClock),
    )
}
pub struct Harness {
    available: Arc<AtomicBool>,
    pub state: State,
    pub scope: DelegatedPlatformContext,
    pub accounts: Arc<RwLock<BTreeMap<Uuid, Account>>>,
    pub blocked: Arc<RwLock<bool>>,
    pub account_signer: Arc<ServiceTokenSigner>,
    pub calling_signer: Arc<ServiceTokenSigner>,
    pub operator: Arc<ServiceTokenSigner>,
    admin: DatabaseConnection,
    name: String,
    server: tokio::task::JoinHandle<()>,
}
impl Harness {
    pub fn stop_owners(&self) {
        self.available.store(false, Ordering::SeqCst);
        self.server.abort();
    }
    pub async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect(
            "PostgreSQL acceptance tests require TEST_DATABASE_URL; there is no implicit skip",
        );
        let admin = Database::connect(&url)
            .await
            .expect("test database connection");
        let name = format!("hub_t_{}", Uuid::new_v4().simple());
        admin
            .execute(Statement::from_string(
                DbBackend::Postgres,
                format!("CREATE DATABASE {name}"),
            ))
            .await
            .unwrap();
        let db = Database::connect(format!("{}/{}", url.rsplit_once('/').unwrap().0, name))
            .await
            .unwrap();
        assert!(!communications_migration::ready(&db).await.unwrap());
        Migrator::up(&db, None).await.unwrap();
        Migrator::up(&db, None).await.unwrap();
        assert!(communications_migration::ready(&db).await.unwrap());
        let hub = signer(ServiceId::communications(), key("hub-test"));
        let account_signer = signer(ServiceId::meeting_service(), key("account-test"));
        let calling_signer = signer(ServiceId::calling(), key("calling-test"));
        let operator = signer(ServiceId::operator_console(), key("operator-test"));
        let verifiers = vec![
            verifier(
                ServiceId::platform_access(),
                vec![(
                    ServiceId::communications(),
                    hub.public_jwk(),
                    vec!["platform-identity.read", "platform-principal.resolve"],
                )],
            ),
            verifier(
                ServiceId::meeting_service(),
                vec![(
                    ServiceId::communications(),
                    hub.public_jwk(),
                    vec!["communications-account.read"],
                )],
            ),
            verifier(
                ServiceId::calling(),
                vec![(
                    ServiceId::communications(),
                    hub.public_jwk(),
                    vec![
                        "calling-communications.read",
                        "calling-communications.apply",
                    ],
                )],
            ),
            verifier(
                ServiceId::messaging(),
                vec![(
                    ServiceId::communications(),
                    hub.public_jwk(),
                    vec![
                        "messaging-communications.read",
                        "messaging-communications.apply",
                    ],
                )],
            ),
        ];
        let accounts = Arc::new(RwLock::new(BTreeMap::new()));
        let blocked = Arc::new(RwLock::new(false));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let available = Arc::new(AtomicBool::new(true));
        let router = Router::new().fallback(any(owner)).with_state(Owners {
            available: available.clone(),
            accounts: accounts.clone(),
            verifiers: Arc::new(verifiers),
            blocked: blocked.clone(),
        });
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut targets = InternalTargets::new(RuntimeEnvironment::Test);
        for id in [
            ServiceId::platform_access(),
            ServiceId::meeting_service(),
            ServiceId::calling(),
            ServiceId::messaging(),
        ] {
            targets.insert(id, &format!("http://{address}")).unwrap();
        }
        let inbound = verifier(
            ServiceId::communications(),
            vec![
                (
                    ServiceId::meeting_service(),
                    account_signer.public_jwk(),
                    vec![
                        "communications-user.forward",
                        "communications-account.apply",
                    ],
                ),
                (
                    ServiceId::calling(),
                    calling_signer.public_jwk(),
                    vec!["communications-policy.check", "communications-block.apply"],
                ),
                (
                    ServiceId::operator_console(),
                    operator.public_jwk(),
                    vec!["communications-backfill.run"],
                ),
            ],
        );
        let state = State {
            store: Store { db },
            config: Arc::new(Config {
                bind: "127.0.0.1:0".parse().unwrap(),
                database_url: "redacted-test-only".to_owned().into(),
                public_app_url: "https://cfmeet.com".into(),
                invite_key: "a".repeat(64).into(),
                verifier: Arc::new(inbound),
                client: Arc::new(
                    InternalServiceClient::new(hub, targets, Duration::from_secs(2)).unwrap(),
                ),
                calling_configured: true,
                messaging_configured: true,
            }),
            metrics: Arc::new(Metrics::default()),
        };
        Self {
            state,
            scope: DelegatedPlatformContext::new(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                ApplicationEnvironmentKind::Test,
                Uuid::new_v4(),
            ),
            accounts,
            blocked,
            account_signer,
            available,
            calling_signer,
            operator,
            admin,
            name,
            server,
        }
    }
    pub fn account(&self) -> VerifiedAccount {
        let principal = Uuid::new_v4();
        let reference = format!("cfmeet-user:{}", Uuid::new_v4());
        self.accounts.write().unwrap().insert(
            principal,
            Account {
                principal,
                reference: reference.clone(),
                active: true,
                version: 1,
                cutoff: None,
            },
        );
        VerifiedAccount {
            account_reference: reference,
            principal_id: principal,
            display_name: "Same chosen name".into(),
            avatar_reference: None,
            authenticated_at: Some(chrono::Utc::now().timestamp()),
        }
    }
    pub fn delegated(&self, a: &VerifiedAccount) -> DelegatedPlatformContext {
        self.scope
            .clone()
            .with_initiating_principal(a.principal_id, DelegatedPrincipalKind::NativeCfUser)
    }
    pub async fn profile(&self, a: &VerifiedAccount) -> Profile {
        self.state
            .ensure_profile(&self.delegated(a), a)
            .await
            .unwrap()
    }
    pub async fn public_id(&self, p: &Profile) -> String {
        self.state
            .store
            .current_public_id(&self.state.store.db, p.id)
            .await
            .unwrap()
    }
    pub async fn http(
        &self,
        signer: &ServiceTokenSigner,
        path: &str,
        body: Value,
        scope: DelegatedPlatformContext,
    ) -> (StatusCode, Value) {
        let headers = signer
            .sign(
                &OutboundServiceRequest::new_operation(ServiceId::communications())
                    .with_delegated_context(scope),
            )
            .unwrap()
            .headers();
        self.http_headers(path, body, headers).await
    }
    pub async fn http_headers(
        &self,
        path: &str,
        body: Value,
        headers: HeaderMap,
    ) -> (StatusCode, Value) {
        let mut r = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        r.headers_mut().extend(headers);
        let response = communications::api::app(self.state.clone())
            .oneshot(r)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    pub async fn connected(&self, a: &Profile, b: &Profile) -> Uuid {
        let request = self
            .state
            .send_request(
                &self.scope,
                a,
                json!({"cfMeetId":self.public_id(b).await}),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        let id = Uuid::parse_str(request["requestId"].as_str().unwrap()).unwrap();
        self.state
            .decide_request(
                &self.scope,
                b,
                id,
                "accept",
                json!({"expectedVersion":1}),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        id
    }
    pub async fn close(self) {
        self.server.abort();
        self.state.store.db.clone().close().await.unwrap();
        self.admin
            .execute(Statement::from_string(
                DbBackend::Postgres,
                format!("DROP DATABASE {} WITH (FORCE)", self.name),
            ))
            .await
            .unwrap();
    }
}
pub fn not_found<T>(r: Result<T, Error>) {
    assert!(matches!(r, Err(Error::NotFound)));
}
pub fn check(
    action: cf_communications_policy::Action,
    a: &Profile,
    b: &Profile,
) -> cf_communications_policy::PolicyCheck {
    cf_communications_policy::PolicyCheck {
        action,
        actor_principal_id: a.principal_id,
        target_principal_id: b.principal_id,
        credential_issued_at: None,
        credential_principal_id: None,
    }
}

// Shared by several test crates; each uses a different subset of helpers.
#![allow(dead_code)]
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
/// Simulated canonical owners for the Sections 9-23 private routes.
#[derive(Default)]
pub struct World {
    pub conversations: BTreeMap<Uuid, MockConversation>,
    /// idempotency key -> (call id, initiator, targets, kind)
    pub launches: BTreeMap<String, (Uuid, Uuid, Vec<Uuid>, String)>,
    /// call id -> (state, terminal, version, outcome for recipients)
    pub calls: BTreeMap<Uuid, (String, bool, i64, String)>,
    pub devices: BTreeMap<Uuid, (Uuid, String)>,
    pub endpoints: BTreeMap<Uuid, (Uuid, String)>,
    /// meeting code -> (space id, owner principal)
    pub meetings: BTreeMap<String, (Uuid, Uuid)>,
    pub instant: BTreeMap<String, (Uuid, String)>,
    pub calling_down: bool,
    pub revoked_devices: Vec<Uuid>,
    pub revoked_endpoints: Vec<Uuid>,
}
#[derive(Clone)]
pub struct MockConversation {
    pub context: String,
    pub members: Vec<Uuid>,
    pub last_sequence: i64,
    /// message id -> (sequence, state)
    pub messages: BTreeMap<Uuid, (i64, String)>,
}
fn reject(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"schemaVersion":1,"error":{"code":code,"message":"mock"}})),
    )
        .into_response()
}
fn uuid_at(v: &Value, k: &str) -> Uuid {
    Uuid::parse_str(v[k].as_str().unwrap_or_default()).unwrap_or_default()
}
fn entry(world: &World, call: Uuid, viewer: Uuid) -> Option<Value> {
    let (_, (id, initiator, targets, kind)) = world.launches.iter().find(|(_, l)| l.0 == call)?;
    if *initiator != viewer && !targets.contains(&viewer) {
        return None;
    }
    let (state, terminal, version, outcome) = world.calls.get(id)?.clone();
    let outgoing = *initiator == viewer;
    let counterparts: Vec<Uuid> = if outgoing {
        targets.clone()
    } else {
        vec![*initiator]
    };
    Some(
        json!({"callId":id,"kind":kind,"state":state,"terminal":terminal,"version":version,
        "createdAt":"2026-11-06T10:00:00Z","answeredAt":null,"endedAt":null,"durationMs":if terminal {json!(0)} else {Value::Null},
        "direction":if outgoing {"OUTGOING"} else {"INCOMING"},"outcome":if outgoing {"ANSWERED".to_owned()} else {outcome.clone()},
        "missed":!outgoing && outcome=="MISSED","counterpartPrincipalIds":counterparts}),
    )
}
fn world_route(world: &mut World, path: &str, body: &Value, caller: Uuid) -> Option<Response> {
    const WORLD: &[&str] = &[
        "conversations/access",
        "conversations/attachments",
        "storage-summary",
        "devices/verify",
        "devices/revoke",
        "devices/push",
        "push-status",
        "endpoints/verify",
        "endpoints/revoke",
        "calls",
        "calls/participation",
        "calls/history",
        "meetings/instant",
        "meetings/resolve",
    ];
    if !WORLD
        .iter()
        .any(|w| path == format!("/internal/v1/communications/{w}"))
    {
        return None;
    }
    let principal = uuid_at(
        body,
        if path.ends_with("/calls") {
            "initiatorPrincipalId"
        } else {
            "principalId"
        },
    );
    if principal != caller {
        return Some(reject(StatusCode::FORBIDDEN, "PRINCIPAL_MISMATCH"));
    }
    let ok = |v: Value| Some(Json(v).into_response());
    match path {
        "/internal/v1/communications/conversations/access" => {
            let id = uuid_at(body, "conversationId");
            match world
                .conversations
                .get(&id)
                .filter(|c| c.members.contains(&principal))
            {
                None => ok(json!({"schemaVersion":1,"member":false})),
                Some(c) => {
                    let mut v = json!({"schemaVersion":1,"member":true,"conversationId":id,"contextClass":c.context,
                        "securityProfile":"PRIVATE_E2EE","lifecycleState":"ACTIVE","lastSequence":c.last_sequence,
                        "visibleFromSequence":0,"retentionSeconds":null,"expiresAt":null,"role":"MEMBER",
                        "memberCount":c.members.len(),"memberPrincipalIds":c.members});
                    if let Some(m) = body["messageId"]
                        .as_str()
                        .and_then(|m| Uuid::parse_str(m).ok())
                    {
                        v["message"] = match c.messages.get(&m) {
                            Some((seq, state)) => {
                                json!({"messageId":m,"sequence":seq,"state":state,"sentAt":"2026-11-06T10:00:00Z","senderPrincipalId":principal})
                            }
                            None => json!({"messageId":m,"sequence":null,"state":"NOT_FOUND"}),
                        };
                    }
                    ok(v)
                }
            }
        }
        "/internal/v1/communications/conversations/attachments" => {
            ok(json!({"schemaVersion":1,"data":[
            {"attachmentId":Uuid::new_v4(),"messageId":Uuid::new_v4(),"messageSequence":1,"state":"READY","sizeBytes":10,"mediaClass":"ENCRYPTED","createdAt":"2026-11-06T10:00:00Z","uploaderPrincipalId":principal},
            {"attachmentId":Uuid::new_v4(),"messageId":Uuid::new_v4(),"messageSequence":5,"state":"READY","sizeBytes":20,"mediaClass":"ENCRYPTED","createdAt":"2026-11-06T10:00:00Z","uploaderPrincipalId":principal}],"nextCursor":null}))
        }
        "/internal/v1/communications/storage-summary" => ok(
            json!({"schemaVersion":1,"attachmentCount":2,"attachmentBytes":30,"backupBytes":0,"environmentQuotaBytes":null,"measuredAt":"2026-11-06T10:00:00Z"}),
        ),
        "/internal/v1/communications/devices/verify" => {
            let d = world.devices.get(&uuid_at(body, "deviceId"));
            ok(
                json!({"schemaVersion":1,"owned":d.is_some_and(|d| d.0==principal),"status":d.filter(|d| d.0==principal).map(|d| d.1.clone()),"pushRegistered":false,"pushPlatform":null}),
            )
        }
        "/internal/v1/communications/devices/revoke" => {
            let id = uuid_at(body, "deviceId");
            match world.devices.get_mut(&id).filter(|d| d.0 == principal) {
                None => Some(reject(StatusCode::NOT_FOUND, "NOT_FOUND")),
                Some(d) => {
                    let already = d.1 == "REVOKED";
                    d.1 = "REVOKED".into();
                    world.revoked_devices.push(id);
                    ok(json!({"schemaVersion":1,"revoked":true,"alreadyRevoked":already}))
                }
            }
        }
        "/internal/v1/communications/devices/push" => {
            let register = body["action"] == "REGISTER";
            if register && body["registration"]["token"].as_str().is_none() {
                return Some(reject(StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
            }
            ok(
                json!({"schemaVersion":1,"deviceId":body["deviceId"],"pushRegistered":register,"platform":if register {json!("FCM")} else {Value::Null}}),
            )
        }
        "/internal/v1/communications/push-status" => ok(
            json!({"schemaVersion":1,"providers":{"APNS":"NOT_CONFIGURED","FCM":"NOT_CONFIGURED","WEB_PUSH":"NOT_CONFIGURED"},"devices":[],"pendingJobs":0,"deadJobs":0}),
        ),
        "/internal/v1/communications/endpoints/verify" => {
            let e = world.endpoints.get(&uuid_at(body, "endpointId"));
            ok(
                json!({"schemaVersion":1,"owned":e.is_some_and(|e| e.0==principal),"status":e.filter(|e| e.0==principal).map(|e| e.1.clone()),"endpointKind":"MOBILE"}),
            )
        }
        "/internal/v1/communications/endpoints/revoke" => {
            let id = uuid_at(body, "endpointId");
            match world.endpoints.get_mut(&id).filter(|e| e.0 == principal) {
                None => Some(reject(StatusCode::NOT_FOUND, "NOT_FOUND")),
                Some(e) => {
                    e.1 = "REVOKED".into();
                    world.revoked_endpoints.push(id);
                    ok(json!({"schemaVersion":1,"revoked":true,"alreadyRevoked":false}))
                }
            }
        }
        "/internal/v1/communications/calls" => {
            if world.calling_down {
                return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
            }
            let endpoint = uuid_at(body, "initiatorEndpointId");
            if !world
                .endpoints
                .get(&endpoint)
                .is_some_and(|e| e.0 == principal && e.1 == "ACTIVE")
            {
                return Some(reject(StatusCode::CONFLICT, "ENDPOINT_NOT_ACTIVE"));
            }
            let key = body["idempotencyKey"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let targets: Vec<Uuid> = body["targetPrincipalIds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| Uuid::parse_str(v.as_str().unwrap()).unwrap())
                .collect();
            let kind = body["kind"].as_str().unwrap_or_default().to_owned();
            if let Some(l) = world.launches.get(&key) {
                if l.2 != targets || l.3 != kind {
                    return Some(reject(StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT"));
                }
                return ok(
                    json!({"schemaVersion":1,"callId":l.0,"state":"RINGING","kind":kind,"version":1,"createdAt":"2026-11-06T10:00:00Z","replayed":true}),
                );
            }
            let id = Uuid::new_v4();
            world
                .launches
                .insert(key, (id, principal, targets, kind.clone()));
            world
                .calls
                .insert(id, ("RINGING".into(), false, 1, "RINGING".into()));
            ok(
                json!({"schemaVersion":1,"callId":id,"state":"RINGING","kind":kind,"version":1,"createdAt":"2026-11-06T10:00:00Z","replayed":false}),
            )
        }
        "/internal/v1/communications/calls/participation" => {
            if world.calling_down {
                return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
            }
            let calls: Vec<Value> = body["callIds"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|c| Uuid::parse_str(c.as_str()?).ok())
                .filter_map(|c| entry(world, c, principal))
                .collect();
            ok(json!({"schemaVersion":1,"calls":calls}))
        }
        "/internal/v1/communications/calls/history" => {
            let ids: Vec<Uuid> = world.launches.values().map(|l| l.0).collect();
            let data: Vec<Value> = ids
                .into_iter()
                .filter_map(|c| entry(world, c, principal))
                .filter(|e| body.get("missed").is_none() || e["missed"] == body["missed"])
                .collect();
            ok(json!({"schemaVersion":1,"data":data,"nextCursor":null}))
        }
        "/internal/v1/communications/meetings/instant" => {
            let key = body["idempotencyKey"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let (space, code) = world
                .instant
                .entry(key)
                .or_insert_with(|| {
                    (
                        Uuid::new_v4(),
                        format!("abc-{}", &Uuid::new_v4().simple().to_string()[..8]),
                    )
                })
                .clone();
            world.meetings.insert(code.clone(), (space, principal));
            ok(
                json!({"schemaVersion":1,"meetingSpaceId":space,"conferenceId":Uuid::new_v4(),"meetingCode":code,"createdAt":"2026-11-06T10:00:00Z","replayed":false}),
            )
        }
        "/internal/v1/communications/meetings/resolve" => match world
            .meetings
            .get(body["meetingCode"].as_str().unwrap_or_default())
        {
            None => Some(reject(StatusCode::NOT_FOUND, "NOT_FOUND")),
            Some((space, owner)) => ok(
                json!({"schemaVersion":1,"meetingSpaceId":space,"meetingCode":body["meetingCode"],"title":null,
                "shareable":*owner==principal,"role":if *owner==principal {"OWNER"} else {"NONE"},"latestConference":null}),
            ),
        },
        _ => None,
    }
}
#[derive(Clone)]
struct Owners {
    available: Arc<AtomicBool>,
    accounts: Arc<RwLock<BTreeMap<Uuid, Account>>>,
    verifiers: Arc<Vec<ServiceTokenVerifier>>,
    blocked: Arc<RwLock<bool>>,
    world: Arc<RwLock<World>>,
}
async fn owner(HttpState(s): HttpState<Owners>, request: Request) -> Response {
    if !s.available.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(ctx) = s
        .verifiers
        .iter()
        .find_map(|v| v.verify_headers(request.headers()).ok())
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let caller = ctx
        .delegated_platform_context()
        .and_then(|d| d.initiating_principal().map(|p| p.principal_id))
        .unwrap_or_default();
    let path = request.uri().path().to_owned();
    let b = to_bytes(request.into_body(), 16384).await.unwrap();
    let body: Value = if b.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&b).unwrap()
    };
    if let Some(response) = world_route(&mut s.world.write().unwrap(), &path, &body, caller) {
        return response;
    }
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
    pub world: Arc<RwLock<World>>,
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
        // The previous image checks the foundation history alone; it must stay ready
        // after the separate Sections 9-23 history is applied (rollback safety).
        assert!(communications_migration::foundation_ready(&db)
            .await
            .unwrap());
        assert!(!communications_migration::ready(&db).await.unwrap());
        communications_migration::up_all(&db).await.unwrap();
        communications_migration::up_all(&db).await.unwrap();
        assert!(communications_migration::foundation_ready(&db)
            .await
            .unwrap());
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
        let world = Arc::new(RwLock::new(World::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let available = Arc::new(AtomicBool::new(true));
        let router = Router::new().fallback(any(owner)).with_state(Owners {
            available: available.clone(),
            accounts: accounts.clone(),
            verifiers: Arc::new(verifiers),
            blocked: blocked.clone(),
            world: world.clone(),
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
                    vec![
                        "communications-policy.check",
                        "communications-block.apply",
                        "communications-events.publish",
                    ],
                ),
                (
                    ServiceId::operator_console(),
                    operator.public_jwk(),
                    vec![
                        "communications-backfill.run",
                        "communications-observability.read",
                        "communications-profile.suspend",
                    ],
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
            world,
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
        conversation_id: None,
    }
}

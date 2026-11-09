//! Sections 9–23 MT-1 / MT-2 — the Communications Hub's private meeting facade
//! (`POST /internal/v1/communications/meetings/instant` and `/resolve`).
//!
//! Only `cfmeet.communications` with the receiver-granted permission, delegated to the
//! governed personal scope with the initiating principal of the account Account maps
//! (`communication_principal_links`), may start or resolve a meeting — and only as that
//! user. Instant meetings run the canonical `POST /api/v2/instant-meetings` domain path
//! and replay per (account, idempotency key). Resolve returns public references only.

mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use backend::services::communications::{Bridge, INSTANT_MEETING_PATH, RESOLVE_MEETING_PATH};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind, KeyId,
    OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::*;
use migration::MigratorTrait;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const CREATE: &str = "meeting-communications.create";
const READ: &str = "meeting-communications.read";
const ACCOUNT_READ: &str = "communications-account.read";

fn scope() -> DelegatedPlatformContext {
    DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        ApplicationEnvironmentKind::Test,
        Uuid::new_v4(),
    )
}

/// The Hub's signer and its public key (for the receiver's trust bundle).
fn hub() -> (ServiceTokenSigner, ServiceSigningKey) {
    let kid = "communications-test-1";
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (
        ServiceTokenSigner::new(
            ServiceId::communications(),
            RuntimeEnvironment::Test,
            key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ),
        public,
    )
}

/// Meeting Service's Communications bridge trusting the Hub's key with `permissions`.
fn bridge(
    hub_key: &ServiceSigningKey,
    permissions: &[&str],
    scope: &DelegatedPlatformContext,
) -> Bridge {
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service", "runtime_environment": "test",
    "trusted_issuers": [{
        "issuer": "cfmeet.communications", "audience": "cfmeet.meeting-service", "runtime_environment": "test",
        "keys": [hub_key.public_jwk()], "permissions": permissions }]});
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    let own = ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("meeting-service-test-1").unwrap()),
        TokenTtl::default(),
        Arc::new(SystemClock),
    );
    let client = InternalServiceClient::new(
        Arc::new(own),
        InternalTargets::new(RuntimeEnvironment::Test),
        Duration::from_secs(1),
    )
    .unwrap();
    Bridge {
        client: Arc::new(client),
        verifier: Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock))),
        scope: scope.clone(),
    }
}

struct Hub {
    app: TestApp,
    signer: ServiceTokenSigner,
    scope: DelegatedPlatformContext,
    /// Receiver granting both permissions.
    router: Router,
    /// Receiver granting `meeting-communications.read` only.
    read_only: Router,
    /// Receiver granting `meeting-communications.create` only.
    create_only: Router,
    /// Receiver granting `communications-account.read` only (the Account routes).
    account_reader: Router,
}

impl Hub {
    async fn start() -> Option<Self> {
        let app = TestApp::new().await?;
        migration::CommunicationsMigrator::up(&app.db.conn, None)
            .await
            .expect("communications migrations apply");
        // The previous image's exact-set readiness must survive the facade history.
        assert!(migration::communications_ready(&app.db.conn).await.unwrap());
        migration::CommunicationsFacadeMigrator::up(&app.db.conn, None)
            .await
            .expect("communications facade migrations apply");
        assert!(migration::communications_ready(&app.db.conn).await.unwrap());
        assert!(migration::communications_facade_ready(&app.db.conn)
            .await
            .unwrap());
        let (signer, public) = hub();
        let scope = scope();
        let router_with = |permissions: &[&str]| {
            let mut state = app.state.clone();
            state.communications = Some(Arc::new(bridge(&public, permissions, &scope)));
            backend::routes::create_routes(state)
                .route_layer(axum::middleware::from_fn(route_coverage::record))
        };
        let router = router_with(&[CREATE, READ]);
        let read_only = router_with(&[READ]);
        let create_only = router_with(&[CREATE]);
        let account_reader = router_with(&[ACCOUNT_READ]);
        Some(Self {
            app,
            signer,
            scope,
            router,
            read_only,
            create_only,
            account_reader,
        })
    }

    /// A registered account linked to a fresh principal in the governed environment.
    async fn account(&self, name: &str) -> (User, Uuid) {
        let user = self.app.user(name).await;
        let principal = Uuid::new_v4();
        self.app
            .db
            .conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO communication_principal_links(subject_kind,subject_id,environment_id,principal_id,principal_kind) VALUES('USER',$1,$2,$3,'NATIVE_CF_USER')",
                [
                    user.id.into(),
                    self.scope.application_environment_id().into(),
                    principal.into(),
                ],
            ))
            .await
            .unwrap();
        (user, principal)
    }

    async fn send(
        &self,
        router: &Router,
        path: &str,
        delegated: Option<DelegatedPlatformContext>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(scope) = delegated {
            let signed = self
                .signer
                .sign(
                    &OutboundServiceRequest::new_operation(ServiceId::meeting_service())
                        .with_delegated_context(scope),
                )
                .unwrap();
            for (k, v) in signed.headers().iter() {
                request = request.header(k, v);
            }
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn as_principal(&self, principal: Uuid) -> Option<DelegatedPlatformContext> {
        Some(
            self.scope
                .clone()
                .with_initiating_principal(principal, DelegatedPrincipalKind::NativeCfUser),
        )
    }

    async fn instant(&self, user: &User, principal: Uuid, key: Uuid) -> (StatusCode, Value) {
        self.send(
            &self.router,
            INSTANT_MEETING_PATH,
            self.as_principal(principal),
            json!({ "principalId": principal, "accountReference": format!("cfmeet-user:{}", user.id),
                    "idempotencyKey": key, "title": "Chat huddle" }),
        )
        .await
    }

    async fn resolve(&self, user: &User, principal: Uuid, code: &str) -> (StatusCode, Value) {
        self.send(
            &self.router,
            RESOLVE_MEETING_PATH,
            self.as_principal(principal),
            json!({ "principalId": principal, "accountReference": format!("cfmeet-user:{}", user.id),
                    "meetingCode": code }),
        )
        .await
    }
}

macro_rules! hub {
    () => {
        match Hub::start().await {
            Some(h) => h,
            None => return,
        }
    };
}

#[tokio::test]
async fn mt_routes_require_the_signed_hub_with_the_named_permission() {
    let hub = hub!();
    let (alice, principal) = hub.account("alice").await;
    let body = json!({ "principalId": principal, "accountReference": format!("cfmeet-user:{}", alice.id),
                       "idempotencyKey": Uuid::new_v4() });

    // Unsigned: never a user, guest or anonymous identity.
    let (s, _) = hub
        .send(&hub.router, INSTANT_MEETING_PATH, None, body.clone())
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Read permission does not create.
    let (s, v) = hub
        .send(
            &hub.read_only,
            INSTANT_MEETING_PATH,
            hub.as_principal(principal),
            body.clone(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        hub.app
            .scalar(&format!(
                "SELECT COUNT(*) FROM meeting_spaces WHERE owner_user_id = '{}'",
                alice.id
            ))
            .await,
        0
    );

    // A different delegated scope is unavailable (404), not a permission oracle.
    let other = scope().with_initiating_principal(principal, DelegatedPrincipalKind::NativeCfUser);
    let (s, _) = hub
        .send(&hub.router, INSTANT_MEETING_PATH, Some(other), body.clone())
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Unknown fields are refused.
    let mut extra = body.clone();
    extra["ownerUserId"] = json!(Uuid::new_v4());
    let (s, _) = hub
        .send(
            &hub.router,
            INSTANT_MEETING_PATH,
            hub.as_principal(principal),
            extra,
        )
        .await;
    assert!(s.is_client_error(), "{s}");
}

#[tokio::test]
async fn mt_routes_act_only_for_the_delegated_principals_own_active_account() {
    let hub = hub!();
    let (alice, alice_p) = hub.account("alice").await;
    let (bob, bob_p) = hub.account("bob").await;
    let key = Uuid::new_v4();

    // Body principal differs from the delegated initiating principal.
    let (s, _) = hub
        .send(
            &hub.router,
            INSTANT_MEETING_PATH,
            hub.as_principal(bob_p),
            json!({ "principalId": alice_p, "accountReference": format!("cfmeet-user:{}", alice.id),
                    "idempotencyKey": key }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Bob's principal naming Alice's account: the mapping does not match.
    let (s, _) = hub
        .send(
            &hub.router,
            INSTANT_MEETING_PATH,
            hub.as_principal(bob_p),
            json!({ "principalId": bob_p, "accountReference": format!("cfmeet-user:{}", alice.id),
                    "idempotencyKey": key }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = hub.resolve(&alice, bob_p, "abc-defg-hij").await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // A non-user delegated principal kind is refused.
    let (s, _) = hub
        .send(
            &hub.router,
            INSTANT_MEETING_PATH,
            Some(
                hub.scope
                    .clone()
                    .with_initiating_principal(bob_p, DelegatedPrincipalKind::BotOrService),
            ),
            json!({ "principalId": bob_p, "accountReference": format!("cfmeet-user:{}", bob.id),
                    "idempotencyKey": key }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Suspended and deactivated accounts are refused.
    for status in ["suspended", "inactive"] {
        hub.app
            .db
            .conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!("UPDATE users SET status = '{status}' WHERE id = $1"),
                [bob.id.into()],
            ))
            .await
            .unwrap();
        let (s, _) = hub.instant(&bob, bob_p, Uuid::new_v4()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{status}");
    }
    assert_eq!(
        hub.app
            .scalar("SELECT COUNT(*) FROM communication_instant_meetings")
            .await,
        0
    );
}

#[tokio::test]
async fn mt1_creates_one_owned_live_meeting_per_idempotency_key() {
    let hub = hub!();
    let (alice, principal) = hub.account("alice").await;
    let key = Uuid::new_v4();

    let (s, first) = hub.instant(&alice, principal, key).await;
    assert_eq!(s, StatusCode::OK, "{first}");
    assert_eq!(first["schemaVersion"], 1);
    assert_eq!(first["replayed"], false);
    let space_id = uuid(&first["meetingSpaceId"]);
    let conference_id = uuid(&first["conferenceId"]);

    // Canonical domain path: owned by Alice, titled, conference LIVE.
    assert_eq!(
        hub.app
            .scalar(&format!(
                "SELECT COUNT(*) FROM meeting_spaces WHERE id = '{space_id}' AND owner_user_id = '{}' AND title = 'Chat huddle' AND meeting_code = '{}'",
                alice.id,
                first["meetingCode"].as_str().unwrap()
            ))
            .await,
        1
    );
    assert_eq!(
        hub.app
            .scalar(&format!(
                "SELECT COUNT(*) FROM conference_sessions WHERE id = '{conference_id}' AND meeting_space_id = '{space_id}' AND status = 'LIVE'"
            ))
            .await,
        1
    );

    // Replay returns the same meeting and creates nothing.
    let (s, again) = hub.instant(&alice, principal, key).await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(again["replayed"], true);
    for field in ["meetingSpaceId", "conferenceId", "meetingCode", "createdAt"] {
        assert_eq!(again[field], first[field], "{field}");
    }
    let owned = format!(
        "SELECT COUNT(*) FROM meeting_spaces WHERE owner_user_id = '{}'",
        alice.id
    );
    assert_eq!(hub.app.scalar(&owned).await, 1);

    // Concurrent replays of a new key still create exactly one meeting.
    let key2 = Uuid::new_v4();
    let (a, b) = tokio::join!(
        hub.instant(&alice, principal, key2),
        hub.instant(&alice, principal, key2)
    );
    assert_eq!((a.0, b.0), (StatusCode::OK, StatusCode::OK));
    assert_eq!(a.1["meetingSpaceId"], b.1["meetingSpaceId"]);
    assert_eq!(hub.app.scalar(&owned).await, 2);

    // The response carries public references only.
    let keys: Vec<_> = first.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys.len(),
        6,
        "unexpected response fields: {keys:?} (no passcode, token or media credential)"
    );

    // Title over 120 characters is refused before creation.
    let (s, _) = hub
        .send(
            &hub.router,
            INSTANT_MEETING_PATH,
            hub.as_principal(principal),
            json!({ "principalId": principal, "accountReference": format!("cfmeet-user:{}", alice.id),
                    "idempotencyKey": Uuid::new_v4(), "title": "x".repeat(121) }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(hub.app.scalar(&owned).await, 2);
}

#[tokio::test]
async fn mt2_resolves_public_references_and_shareability_by_role() {
    let hub = hub!();
    let (alice, alice_p) = hub.account("alice").await;
    let (bob, bob_p) = hub.account("bob").await;
    let (_, created) = hub.instant(&alice, alice_p, Uuid::new_v4()).await;
    let code = created["meetingCode"].as_str().unwrap().to_string();

    let (s, own) = hub.resolve(&alice, alice_p, &code).await;
    assert_eq!(s, StatusCode::OK, "{own}");
    assert_eq!(own["schemaVersion"], 1);
    assert_eq!(own["meetingSpaceId"], created["meetingSpaceId"]);
    assert_eq!(own["meetingCode"], code.as_str());
    assert_eq!(own["title"], "Chat huddle");
    assert_eq!(own["role"], "OWNER");
    assert_eq!(own["shareable"], true);
    assert_eq!(own["latestConference"]["id"], created["conferenceId"]);
    assert_eq!(own["latestConference"]["status"], "LIVE");
    let mut keys: Vec<_> = own.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "latestConference",
            "meetingCode",
            "meetingSpaceId",
            "role",
            "schemaVersion",
            "shareable",
            "title"
        ]
    );
    let mut conference_keys: Vec<_> = own["latestConference"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    conference_keys.sort();
    assert_eq!(conference_keys, ["id", "scheduledStartAt", "status"]);

    // Another user's meeting: resolvable by code, but neither theirs to share nor a role.
    let (s, other) = hub.resolve(&bob, bob_p, &code).await;
    assert_eq!(s, StatusCode::OK, "{other}");
    assert_eq!(other["role"], "NONE");
    assert_eq!(other["shareable"], false);
    assert_eq!(other["meetingSpaceId"], created["meetingSpaceId"]);

    // Unknown code, and identifiers that are not meeting codes.
    let (s, _) = hub.resolve(&bob, bob_p, "zzz-zzzz-zzz").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let space_id = created["meetingSpaceId"].as_str().unwrap().to_string();
    let (s, _) = hub.resolve(&bob, bob_p, &space_id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Create permission alone does not read.
    let (s, _) = hub
        .send(
            &hub.create_only,
            RESOLVE_MEETING_PATH,
            hub.as_principal(bob_p),
            json!({ "principalId": bob_p, "accountReference": format!("cfmeet-user:{}", bob.id),
                    "meetingCode": code }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// SEC-020: the Account-owned internal routes (`/internal/v1/accounts/communications/*`)
/// answer only the signed Hub holding `communications-account.read`, delegated to the
/// governed personal scope; any other caller or scope learns nothing about an account.
#[tokio::test]
async fn account_routes_require_the_signed_hub_with_the_account_read_permission() {
    let hub = hub!();
    let (alice, principal) = hub.account("alice").await;
    let status = "/internal/v1/accounts/communications/status";
    let backfill = "/internal/v1/accounts/communications/backfill";
    let status_body = json!({ "accountReference": format!("cfmeet-user:{}", alice.id) });
    let backfill_body = json!({ "limit": 1 });
    for (path, body) in [(status, &status_body), (backfill, &backfill_body)] {
        // Unsigned (a browser / any unauthenticated caller).
        let (s, v) = hub
            .send(&hub.account_reader, path, None, body.clone())
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}: {v}");
        assert!(!v.to_string().contains(&principal.to_string()), "{v}");
        // Signed Hub without the account permission (meeting permissions only).
        let (s, v) = hub
            .send(&hub.router, path, Some(hub.scope.clone()), body.clone())
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}: {v}");
        assert!(!v.to_string().contains(&principal.to_string()), "{v}");
        // The permission, delegated to another scope: not found, not an oracle.
        let (s, v) = hub
            .send(&hub.account_reader, path, Some(scope()), body.clone())
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}: {v}");
        assert!(!v.to_string().contains(&principal.to_string()), "{v}");
    }
    // Positive control: the governed scope reads the account's eligibility and link.
    let (s, v) = hub
        .send(
            &hub.account_reader,
            status,
            Some(hub.scope.clone()),
            status_body,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "ACTIVE", "{v}");
    assert_eq!(v["principalId"], json!(principal), "{v}");
}

//! Shared harness for Telephony integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets
//! its own database cloned from a freshly migrated template. Without it the tests FAIL,
//! unless `CFMEET_SKIP_DB_TESTS=1` (then they print an explicit SKIPPED line).
//!
//! Calling and Privacy Relay are deterministic fakes of Telephony's ports; requests use
//! REAL service tokens (Ed25519) verified by Telephony's receiver-authoritative trust
//! bundle. The interconnect is the deterministic test adapter (never a carrier).
#![allow(dead_code)]

pub mod edge;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, DelegatedPlatformContext, KeyId, OutboundServiceRequest,
    RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use telephony::{
    clock::ManualClock,
    config::{ServiceAuthRuntime, TelecomFlags},
    internal,
    ports::{
        CallingPort, DcnInbound, DcnRouting, DcnScope, DcnVoiceRoute, InboundCaller,
        InboundMapping, NumberingPort, PortError, PrivacyRelayPort, ResolveForExecution,
        ResolvedExecution, VerifiedDcnIdentity,
    },
    providers::{deterministic::DeterministicProvider, ExecutionNumber, ProviderRegistry},
    routes,
    service::{ExecutionSettings, TelephonyService},
    state::AppState,
};
use telephony_migration::MigratorTrait;
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

pub fn admin_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!("SKIPPED: database integration test (CFMEET_SKIP_DB_TESTS=1, TEST_DATABASE_URL unset)");
                None
            } else {
                panic!(
                    "TEST_DATABASE_URL is not set. Telephony integration tests require PostgreSQL \
                     (e.g. TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres). \
                     Set CFMEET_SKIP_DB_TESTS=1 to skip explicitly."
                )
            }
        }
    }
}

fn with_db(admin: &str, db: &str) -> String {
    let (base, _) = admin
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL must include a database name");
    format!("{base}/{db}")
}

async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'tel\\_%'",
        ))
        .await
    else {
        return;
    };
    for row in rows {
        let Ok(name) = row.try_get_by_index::<String>(0) else {
            continue;
        };
        let pid = name.split('_').nth(2).and_then(|p| p.parse::<u32>().ok());
        match pid {
            Some(pid) if std::path::Path::new(&format!("/proc/{pid}")).exists() => {}
            _ => {
                let _ = conn
                    .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                    .await;
            }
        }
    }
}

async fn template(admin: &str) -> String {
    TEMPLATE
        .get_or_init(|| async {
            let name = format!(
                "tel_tmpl_{}_{}",
                std::process::id(),
                Uuid::new_v4().simple()
            );
            let conn = Database::connect(admin).await.expect("connect admin db");
            if std::path::Path::new("/proc/self").exists() {
                drop_databases_of_dead_runs(&conn).await;
            }
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            telephony_migration::Migrator::up(&tmpl, None)
                .await
                .expect("migrations apply");
            tmpl.close().await.ok();
            name
        })
        .await
        .clone()
}

pub struct TestDb {
    pub conn: DatabaseConnection,
    pub url: String,
    name: String,
    admin: String,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        let name = format!("tel_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = CREATE_LOCK.lock().await;
            let conn = Database::connect(&admin).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        Self::connect(admin, name).await
    }

    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        let name = format!("tel_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        let conn = Database::connect(&admin).await.unwrap();
        conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        conn.close().await.ok();
        Self::connect(admin, name).await
    }

    async fn connect(admin: String, name: String) -> Option<Self> {
        let url = with_db(&admin, &name);
        let mut opts = sea_orm::ConnectOptions::new(url.clone());
        opts.max_connections(20).sqlx_logging(false);
        let conn = Database::connect(opts).await.unwrap();
        Some(Self {
            conn,
            url,
            name,
            admin,
        })
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
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

pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2031-03-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

// ---------------------------------------------------------------------------
// Fake Privacy Relay (contact resolution, inbound mapping, caller protection).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrMode {
    Normal,
    Unavailable,
    Deny,
}

pub struct FakePrivacyRelay {
    pub mode: StdMutex<PrMode>,
    /// contact reference -> (E.164, country)
    pub contacts: StdMutex<HashMap<Uuid, (String, String)>>,
    /// The relay identity the session presents.
    pub relay_number: StdMutex<Option<String>>,
    /// Privacy Relay's assignment reference of that identity.
    pub assignment_ref: StdMutex<Option<Uuid>>,
    pub resolutions: StdMutex<Vec<(Uuid, Uuid, Uuid)>>,
    /// called relay number -> mapping
    pub mappings: StdMutex<HashMap<String, InboundMapping>>,
    pub protected: StdMutex<Vec<Uuid>>,
    pub allowed_targets: StdMutex<Vec<Value>>,
    /// Part IV L: the Numbering grants DCN callers were protected under.
    pub dcn_protected: StdMutex<Vec<String>>,
}

impl FakePrivacyRelay {
    pub fn new() -> Self {
        Self {
            mode: StdMutex::new(PrMode::Normal),
            contacts: StdMutex::default(),
            relay_number: StdMutex::new(Some(RELAY_NUMBER.to_string())),
            assignment_ref: StdMutex::new(Some(Uuid::new_v4())),
            resolutions: StdMutex::default(),
            mappings: StdMutex::default(),
            protected: StdMutex::default(),
            allowed_targets: StdMutex::default(),
            dcn_protected: StdMutex::default(),
        }
    }

    pub fn set_mode(&self, mode: PrMode) {
        *self.mode.lock().unwrap() = mode;
    }

    pub fn add_contact(&self, e164: &str, country: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.contacts
            .lock()
            .unwrap()
            .insert(id, (e164.to_string(), country.to_string()));
        id
    }

    pub fn resolutions(&self) -> Vec<(Uuid, Uuid, Uuid)> {
        self.resolutions.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl PrivacyRelayPort for FakePrivacyRelay {
    async fn resolve_for_execution(
        &self,
        r: &ResolveForExecution,
    ) -> Result<ResolvedExecution, PortError> {
        match *self.mode.lock().unwrap() {
            PrMode::Unavailable => return Err(PortError::Unavailable),
            PrMode::Deny => return Err(PortError::Denied("CONTACT_RESOLUTION_DENIED".into())),
            PrMode::Normal => {}
        }
        self.resolutions.lock().unwrap().push((
            r.contact_reference,
            r.grant_id,
            r.execution_attempt_id,
        ));
        let (e164, country) = self
            .contacts
            .lock()
            .unwrap()
            .get(&r.contact_reference)
            .cloned()
            .ok_or(PortError::NotFound)?;
        Ok(ResolvedExecution {
            destination: ExecutionNumber::parse(&e164).unwrap(),
            country: Some(country),
            caller_identity: self
                .relay_number
                .lock()
                .unwrap()
                .as_deref()
                .and_then(ExecutionNumber::parse),
            relay_assignment_ref: *self.assignment_ref.lock().unwrap(),
        })
    }

    async fn resolve_inbound(
        &self,
        called: &ExecutionNumber,
        _: Option<&str>,
        _: DateTime<Utc>,
        _: Option<DateTime<Utc>>,
        _: &str,
    ) -> Result<InboundMapping, PortError> {
        if *self.mode.lock().unwrap() == PrMode::Unavailable {
            return Err(PortError::Unavailable);
        }
        self.mappings
            .lock()
            .unwrap()
            .get(called.expose())
            .cloned()
            .ok_or(PortError::Denied("RELAY_MAPPING_NOT_ACTIVE".into()))
    }

    async fn protect_inbound_caller(
        &self,
        _: Uuid,
        _: &ExecutionNumber,
        _: &str,
    ) -> Result<InboundCaller, PortError> {
        let id = Uuid::new_v4();
        self.protected.lock().unwrap().push(id);
        Ok(InboundCaller {
            contact_reference: id,
            allowed_targets: self.allowed_targets.lock().unwrap().clone(),
        })
    }

    async fn protect_dcn_caller(
        &self,
        number_grant: &str,
        _: &ExecutionNumber,
        _: &str,
    ) -> Result<Uuid, PortError> {
        match *self.mode.lock().unwrap() {
            PrMode::Unavailable => return Err(PortError::Unavailable),
            PrMode::Deny => return Err(PortError::Denied("RELAY_MAPPING_NOT_ACTIVE".into())),
            PrMode::Normal => {}
        }
        self.dcn_protected
            .lock()
            .unwrap()
            .push(number_grant.to_string());
        Ok(Uuid::new_v4())
    }
}

// ---------------------------------------------------------------------------
// Fake Numbering (Part IV L): DCN inbound resolution, caller-identity grants, voice
// route readiness. Grants are opaque strings here; single use per operation.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FakeDcn {
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub scope: DcnScope,
    /// `None`: assigned but routed nowhere (NOT_ROUTED).
    pub target: Option<(String, Uuid)>,
    /// `Some(reason)`: Numbering answers UNAVAILABLE.
    pub unavailable: Option<String>,
}

#[derive(Clone)]
pub struct FakeGrant {
    pub e164: String,
    pub dcn: FakeDcn,
    pub operation_ref: String,
    pub consumed: bool,
    pub revoked: bool,
}

pub struct FakeNumbering {
    pub available: StdMutex<bool>,
    /// called E.164 -> the DCN
    pub dcns: StdMutex<HashMap<String, FakeDcn>>,
    /// grant token -> grant
    pub grants: StdMutex<HashMap<String, FakeGrant>>,
    pub voice_routes: StdMutex<HashMap<Uuid, (String, Option<DcnVoiceRoute>)>>,
    pub acknowledged: StdMutex<Vec<(Uuid, Uuid, i64)>>,
    pub verifications: StdMutex<Vec<String>>,
}

impl FakeNumbering {
    pub fn new() -> Self {
        Self {
            available: StdMutex::new(true),
            dcns: StdMutex::default(),
            grants: StdMutex::default(),
            voice_routes: StdMutex::default(),
            acknowledged: StdMutex::default(),
            verifications: StdMutex::default(),
        }
    }

    pub fn set_available(&self, up: bool) {
        *self.available.lock().unwrap() = up;
    }

    pub fn add_dcn(&self, e164: &str, dcn: FakeDcn) {
        self.dcns.lock().unwrap().insert(e164.to_string(), dcn);
    }

    /// Issues a CALLER_IDENTITY grant for `operation_ref`; returns the token.
    pub fn issue_caller_identity(&self, e164: &str, dcn: FakeDcn, operation_ref: &str) -> String {
        let token = format!("eyJhbGciOiJFZERTQSJ9.{}.c2ln", Uuid::new_v4().simple());
        self.grants.lock().unwrap().insert(
            token.clone(),
            FakeGrant {
                e164: e164.to_string(),
                dcn,
                operation_ref: operation_ref.to_string(),
                consumed: false,
                revoked: false,
            },
        );
        token
    }
}

#[async_trait::async_trait]
impl NumberingPort for FakeNumbering {
    async fn resolve_inbound_voice(
        &self,
        called: &ExecutionNumber,
        _: &str,
    ) -> Result<DcnInbound, PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        let Some(dcn) = self.dcns.lock().unwrap().get(called.expose()).cloned() else {
            return Ok(DcnInbound::Unavailable("NUMBER_NOT_ASSIGNED".into()));
        };
        if let Some(reason) = dcn.unavailable {
            return Ok(DcnInbound::Unavailable(reason));
        }
        let Some((kind, target)) = dcn.target else {
            return Ok(DcnInbound::NotRouted);
        };
        Ok(DcnInbound::Routed(Box::new(DcnRouting {
            number_resource_id: dcn.number_resource_id,
            assignment_id: dcn.assignment_id,
            routing_revision: dcn.routing_revision,
            ownership_class: "ENTERPRISE".into(),
            scope: dcn.scope,
            target_kind: kind,
            target_ref: target,
            grant: format!(
                "eyJhbGciOiJFZERTQSJ9.inbound{}.c2ln",
                Uuid::new_v4().simple()
            ),
            grant_id: Uuid::new_v4(),
        })))
    }

    async fn verify_caller_identity(
        &self,
        grant: &str,
        operation_ref: &str,
        _: &str,
    ) -> Result<Result<VerifiedDcnIdentity, String>, PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        self.verifications
            .lock()
            .unwrap()
            .push(operation_ref.to_string());
        let mut grants = self.grants.lock().unwrap();
        let Some(g) = grants.get_mut(grant) else {
            return Ok(Err("GRANT_INVALID".into()));
        };
        if g.revoked {
            return Ok(Err("GRANT_REVOKED".into()));
        }
        if g.consumed {
            return Ok(Err("GRANT_ALREADY_USED".into()));
        }
        if g.operation_ref != operation_ref {
            return Ok(Err("OPERATION_MISMATCH".into()));
        }
        g.consumed = true;
        Ok(Ok(VerifiedDcnIdentity {
            identity: ExecutionNumber::parse(&g.e164).unwrap(),
            number_resource_id: g.dcn.number_resource_id,
            assignment_id: g.dcn.assignment_id,
            routing_revision: g.dcn.routing_revision,
            grant_id: Uuid::new_v4(),
            scope: g.dcn.scope,
        }))
    }

    async fn voice_route(
        &self,
        number_resource_id: Uuid,
    ) -> Result<(ExecutionNumber, Option<DcnVoiceRoute>), PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        let (e164, route) = self
            .voice_routes
            .lock()
            .unwrap()
            .get(&number_resource_id)
            .cloned()
            .ok_or(PortError::NotFound)?;
        Ok((ExecutionNumber::parse(&e164).unwrap(), route))
    }

    async fn acknowledge_voice_route(
        &self,
        number_resource_id: Uuid,
        route: &DcnVoiceRoute,
    ) -> Result<(), PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        self.acknowledged.lock().unwrap().push((
            number_resource_id,
            route.route_ref,
            route.route_revision,
        ));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fake Calling (normalized events, inbound offers).
// ---------------------------------------------------------------------------

pub struct FakeCalling {
    pub available: StdMutex<bool>,
    pub events: StdMutex<Vec<Value>>,
    pub offers: StdMutex<Vec<Value>>,
    pub offer_route_class: StdMutex<String>,
    /// Part IV L: DCN inbound offers; `dcn_refusal` makes Calling refuse them.
    pub dcn_offers: StdMutex<Vec<Value>>,
    pub dcn_refusal: StdMutex<Option<&'static str>>,
    /// Part F: enterprise dialog offers and capture reports received.
    pub enterprise_offers: StdMutex<Vec<Value>>,
    pub captures: StdMutex<Vec<Value>>,
    /// How the capture receiver answers: OK, UNAVAILABLE (503), DENIED (400), NOT_FOUND.
    pub capture_mode: StdMutex<&'static str>,
    /// How the event receiver answers when available: OK, DENIED (400 VALIDATION_ERROR),
    /// NOT_FOUND (404), AUTH (401 INTERNAL_AUTHENTICATION_FAILED).
    pub event_mode: StdMutex<&'static str>,
    /// The correlation id each delivered event / keypad / capture continued (event id or
    /// capture id -> correlation; `None`: a fresh one would be minted).
    pub correlations: StdMutex<Vec<(Value, Option<String>)>>,
    /// Every delivery attempt of an event (event ids, including refused ones).
    pub event_attempts: StdMutex<Vec<Value>>,
}

impl FakeCalling {
    pub fn new() -> Self {
        Self {
            available: StdMutex::new(true),
            events: StdMutex::default(),
            offers: StdMutex::default(),
            offer_route_class: StdMutex::new("PSTN_TO_APP".into()),
            dcn_offers: StdMutex::default(),
            dcn_refusal: StdMutex::new(None),
            enterprise_offers: StdMutex::default(),
            captures: StdMutex::default(),
            capture_mode: StdMutex::new("OK"),
            event_mode: StdMutex::new("OK"),
            correlations: StdMutex::default(),
            event_attempts: StdMutex::default(),
        }
    }

    pub fn set_event_mode(&self, mode: &'static str) {
        *self.event_mode.lock().unwrap() = mode;
    }

    /// The correlation id the delivery of `id` (an event id / capture id) continued.
    pub fn correlation_of(&self, id: &Value) -> Option<Option<String>> {
        self.correlations
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(k, _)| k == id)
            .map(|(_, c)| c.clone())
    }

    pub fn captures(&self) -> Vec<Value> {
        self.captures.lock().unwrap().clone()
    }

    pub fn set_capture_mode(&self, mode: &'static str) {
        *self.capture_mode.lock().unwrap() = mode;
    }

    pub fn set_available(&self, up: bool) {
        *self.available.lock().unwrap() = up;
    }

    pub fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    pub fn event_types_for(&self, leg: Uuid) -> Vec<String> {
        self.events()
            .iter()
            .filter(|e| e["telephonyLegId"] == json!(leg))
            .map(|e| e["eventType"].as_str().unwrap().to_string())
            .collect()
    }
}

#[async_trait::async_trait]
impl CallingPort for FakeCalling {
    async fn deliver_event(
        &self,
        payload: &Value,
        correlation_id: Option<&str>,
    ) -> Result<(), PortError> {
        self.event_attempts
            .lock()
            .unwrap()
            .push(payload["eventId"].clone());
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        match *self.event_mode.lock().unwrap() {
            "DENIED" => return Err(PortError::Denied("VALIDATION_ERROR".into())),
            "NOT_FOUND" => return Err(PortError::NotFound),
            "AUTH" => return Err(PortError::Denied("INTERNAL_AUTHENTICATION_FAILED".into())),
            _ => {}
        }
        self.correlations.lock().unwrap().push((
            payload["eventId"].clone(),
            correlation_id.map(str::to_string),
        ));
        self.events.lock().unwrap().push(payload.clone());
        Ok(())
    }

    async fn deliver_keypad(
        &self,
        payload: &Value,
        correlation_id: Option<&str>,
    ) -> Result<(), PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        self.correlations.lock().unwrap().push((
            payload["telephonyLegId"].clone(),
            correlation_id.map(str::to_string),
        ));
        Ok(())
    }

    async fn offer_inbound(&self, offer: &Value) -> Result<Value, PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        self.offers.lock().unwrap().push(offer.clone());
        Ok(json!({
            "callId": Uuid::new_v4(),
            "callLegId": Uuid::new_v4(),
            "routeClass": *self.offer_route_class.lock().unwrap(),
        }))
    }

    async fn offer_dcn_inbound(&self, offer: &Value) -> Result<Value, PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        if let Some(code) = *self.dcn_refusal.lock().unwrap() {
            return Err(PortError::Denied(code.into()));
        }
        self.dcn_offers.lock().unwrap().push(offer.clone());
        Ok(json!({
            "callId": Uuid::new_v4(),
            "callLegId": Uuid::new_v4(),
            "routeClass": "PSTN_TO_APP",
        }))
    }

    async fn offer_enterprise(&self, offer: &Value) -> Result<Value, PortError> {
        if !*self.available.lock().unwrap() {
            return Err(PortError::Unavailable);
        }
        self.enterprise_offers.lock().unwrap().push(offer.clone());
        Ok(json!({
            "callId": Uuid::new_v4(),
            "callLegId": Uuid::new_v4(),
            "routeClass": "ENTERPRISE_INTERNAL",
        }))
    }

    async fn deliver_capture(
        &self,
        report: &Value,
        correlation_id: Option<&str>,
    ) -> Result<Value, PortError> {
        match *self.capture_mode.lock().unwrap() {
            "UNAVAILABLE" => return Err(PortError::Unavailable),
            "DENIED" => return Err(PortError::Denied("VALIDATION_ERROR".into())),
            "NOT_FOUND" => return Err(PortError::NotFound),
            _ => {}
        }
        self.correlations.lock().unwrap().push((
            report["captureId"].clone(),
            correlation_id.map(str::to_string),
        ));
        self.captures.lock().unwrap().push(report.clone());
        Ok(json!({
            "captureId": report["captureId"],
            "status": if report["outcome"] == "COMPLETED" { "STORED" } else { "ACKNOWLEDGED" },
        }))
    }
}

// ---------------------------------------------------------------------------
// Service identities.
// ---------------------------------------------------------------------------

pub const CALLING_PERMISSIONS: &[&str] = &[
    "telephony-leg.originate",
    "telephony-leg.terminate",
    "telephony-leg.hold",
    "telephony-leg.resume",
    "telephony-bridge.manage",
    "telephony-status.read",
];

pub fn trust_bundle_json(
    receiver: &str,
    env: RuntimeEnvironment,
    grants: &[(&str, Vec<cf_service_auth::PublicJwk>, &[&str])],
) -> String {
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": env,
        "trusted_issuers": grants.iter().map(|(issuer, keys, permissions)| json!({
            "issuer": issuer,
            "audience": receiver,
            "runtime_environment": env,
            "keys": keys,
            "permissions": permissions,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub struct Identities {
    pub runtime: ServiceAuthRuntime,
    pub calling: Arc<ServiceTokenSigner>,
    pub privacy_relay: Arc<ServiceTokenSigner>,
    pub untrusted: Arc<ServiceTokenSigner>,
}

pub fn identities(calling_permissions: &[&str]) -> Identities {
    let env = RuntimeEnvironment::Test;
    let clock: cf_service_auth::SharedClock = Arc::new(cf_service_auth::SystemClock);
    let me = ServiceSigningKey::generate(KeyId::parse("telephony-test-1").unwrap());
    let calling_key = ServiceSigningKey::generate(KeyId::parse("calling-test-1").unwrap());
    let relay_key = ServiceSigningKey::generate(KeyId::parse("privacy-relay-test-1").unwrap());
    let untrusted_key = ServiceSigningKey::generate(KeyId::parse("untrusted-test-1").unwrap());
    let policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::TELEPHONY,
            env,
            &[
                (
                    cf_service_auth::services::CALLING,
                    vec![calling_key.public_jwk()],
                    calling_permissions,
                ),
                // Privacy Relay is not a Telephony issuer at all (no permission is
                // grantable to it here).
            ],
        ),
        &ServiceId::telephony(),
        env,
    )
    .unwrap();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::telephony(),
        env,
        me,
        TokenTtl::default(),
        clock.clone(),
    ));
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock.clone()));
    let client = Arc::new(
        InternalServiceClient::new(
            signer.clone(),
            InternalTargets::new(env),
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    );
    Identities {
        runtime: ServiceAuthRuntime::new(signer, verifier, client).unwrap(),
        calling: Arc::new(ServiceTokenSigner::new(
            ServiceId::calling(),
            env,
            calling_key,
            TokenTtl::default(),
            clock.clone(),
        )),
        privacy_relay: Arc::new(ServiceTokenSigner::new(
            ServiceId::privacy_relay(),
            env,
            relay_key,
            TokenTtl::default(),
            clock.clone(),
        )),
        untrusted: Arc::new(ServiceTokenSigner::new(
            ServiceId::parse("cfmeet.numbering").unwrap(),
            env,
            untrusted_key,
            TokenTtl::default(),
            clock,
        )),
    }
}

/// A delegated application scope.
#[derive(Debug, Clone)]
pub struct Consumer {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
}

impl Consumer {
    pub fn new() -> Self {
        Self {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            service_account_id: Uuid::new_v4(),
        }
    }

    pub fn delegated(&self) -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            self.tenant_id,
            self.application_id,
            self.environment_id,
            ApplicationEnvironmentKind::Production,
            self.service_account_id,
        )
    }
}

// ---------------------------------------------------------------------------
// Harness.
// ---------------------------------------------------------------------------

pub const CALLBACK_SECRET: &[u8] = b"deterministic-test-callback-secret-0123456789";
/// The previous callback secret of provider a (rotation overlap, Part H §44).
pub const PREVIOUS_CALLBACK_SECRET: &[u8] = b"deterministic-test-previous-secret-9876543210";
/// The platform relay number the fake Privacy Relay presents (public, platform-owned).
pub const RELAY_NUMBER: &str = "+2342010000001";

pub struct Response {
    pub status: StatusCode,
    pub json: Value,
    pub text: String,
}

impl Response {
    pub fn code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }
}

pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    pub flags: TelecomFlags,
    pub provider_a: DeterministicProvider,
    pub provider_b: DeterministicProvider,
    pub privacy: Arc<FakePrivacyRelay>,
    pub calling: Arc<FakeCalling>,
    pub numbering: Arc<FakeNumbering>,
    pub ids: Identities,
    pub svc: TelephonyService,
    pub app: Router,
    pub consumer: Consumer,
}

pub fn all_on() -> TelecomFlags {
    TelecomFlags::new(true, true, true, true)
}

pub fn fast_settings() -> ExecutionSettings {
    ExecutionSettings {
        ring_timeout: std::time::Duration::from_secs(60),
        provider_request_timeout: std::time::Duration::from_millis(500),
        command_lease: std::time::Duration::from_secs(30),
        reconcile_delay: std::time::Duration::from_secs(5),
        reconcile_backoff_cap: std::time::Duration::from_secs(60),
        reconcile_escalate_after: 3,
        ringing_drift_after: std::time::Duration::from_secs(120),
        provider_event_retention: std::time::Duration::from_secs(3600),
        callback_auth_failure_incident_threshold: 5,
        callback_invalid_incident_threshold: 5,
        blocked_destination_classes: telephony::destinations::DestinationClass::DEFAULT_BLOCKED
            .to_vec(),
        unclassified_destination_policy: telephony::destinations::UnclassifiedPolicy::Allow,
        incident_retention: std::time::Duration::from_secs(365 * 24 * 3600),
    }
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with(CALLING_PERMISSIONS, all_on()).await
    }

    pub async fn with(calling_permissions: &[&str], flags: TelecomFlags) -> Option<Self> {
        Self::build(calling_permissions, flags, None).await
    }

    /// With a real SIP / media edge: the interconnects are SIP trunks on that edge
    /// (`sip-a` -> route ng-a -> peer-ng-a, `sip-b` -> ng-b -> peer-ng-b).
    pub async fn with_edge(edge: Arc<dyn telephony::sip::SipMediaEdge>) -> Option<Self> {
        Self::build(CALLING_PERMISSIONS, all_on(), Some(edge)).await
    }

    pub async fn build(
        calling_permissions: &[&str],
        flags: TelecomFlags,
        edge: Option<Arc<dyn telephony::sip::SipMediaEdge>>,
    ) -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        let clock = ManualClock::new(Utc::now());
        let provider_a = DeterministicProvider::new("dp-a", CALLBACK_SECRET)
            .with_previous_secret(Some(PREVIOUS_CALLBACK_SECRET));
        let provider_b = DeterministicProvider::new("dp-b", CALLBACK_SECRET);
        let privacy = Arc::new(FakePrivacyRelay::new());
        let calling = Arc::new(FakeCalling::new());
        let numbering = Arc::new(FakeNumbering::new());
        let ids = identities(calling_permissions);
        let registry = match &edge {
            None => ProviderRegistry::new()
                .with(Arc::new(provider_a.clone()))
                .with(Arc::new(provider_b.clone())),
            Some(edge) => {
                let caps = vec![
                    telephony::domain::ProviderCapability::OutboundVoice,
                    telephony::domain::ProviderCapability::InboundVoice,
                    telephony::domain::ProviderCapability::SipTrunk,
                    telephony::domain::ProviderCapability::StatusQuery,
                    telephony::domain::ProviderCapability::Events,
                    telephony::domain::ProviderCapability::Dtmf,
                    telephony::domain::ProviderCapability::CallerIdSelection,
                ];
                ProviderRegistry::new()
                    .with(Arc::new(
                        telephony::providers::sip_trunk::SipTrunkProvider::new(
                            "sip-a",
                            caps.clone(),
                            edge.clone(),
                        ),
                    ))
                    .with(Arc::new(
                        telephony::providers::sip_trunk::SipTrunkProvider::new(
                            "sip-b",
                            caps,
                            edge.clone(),
                        ),
                    ))
            }
        };
        let mut svc =
            TelephonyService::new(db.conn.clone(), Arc::new(clock.clone()), flags.clone())
                .with_providers(registry)
                .with_privacy_relay(privacy.clone())
                .with_calling(calling.clone())
                .with_numbering(numbering.clone())
                .with_settings(fast_settings());
        if let Some(edge) = &edge {
            svc = svc.with_edge(edge.clone());
        }
        let sip = edge.is_some();
        let app = routes::app(
            AppState::new(svc.clone(), ids.runtime.clone()),
            internal::routes(),
        );
        let h = Self {
            db,
            clock,
            flags,
            provider_a,
            provider_b,
            privacy,
            calling,
            numbering,
            ids,
            svc,
            app,
            consumer: Consumer::new(),
        };
        h.seed(sip).await;
        Some(h)
    }

    /// Two NG providers / routes (a: priority 10, b: priority 20) and the relay number
    /// bound to both.
    pub async fn seed(&self, sip: bool) {
        let now = self.clock_now();
        let (adapter, a, b) = if sip {
            ("SIP_TRUNK", "sip-a", "sip-b")
        } else {
            ("DETERMINISTIC_TEST", "dp-a", "dp-b")
        };
        for (provider, route, priority) in [(a, "ng-a", 10), (b, "ng-b", 20)] {
            self.sql(&format!(
                "INSERT INTO telecom_providers (id, adapter, capabilities, status, credential_ref, max_concurrent_legs, created_at, updated_at, health) \
                 VALUES ('{provider}', '{adapter}', ARRAY['OUTBOUND_VOICE','INBOUND_VOICE','STATUS_QUERY','EVENTS','HEALTH','CAPACITY'], \
                 'ENABLED', 'dp-secret', 100, '{now}', '{now}', 'HEALTHY')"
            ))
            .await;
            self.sql(&format!(
                "INSERT INTO sip_peer_config_refs (id, provider_id, secret_ref, transport, allowed_source_cidrs, max_concurrent_dialogs, created_at, updated_at) \
                 VALUES ('peer-{route}', '{provider}', 'trunk/{route}', 'TLS', ARRAY['192.0.2.0/24']::cidr[], 100, '{now}', '{now}')"
            ))
            .await;
            self.sql(&format!(
                "INSERT INTO telecom_routes (id, provider_id, countries, capabilities, priority, status, sip_peer_config_ref_id, \
                 max_concurrent_legs, estimated_leg_cost_minor, cost_currency, max_call_seconds, created_at, updated_at, health) \
                 VALUES ('{route}', '{provider}', ARRAY['NG'], ARRAY['VOICE_OUTBOUND','VOICE_INBOUND'], {priority}, 'ENABLED', 'peer-{route}', \
                 50, 100, 'NGN', 3600, '{now}', '{now}', 'HEALTHY')"
            ))
            .await;
        }
        self.sql(&format!(
            "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, created_at, updated_at) \
             VALUES ('{RELAY_NUMBER}', 'ng-a', ARRAY['VOICE_OUTBOUND','VOICE_INBOUND'], 'ENABLED', '{now}', '{now}'), \
                    ('{RELAY_NUMBER}', 'ng-b', ARRAY['VOICE_OUTBOUND','VOICE_INBOUND'], 'ENABLED', '{now}', '{now}')"
        ))
        .await;
    }

    pub fn clock_now(&self) -> String {
        use telephony::clock::Clock;
        self.clock.now().to_rfc3339()
    }

    pub fn now(&self) -> DateTime<Utc> {
        use telephony::clock::Clock;
        self.clock.now()
    }

    pub fn advance(&self, seconds: i64) {
        self.clock.advance(Duration::seconds(seconds));
    }

    pub async fn sql(&self, sql: &str) {
        self.db
            .conn
            .execute_unprepared(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    pub async fn try_sql(&self, sql: &str) -> Result<(), String> {
        self.db
            .conn
            .execute_unprepared(sql)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        let row = self
            .db
            .conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get_by_index::<i64>(0).unwrap()
    }

    pub async fn text(&self, sql: &str) -> Option<String> {
        let row = self
            .db
            .conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap()?;
        row.try_get_by_index::<Option<String>>(0).unwrap()
    }

    /// Every text column of every Telephony table (canary scans).
    pub async fn dump(&self) -> String {
        let rows = self
            .db
            .conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'"
                    .to_string(),
            ))
            .await
            .unwrap();
        let mut out = String::new();
        for r in rows {
            let t: String = r.try_get_by_index(0).unwrap();
            let rows = self
                .db
                .conn
                .query_all(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!("SELECT to_jsonb(x)::text FROM {t} x"),
                ))
                .await
                .unwrap();
            for row in rows {
                out.push_str(&row.try_get_by_index::<String>(0).unwrap());
                out.push('\n');
            }
        }
        out
    }

    pub async fn send_as(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        delegated: Option<&Consumer>,
        body: Option<Value>,
    ) -> Response {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::telephony());
        if let Some(c) = delegated {
            outbound = outbound.with_delegated_context(c.delegated());
        }
        let signed = signer.sign(&outbound).unwrap();
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in signed.headers().iter() {
            builder = builder.header(name, value);
        }
        self.raw(builder, body).await
    }

    pub async fn calling(&self, method: Method, path: &str, body: Option<Value>) -> Response {
        self.send_as(&self.ids.calling, method, path, Some(&self.consumer), body)
            .await
    }

    pub async fn raw(
        &self,
        builder: axum::http::request::Builder,
        body: Option<Value>,
    ) -> Response {
        let request = match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Response { status, json, text }
    }

    /// A complete, current ORIGINATE command for a fresh call leg to `contact`.
    pub fn originate_body(&self, contact: Uuid, call_leg: Uuid, generation: i32) -> Value {
        let now = self.now();
        json!({
            "schemaVersion": "1",
            "commandId": Uuid::new_v4(),
            "idempotencyKey": format!("orig-{call_leg}-{generation}"),
            "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            "expectedGeneration": generation,
            "call": {
                "callId": Uuid::new_v4(),
                "callLegId": call_leg,
                "attemptId": Uuid::new_v4(),
                "routeId": Uuid::new_v4(),
                "routeClass": "APP_TO_PSTN",
                "routePolicyVersion": "ROUTING_POLICY_V1.r1",
            },
            "scope": {
                "tenantId": self.consumer.tenant_id,
                "applicationId": self.consumer.application_id,
                "environmentId": self.consumer.environment_id,
            },
            "authorization": {
                "callAuthorizationId": Uuid::new_v4(),
                "callPolicyVersion": "PRIVACY_RELAY_V1",
                "privacyDecisionId": Uuid::new_v4(),
                "contextId": Uuid::new_v4(),
                "contextVersion": 1,
                "relaySessionId": Uuid::new_v4(),
                "relaySessionVersion": 1,
                "decidedAt": now,
                "validUntil": now + Duration::minutes(30),
            },
            "destination": {
                "contactReference": contact,
                "grantId": Uuid::new_v4(),
                "grantSecret": "grant-secret-0123456789abcdefghijklmnopqrstuv",
                "grantExpiresAt": now + Duration::seconds(120),
                "country": "NG",
            },
            "spend": {
                "reservationId": Uuid::new_v4(),
                "reservedAmountMinor": 500,
                "currency": "NGN",
                "maxDurationSeconds": 3600,
            },
            "media": "NONE",
            "candidateRoutes": ["ng-a", "ng-b"],
        })
    }

    /// Originates a leg to a fresh protected contact and returns (telephony leg id, body).
    pub async fn originate(&self) -> (Uuid, Value) {
        let contact = self.privacy.add_contact("+2348031234567", "NG");
        let body = self.originate_body(contact, Uuid::new_v4(), 1);
        let r = self
            .calling(
                Method::POST,
                "/internal/v1/legs/originate",
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
        (
            r.json["leg"]["telephonyLegId"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
            body,
        )
    }

    pub async fn leg_status(&self, leg: Uuid) -> String {
        self.text(&format!(
            "SELECT status FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap()
    }

    pub async fn correlation_of(&self, leg: Uuid) -> String {
        self.text(&format!(
            "SELECT correlation_key FROM provider_call_bindings WHERE telephony_leg_id = '{leg}' ORDER BY created_at DESC LIMIT 1"
        ))
        .await
        .unwrap()
    }

    pub async fn run(&self) {
        self.svc.sweep_once().await.unwrap();
    }
}

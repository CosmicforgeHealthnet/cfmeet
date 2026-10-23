//! Shared harness of the Part VI §51 platform-credential tests: a scripted Platform
//! Access, Platform Access customer chains, and requests exactly as the API Gateway
//! forwards them (a REAL Ed25519 `cfmeet.api-gateway` service token verified by the
//! Meeting Service's own trust bundle, no `Authorization`).
#![allow(dead_code)]

use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use backend::services::{
    actor::PlatformActor,
    platform::{
        authority::{Authentication, AuthorityChain, AuthorityUnavailable, CredentialAuthority},
        PlatformReceiver,
    },
};
use cf_service_auth::{
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, KeyId,
    OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{TestApp, TestOptions};

pub const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

/// Scripted Platform Access: credential id -> answer.
#[derive(Default)]
pub struct FakePlatformAccess {
    pub answers: Mutex<HashMap<Uuid, Authentication>>,
    pub unavailable: std::sync::atomic::AtomicBool,
    pub calls: AtomicUsize,
    pub correlations: Mutex<Vec<String>>,
}

#[async_trait]
impl CredentialAuthority for FakePlatformAccess {
    async fn delegated_authority(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.correlations
            .lock()
            .unwrap()
            .push(correlation_id.as_str().to_string());
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(AuthorityUnavailable("connect".into()));
        }
        let id = delegated.initiating_credential_id().unwrap();
        Ok(self
            .answers
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or(Authentication::Rejected {
                reason: "unknown_credential".into(),
            }))
    }
}

/// A Platform Access customer chain.
#[derive(Debug, Clone)]
pub struct Machine {
    pub actor: PlatformActor,
}

impl Machine {
    pub fn new(scopes: &[&str]) -> Self {
        Self {
            actor: PlatformActor {
                tenant_id: Uuid::new_v4(),
                application_id: Uuid::new_v4(),
                environment_id: Uuid::new_v4(),
                environment_kind: "SANDBOX".into(),
                service_account_id: Uuid::new_v4(),
                credential_id: Uuid::new_v4(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    /// Another credential / service account / environment of the SAME application.
    pub fn sibling_environment(&self) -> Self {
        let mut m = self.clone();
        m.actor.environment_id = Uuid::new_v4();
        m.actor.service_account_id = Uuid::new_v4();
        m.actor.credential_id = Uuid::new_v4();
        m
    }

    /// Another credential of the same service account and environment.
    pub fn same_environment(&self, scopes: &[&str]) -> Self {
        let mut m = self.clone();
        m.actor.credential_id = Uuid::new_v4();
        m.actor.scopes = scopes
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>();
        m
    }

    pub fn delegated(&self) -> DelegatedPlatformContext {
        let a = &self.actor;
        DelegatedPlatformContext::new(
            a.tenant_id,
            a.application_id,
            a.environment_id,
            ApplicationEnvironmentKind::Sandbox,
            a.service_account_id,
        )
        .with_initiating_credential(a.credential_id)
    }

    pub fn chain(&self, full: bool) -> Authentication {
        Authentication::Authenticated(Box::new(AuthorityChain {
            actor: self.actor.clone(),
            full_authority: full,
        }))
    }
}

pub fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

pub fn signer(service: ServiceId, key: ServiceSigningKey) -> ServiceTokenSigner {
    ServiceTokenSigner::new(
        service,
        ENV,
        key,
        TokenTtl::seconds(60).unwrap(),
        Arc::new(SystemClock),
    )
}

pub struct Harness {
    pub app: TestApp,
    pub pa: Arc<FakePlatformAccess>,
    /// The API Gateway: trusted with exactly `meeting-request.forward`.
    pub gateway: ServiceTokenSigner,
    /// The Developer Platform: trusted, but only with `user-session.verify`.
    pub developer_platform: ServiceTokenSigner,
    /// Claims the API Gateway identity with a key the bundle does not hold.
    pub rogue_gateway: ServiceTokenSigner,
}

/// The receiver's trust bundle and a scripted Platform Access.
pub fn receiver() -> (
    PlatformReceiver,
    Arc<FakePlatformAccess>,
    ServiceTokenSigner,
    ServiceTokenSigner,
    ServiceTokenSigner,
) {
    let gw = key("api-gateway-test-1");
    let dp = key("developer-platform-test-1");
    let bundle = json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::MEETING_SERVICE,
        "runtime_environment": ENV,
        "trusted_issuers": [{
            "issuer": cf_service_auth::services::API_GATEWAY,
            "audience": cf_service_auth::services::MEETING_SERVICE,
            "runtime_environment": ENV,
            "keys": [gw.public_jwk()],
            "permissions": ["meeting-request.forward"],
        }, {
            "issuer": cf_service_auth::services::DEVELOPER_PLATFORM,
            "audience": cf_service_auth::services::MEETING_SERVICE,
            "runtime_environment": ENV,
            "keys": [dp.public_jwk()],
            "permissions": ["user-session.verify"],
        }],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::meeting_service(), ENV).unwrap();
    let pa = Arc::new(FakePlatformAccess::default());
    let receiver = PlatformReceiver {
        verifier: Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock))),
        authority: pa.clone(),
    };
    (
        receiver,
        pa,
        signer(ServiceId::api_gateway(), gw),
        signer(ServiceId::developer_platform(), dp),
        signer(ServiceId::api_gateway(), key("api-gateway-test-2")),
    )
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with(TestOptions::default()).await
    }

    pub async fn with(opts: TestOptions) -> Option<Self> {
        let (receiver, pa, gateway, developer_platform, rogue_gateway) = receiver();
        let app = TestApp::with(TestOptions {
            platform: Some(receiver),
            ..opts
        })
        .await?;
        Some(Self {
            app,
            pa,
            gateway,
            developer_platform,
            rogue_gateway,
        })
    }

    pub fn register(&self, m: &Machine) {
        self.pa
            .answers
            .lock()
            .unwrap()
            .insert(m.actor.credential_id, m.chain(true));
    }

    /// A request as the gateway forwards it. Returns status, body and the signed
    /// correlation id.
    pub async fn forwarded_by(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        delegated: Option<DelegatedPlatformContext>,
        extra: &[(&str, &str)],
    ) -> (StatusCode, Value, String) {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::meeting_service());
        if let Some(d) = delegated {
            outbound = outbound.with_delegated_context(d);
        }
        let correlation = outbound.correlation_id().as_str().to_string();
        let signed = signer.sign(&outbound).unwrap();
        let mut headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        for (k, v) in extra {
            headers.push((k.to_string(), v.to_string()));
        }
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let (s, v) = self
            .app
            .call_with_headers(
                method,
                path,
                &refs,
                body.map(|b| b.to_string()).unwrap_or_default(),
            )
            .await;
        (s, v, correlation)
    }

    pub async fn machine(
        &self,
        m: &Machine,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (s, v, _) = self
            .forwarded_by(&self.gateway, method, path, body, Some(m.delegated()), &[])
            .await;
        (s, v)
    }
}

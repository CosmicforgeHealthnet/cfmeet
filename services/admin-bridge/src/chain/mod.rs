//! The one authorization path. Every HQ request goes through [`Bridge::handle`], in the
//! order of the signing profile:
//!
//! 1 shape → 2 client certificate → 3 HMAC v2 → 4 staff assertion → 5 request digest →
//! 6 revocation → 7 replay → 8 capability lookup → 9 authorization → 10 rate limit →
//! 11 audit (before any answer) → route.
//!
//! Once the client certificate is accepted, every answer (allowed or refused) is
//! audited first; if the audit row cannot be written the answer is
//! `503 AUDIT_UNAVAILABLE` and nothing is done.

pub mod assertion;
pub mod request;

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, RwLock},
};

use chrono::{DateTime, Duration, DurationRound, Utc};
use ed25519_dalek::VerifyingKey;
use http::HeaderMap;
use serde_json::{json, Value};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::config::{SecurityConfig, Switches};
use crate::contract::model::*;
use crate::errors::ErrorCode;
use crate::metrics::Metrics;
use crate::policy::AccessPolicy;
use crate::store::{
    AuditDraft, Claim, ClaimKind, ClaimOutcome, PrincipalKind, RateKey, Store, StoreError,
};
use crate::wire;

use assertion::{approval_time, Staff};
use request::{Matched, Query, RouteTable};

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// An HMAC key file's bytes, keyed by the enrolled digest they matched.
type CachedKey = (String, Arc<Zeroizing<Vec<u8>>>);

pub const REPLAY_WINDOW_MINUTES: i64 = 11;
pub const HMAC_SKEW_SECONDS: i64 = 30;
pub const MFA_MAX_AGE_SECONDS: i64 = 300;

/// The TLS layer's view of the client certificate (inserted per connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCert {
    /// Lowercase hex SHA-256 of the leaf certificate DER.
    pub fingerprint: String,
    pub uri_sans: Vec<String>,
}

/// One HQ request as received.
#[derive(Debug, Clone)]
pub struct Inbound {
    pub method: String,
    pub path: String,
    /// Raw query string exactly as sent (without `?`).
    pub query: Option<String>,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub peer: Option<PeerCert>,
}

impl Inbound {
    /// The exact request URI covered by the HMAC signature and the request digest.
    pub fn uri(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{}", self.path, q),
            None => self.path.clone(),
        }
    }
}

/// The answer, ready for the HTTP layer.
#[derive(Debug, Clone)]
pub struct Outbound {
    pub status: u16,
    pub correlation_id: Uuid,
    pub body: Vec<u8>,
}

/// Everything step 9 established, passed to the route.
pub struct Authorized {
    pub capability: Arc<Capability>,
    pub params: std::collections::BTreeMap<&'static str, String>,
    pub query: Query,
    pub staff: Staff,
    pub roles_used: Vec<Role>,
    pub correlation_id: Uuid,
    pub now: DateTime<Utc>,
    /// Commands: HQ's `Idempotency-Key`.
    pub idempotency_key: Option<Uuid>,
    /// Commands: this staff member's earlier operation under the same key (a replay; its
    /// approval was not consumed again).
    pub existing: Option<crate::store::Operation>,
    /// The request body (covered by the HMAC and the request digest).
    pub body: Vec<u8>,
}

/// A route's result.
pub struct Answer {
    pub result: Value,
    pub freshness: &'static str,
}

pub struct Bridge {
    pub switches: Switches,
    pub security: Option<SecurityConfig>,
    pub store: Arc<dyn Store>,
    pub routes: RouteTable,
    pub openapi_sha256: String,
    pub policy: RwLock<Option<AccessPolicy>>,
    pub clock: Clock,
    pub metrics: Metrics,
    pub health: crate::routes::HealthProbe,
    /// Owner reads (none configured: every owner read is OWNER_UNAVAILABLE).
    pub owners: Arc<dyn crate::owners::OwnerPort>,
    /// False when no database is configured (disabled bridge): readiness says so.
    pub database_configured: bool,
    /// Tests only ([`Bridge::with_test_commands`]): capability id → owner and owner path.
    #[doc(hidden)]
    pub test_commands: Vec<(&'static str, crate::contract::model::Owner, &'static str)>,
    hmac_cache: Mutex<HashMap<String, CachedKey>>,
}

/// Refusal before or after the audit point.
struct Refusal {
    code: ErrorCode,
    /// Who and what, once known (for the audit row).
    actor: Option<(String, String)>,
    capability: Option<String>,
    purpose: Option<Purpose>,
    scope: Value,
    request_digest: Option<String>,
}

impl From<ErrorCode> for Refusal {
    fn from(code: ErrorCode) -> Self {
        Refusal {
            code,
            actor: None,
            capability: None,
            purpose: None,
            scope: Value::Null,
            request_digest: None,
        }
    }
}

impl From<ErrorCode> for Box<Refusal> {
    fn from(code: ErrorCode) -> Self {
        Box::new(Refusal::from(code))
    }
}

fn store_down(_: StoreError) -> ErrorCode {
    ErrorCode::BridgeUnavailable
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut all = h.get_all(name).iter();
    let first = all.next()?;
    if all.next().is_some() {
        return None; // repeated security headers are refused
    }
    first.to_str().ok()
}

fn lower_hex(v: &str, len: usize) -> bool {
    v.len() == len
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn key_id_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.as_bytes()[0].is_ascii_alphanumeric()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Bridge {
    pub fn new(
        switches: Switches,
        security: Option<SecurityConfig>,
        store: Arc<dyn Store>,
        clock: Clock,
        health: crate::routes::HealthProbe,
    ) -> Self {
        let files = crate::contract::render::render();
        Bridge {
            switches,
            security,
            store,
            routes: RouteTable::new(crate::contract::catalog::catalog()),
            openapi_sha256: wire::sha256_hex(&files["openapi.json"]),
            policy: RwLock::new(None),
            clock,
            metrics: Metrics::default(),
            health,
            owners: Arc::new(crate::owners::NoOwners),
            database_configured: true,
            test_commands: Vec::new(),
            hmac_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Replaces the route table (tests exercise refusal paths of capabilities that no
    /// phase serves yet). Never used by the binary.
    pub fn with_owners(mut self, owners: Arc<dyn crate::owners::OwnerPort>) -> Self {
        self.owners = owners;
        self
    }

    /// Routes served test commands through the generic command core (the security and
    /// ledger tests of A3.0, before any family serves a command). Never used by the binary.
    #[doc(hidden)]
    pub fn with_test_commands(
        mut self,
        commands: Vec<(&'static str, crate::contract::model::Owner, &'static str)>,
    ) -> Self {
        self.test_commands = commands;
        self
    }

    #[doc(hidden)]
    pub fn with_catalog(mut self, caps: Vec<Capability>) -> Self {
        self.routes = RouteTable::new(caps);
        self
    }

    /// Loads and verifies the pinned access-policy revision. Called at start and by the
    /// maintenance loop; on failure the previous verified policy is dropped (fail closed).
    pub async fn load_policy(&self) -> Result<i64, crate::policy::PolicyError> {
        let Some(sec) = &self.security else {
            return Err(crate::policy::PolicyError::Missing);
        };
        let result = match self.store.policy_revision(sec.policy_revision).await {
            Ok(Some(row)) => crate::policy::verify(
                &row,
                &sec.policy_key,
                &sec.policy_key_id,
                sec.policy_revision,
            ),
            Ok(None) | Err(_) => Err(crate::policy::PolicyError::Missing),
        };
        let mut slot = self.policy.write().expect("policy lock");
        match result {
            Ok(p) => {
                let rev = p.revision;
                *slot = Some(p);
                Ok(rev)
            }
            Err(e) => {
                *slot = None;
                Err(e)
            }
        }
    }

    fn hmac_secret(&self, dir: &Path, kid: &str, digest: &str) -> Option<Arc<Zeroizing<Vec<u8>>>> {
        let mut cache = self.hmac_cache.lock().expect("hmac cache");
        if let Some((d, k)) = cache.get(kid) {
            if d == digest {
                return Some(k.clone());
            }
        }
        let raw = Zeroizing::new(std::fs::read(dir.join(format!("{kid}.key"))).ok()?);
        let bytes = Zeroizing::new(wire::decode_hmac_key_file(&raw).unwrap_or_default());
        if bytes.len() < 32 || wire::sha256_hex(&bytes) != digest {
            tracing::error!(
                kid_len = kid.len(),
                "HMAC key file missing, not base64url text of 32+ bytes, or not the enrolled key"
            );
            return None;
        }
        let k = Arc::new(bytes);
        cache.insert(kid.to_string(), (digest.to_string(), k.clone()));
        Some(k)
    }

    pub async fn handle(&self, req: Inbound) -> Outbound {
        let correlation_id = Uuid::new_v4();
        let now = (self.clock)();
        let outcome = self.pipeline(&req, correlation_id, now).await;
        let (status, body) = match outcome {
            Ok(answer) => {
                self.metrics.count("ALLOWED");
                (
                    200,
                    envelope(
                        correlation_id,
                        now,
                        answer.freshness,
                        Some(answer.result),
                        None,
                    ),
                )
            }
            Err(code) => {
                self.metrics.count(code.code());
                (
                    code.status(),
                    envelope(correlation_id, now, "UNAVAILABLE", None, Some(code)),
                )
            }
        };
        Outbound {
            status,
            correlation_id,
            body,
        }
    }

    async fn pipeline(
        &self,
        req: &Inbound,
        correlation_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Answer, ErrorCode> {
        // 1. Shape (and the master switch).
        if !self.switches.enabled {
            return Err(ErrorCode::BridgeDisabled);
        }
        request::check_shape(&req.method, &req.path)?;
        if req.body.len() > request::MAX_BODY_BYTES {
            return Err(ErrorCode::BodyTooLarge);
        }
        let sec = self.security.as_ref().ok_or(ErrorCode::BridgeDisabled)?;

        // 2. Client certificate.
        let peer = req
            .peer
            .as_ref()
            .ok_or(ErrorCode::ClientCertificateRefused)?;
        if peer.uri_sans.len() != 1 || peer.uri_sans[0] != sec.hq_workload_uri {
            return Err(ErrorCode::ClientCertificateRefused);
        }
        let cert = self
            .store
            .principal(PrincipalKind::ClientCert, &peer.fingerprint)
            .await
            .map_err(store_down)?;
        if !cert.is_some_and(|c| c.principal == HQ_PRINCIPAL && c.live_at(now)) {
            return Err(ErrorCode::ClientCertificateRefused);
        }

        // From here every answer is audited before it is sent.
        let decided = self.authorize(req, sec, correlation_id, now).await;
        let (draft, result) = match decided {
            Ok(auth) => {
                let draft = AuditDraft {
                    correlation_id,
                    occurred_at: now,
                    actor: format!("hq:{}", auth.staff.claims.sub),
                    session_id: Some(auth.staff.claims.session_id.clone()),
                    capability: Some(auth.capability.id.to_string()),
                    method: req.method.clone(),
                    path: req.path.clone(),
                    purpose: Some(auth.query.purpose.as_str().to_string()),
                    scope: scope_json(&auth.query),
                    decision: "ALLOWED".into(),
                    http_status: 200,
                    request_digest: Some(auth.staff.claims.request_digest.clone()),
                    approval_id: auth
                        .staff
                        .claims
                        .approval
                        .as_ref()
                        .map(|a| a.approval_id.clone()),
                    approver: auth
                        .staff
                        .claims
                        .approval
                        .as_ref()
                        .map(|a| format!("hq:{}", a.approver_ref)),
                    detail: json!({
                        "roles_used": auth.roles_used.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                        "risk": auth.capability.risk.as_str(),
                        "policy_revision": self.policy.read().expect("policy lock").as_ref().map(|p| p.revision),
                    }),
                };
                (draft, Ok(auth))
            }
            Err(r) => {
                let (actor, session) = r
                    .actor
                    .clone()
                    .unwrap_or_else(|| ("unauthenticated".into(), String::new()));
                let draft = AuditDraft {
                    correlation_id,
                    occurred_at: now,
                    actor,
                    session_id: (!session.is_empty()).then_some(session),
                    capability: r.capability.clone(),
                    method: req.method.clone(),
                    path: req.path.clone(),
                    purpose: r.purpose.map(|p| p.as_str().to_string()),
                    scope: r.scope.clone(),
                    decision: r.code.code().to_string(),
                    http_status: r.code.status(),
                    request_digest: r.request_digest.clone(),
                    approval_id: None,
                    approver: None,
                    detail: Value::Null,
                };
                (draft, Err(r.code))
            }
        };
        // 11. Audit before answering.
        if self.store.append_audit(&draft).await.is_err() {
            tracing::error!(%correlation_id, "audit write failed: answering 503 with no data");
            return Err(ErrorCode::AuditUnavailable);
        }
        let auth = result?;
        crate::routes::dispatch(self, &auth).await
    }

    async fn authorize(
        &self,
        req: &Inbound,
        sec: &SecurityConfig,
        correlation_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Authorized, Box<Refusal>> {
        let uri = req.uri();
        let h = &req.headers;

        // 3. HMAC v2.
        let name = header(h, "x-service-name").ok_or(ErrorCode::SignatureInvalid)?;
        let version =
            header(h, "x-service-signature-version").ok_or(ErrorCode::SignatureInvalid)?;
        let kid = header(h, "x-hq-key-id").ok_or(ErrorCode::SignatureInvalid)?;
        let ts = header(h, "x-service-timestamp").ok_or(ErrorCode::SignatureInvalid)?;
        let nonce = header(h, "x-service-nonce").ok_or(ErrorCode::SignatureInvalid)?;
        let signature = header(h, "x-service-signature").ok_or(ErrorCode::SignatureInvalid)?;
        if name != HQ_PRINCIPAL
            || version != wire::SIGNATURE_VERSION
            || !key_id_ok(kid)
            || !lower_hex(nonce, 32)
            || !lower_hex(signature, 64)
        {
            return Err(ErrorCode::SignatureInvalid.into());
        }
        let ts: i64 = ts
            .parse()
            .ok()
            .filter(|_| ts.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(ErrorCode::SignatureInvalid)?;
        if (now.timestamp() - ts).abs() > HMAC_SKEW_SECONDS {
            return Err(ErrorCode::TimestampSkew.into());
        }
        let key_row = self
            .store
            .principal(PrincipalKind::HmacKey, kid)
            .await
            .map_err(store_down)?;
        let key_row = key_row
            .filter(|k| k.principal == HQ_PRINCIPAL && k.live_at(now))
            .ok_or(ErrorCode::HqKeyUnknown)?;
        let secret = self
            .hmac_secret(&sec.hmac_key_dir, kid, &key_row.material)
            .ok_or(ErrorCode::HqKeyUnknown)?;
        if !wire::hmac_v2_verify(&secret, &req.method, &uri, ts, nonce, &req.body, signature) {
            return Err(ErrorCode::SignatureInvalid.into());
        }

        // 4. Staff assertion.
        let token = header(h, "authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ErrorCode::StaffAssertionInvalid)?;
        let staff_kid = assertion::kid(token)?;
        let staff_row = self
            .store
            .principal(PrincipalKind::StaffKey, &staff_kid)
            .await
            .map_err(store_down)?;
        let staff_row = staff_row
            .filter(|k| k.principal == HQ_PRINCIPAL && k.live_at(now))
            .ok_or(ErrorCode::StaffAssertionInvalid)?;
        let key = hex::decode(&staff_row.material)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
            .ok_or(ErrorCode::StaffAssertionInvalid)?;
        let staff = assertion::verify(
            token,
            &assertion::Expect {
                key: &key,
                issuer: staff_row.issuer.as_deref().unwrap_or_default(),
                audience: staff_row.audience.as_deref().unwrap_or_default(),
                strong_acr: &sec.strong_acr,
                workload_acr: &sec.workload_acr,
                now,
            },
        )?;
        let c = &staff.claims;
        let who = |code: ErrorCode| -> Box<Refusal> {
            Box::new(Refusal {
                code,
                actor: Some((format!("hq:{}", c.sub), c.session_id.clone())),
                capability: None,
                purpose: None,
                scope: Value::Null,
                request_digest: Some(c.request_digest.clone()),
            })
        };

        // 5. Bound to this exact request.
        if c.request_digest != wire::request_digest(&req.method, &uri, &req.body) {
            return Err(who(ErrorCode::RequestDigestMismatch));
        }
        // 6. Revocation.
        if self
            .store
            .is_revoked(&c.sub, &c.session_id, now)
            .await
            .map_err(|e| who(store_down(e)))?
        {
            return Err(who(ErrorCode::Revoked));
        }
        // 7. Replay: nonce and jti together, one transaction.
        let keep = now + Duration::minutes(REPLAY_WINDOW_MINUTES);
        let claims = [
            Claim {
                kind: ClaimKind::Nonce,
                value: nonce.to_string(),
                keep_until: keep,
            },
            Claim {
                kind: ClaimKind::Jti,
                value: c.jti.clone(),
                keep_until: keep,
            },
        ];
        match self
            .store
            .claim_once(&claims)
            .await
            .map_err(|e| who(store_down(e)))?
        {
            ClaimOutcome::Recorded => {}
            ClaimOutcome::Replay(_) => return Err(who(ErrorCode::Replay)),
        }
        // 8. Capability lookup.
        let Matched { capability, params } = self
            .routes
            .lookup(&req.method, &req.path)
            .ok_or_else(|| who(ErrorCode::CapabilityNotFound))?;
        let at = |code: ErrorCode, query: Option<&Query>| -> Box<Refusal> {
            Box::new(Refusal {
                capability: Some(capability.id.to_string()),
                purpose: query.map(|q| q.purpose),
                scope: query.map(scope_json).unwrap_or(Value::Null),
                ..*who(code)
            })
        };
        let query = request::parse_query(
            req.query.as_deref(),
            capability.shape,
            &capability.purposes,
            now,
        )
        .map_err(|e| at(e, None))?;
        let q = Some(&query);

        // 9. Authorization.
        let roles_used: Vec<Role> = {
            let guard = self.policy.read().expect("policy lock");
            let policy = guard
                .as_ref()
                .ok_or_else(|| at(ErrorCode::PolicyUnavailable, q))?;
            if !c.permissions.iter().any(|p| *p == capability.permission) {
                return Err(at(ErrorCode::PermissionDenied, q));
            }
            let mut used: Vec<Role> = staff
                .roles
                .iter()
                .copied()
                .filter(|r| {
                    capability.roles.contains(r)
                        && r.allows_purpose(query.purpose)
                        && policy.allows(*r, &capability.permission, query.purpose)
                })
                .collect();
            if used.is_empty() {
                let any_purpose = staff.roles.iter().any(|r| {
                    capability.roles.contains(r)
                        && Purpose::ALL
                            .iter()
                            .any(|p| policy.allows(*r, &capability.permission, *p))
                });
                return Err(at(
                    if any_purpose {
                        ErrorCode::PurposeNotAllowed
                    } else {
                        ErrorCode::PermissionDenied
                    },
                    q,
                ));
            }
            used.sort();
            used.dedup();
            used
        };
        let in_scope = c.market_scopes.contains(&query.market)
            && sec.enrolled_markets.contains(&query.market)
            && c.organization_scopes.contains(&query.organization)
            && c.zone_scopes.contains(&query.zone)
            && c.tenant_scopes.contains(&query.tenant)
            && sec.enrolled_tenants.contains(&query.tenant);
        if !in_scope {
            return Err(at(ErrorCode::ScopeDenied, q));
        }
        if capability.risk >= Risk::R2 && now.timestamp() - c.auth_time > MFA_MAX_AGE_SECONDS {
            return Err(at(ErrorCode::RecentMfaRequired, q));
        }
        if capability.risk >= Risk::R3 && !staff.strong {
            return Err(at(ErrorCode::StrongAuthRequired, q));
        }
        match (capability.approval(), &c.approval) {
            (Approval::None, None) => {}
            (Approval::None, Some(_)) => return Err(at(ErrorCode::ApprovalInvalid, q)),
            (Approval::TwoPerson, None) => return Err(at(ErrorCode::ApprovalRequired, q)),
            (Approval::TwoPerson, Some(a)) => {
                let (decided, expires) =
                    match (approval_time(&a.decided_at), approval_time(&a.expires_at)) {
                        (Some(d), Some(e)) => (d, e),
                        _ => return Err(at(ErrorCode::ApprovalInvalid, q)),
                    };
                if a.capability != capability.id
                    || a.requester_ref != c.sub
                    || a.approver_ref == c.sub
                    || expires <= decided
                    || (expires - decided).num_seconds() > assertion::MAX_APPROVAL_SECONDS
                    || decided > now + Duration::seconds(assertion::SKEW_SECONDS)
                {
                    return Err(at(ErrorCode::ApprovalInvalid, q));
                }
                if expires <= now {
                    return Err(at(ErrorCode::ApprovalExpired, q));
                }
            }
        }
        if !capability.served
            || !self.switches.module_on(capability.module())
            || (capability.kind == Kind::Command && !self.switches.writes_enabled)
        {
            return Err(at(ErrorCode::CapabilityDisabled, q));
        }
        let idempotency_key = if capability.idempotency_required() {
            Some(
                header(h, "idempotency-key")
                    .filter(|k| assertion::uuid_ok(k))
                    .and_then(|k| Uuid::parse_str(k).ok())
                    .ok_or_else(|| at(ErrorCode::IdempotencyKeyRequired, q))?,
            )
        } else {
            None
        };
        // A replay of this staff member's earlier command returns its stored outcome
        // (routes::commands); its approval was consumed by the first request.
        let existing = match idempotency_key {
            Some(key) => self
                .store
                .operation(&c.sub, key)
                .await
                .map_err(|e| at(store_down(e), q))?,
            None => None,
        };
        // Single use: the approval is consumed only once the command can run at all.
        if let (Approval::TwoPerson, Some(a), None) =
            (capability.approval(), &c.approval, &existing)
        {
            let expires =
                approval_time(&a.expires_at).ok_or_else(|| at(ErrorCode::ApprovalInvalid, q))?;
            let claim = [Claim {
                kind: ClaimKind::Approval,
                value: a.approval_id.clone(),
                keep_until: expires,
            }];
            match self
                .store
                .claim_once(&claim)
                .await
                .map_err(|e| at(store_down(e), q))?
            {
                ClaimOutcome::Recorded => {}
                ClaimOutcome::Replay(_) => return Err(at(ErrorCode::ApprovalReused, q)),
            }
        }

        // 10. Rate limit, per staff member, permission and scope.
        let bucket = wire::sha256_hex(
            [
                c.sub.as_str(),
                &capability.permission,
                &query.market,
                &query.organization,
                &query.zone,
                &query.tenant,
            ]
            .join("\n")
            .as_bytes(),
        );
        let window_start = now.duration_trunc(Duration::minutes(1)).unwrap_or(now);
        if !self
            .store
            .rate_allow(
                &RateKey {
                    bucket,
                    window_start,
                },
                sec.rate_limit_per_minute,
            )
            .await
            .map_err(|e| at(store_down(e), q))?
        {
            return Err(at(ErrorCode::RateLimited, q));
        }
        Ok(Authorized {
            capability,
            params,
            query,
            staff,
            roles_used,
            correlation_id,
            now,
            idempotency_key,
            existing,
            body: req.body.clone(),
        })
    }
}

fn scope_json(q: &Query) -> Value {
    json!({"market": q.market, "organization": q.organization, "zone": q.zone, "tenant": q.tenant})
}

pub fn envelope(
    correlation_id: Uuid,
    now: DateTime<Utc>,
    freshness: &str,
    result: Option<Value>,
    error: Option<ErrorCode>,
) -> Vec<u8> {
    let mut v = json!({
        "contract_version": ENVELOPE_VERSION,
        "product": PRODUCT,
        "correlation_id": correlation_id.to_string(),
        "as_of": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "freshness": freshness,
    });
    match (result, error) {
        (Some(r), None) => {
            v["result"] = r;
            v["error"] = Value::Null;
        }
        (_, Some(e)) => v["error"] = json!({"code": e.code(), "message": e.meaning()}),
        (None, None) => unreachable!("an answer has a result or an error"),
    }
    serde_json::to_vec(&v).expect("serializable")
}

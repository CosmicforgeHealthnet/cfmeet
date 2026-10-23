//! Provisioning exactly as operators and customers do it: signed operator-console calls
//! (Platform Access administration, Developer Platform operator routes), the Meeting
//! Service account API, and the public API Gateway.

use std::time::Duration;

use anyhow::{bail, Context};
use base64::Engine;
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

/// A response: status, selected headers and the JSON body (or `Null`).
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Value,
    pub text: String,
}

impl Resp {
    pub async fn from(r: reqwest::Response) -> Resp {
        let status = r.status().as_u16();
        let headers = r.headers().clone();
        let text = r.text().await.unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Resp {
            status,
            headers,
            body,
            text,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The error code of the CF error envelope (`{"error": {"code": ...}}`).
    pub fn code(&self) -> Option<&str> {
        self.body
            .pointer("/error/code")
            .and_then(Value::as_str)
            .or_else(|| self.body.get("code").and_then(Value::as_str))
    }

    pub fn str(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{pointer} missing in {} {}", self.status, self.text))
            .to_string()
    }

    pub fn uuid(&self, pointer: &str) -> Uuid {
        Uuid::parse_str(&self.str(pointer)).expect("uuid")
    }

    /// `Ok(self)` when the status is `expected`, otherwise an error naming `what`.
    pub fn expect(self, expected: u16, what: &str) -> anyhow::Result<Resp> {
        if self.status != expected {
            bail!(
                "{what}: expected {expected}, got {} {}",
                self.status,
                self.text
            );
        }
        Ok(self)
    }
}

// -------------------------------------------------------------------------------------
// Operator console
// -------------------------------------------------------------------------------------

/// The operator console: every call is a service token of `cfmeet.operator-console`
/// (the harness's operator key) plus the named human operator in `x-cf-operator`.
pub struct Operator {
    signer: ServiceTokenSigner,
    pa: String,
    dp: String,
    http: reqwest::Client,
}

/// A Platform Access customer chain (tenant -> application -> environment -> service
/// account -> credential).
#[derive(Debug, Clone)]
pub struct Customer {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub secret: String,
    pub scopes: Vec<String>,
}

pub const OPERATOR_NAME: &str = "cfmeet-certification";

impl Operator {
    pub fn new(signer: ServiceTokenSigner, pa: &str, dp: &str, http: reqwest::Client) -> Self {
        Self {
            signer,
            pa: pa.to_string(),
            dp: dp.to_string(),
            http,
        }
    }

    async fn call(
        &self,
        audience: ServiceId,
        base: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Resp> {
        let signed = self
            .signer
            .sign(&OutboundServiceRequest::new_operation(audience))?;
        let mut r = self
            .http
            .request(method, format!("{base}{path}"))
            .header("x-cf-operator", OPERATOR_NAME);
        for (k, v) in signed.headers().iter() {
            r = r.header(k.as_str(), v.as_bytes());
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        Ok(Resp::from(
            r.send()
                .await
                .with_context(|| format!("operator call {path}"))?,
        )
        .await)
    }

    /// Platform Access operator API (`/internal/v1/admin/**`).
    pub async fn pa(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Resp> {
        self.call(
            ServiceId::platform_access(),
            &self.pa,
            method,
            &format!("/internal/v1{path}"),
            body,
        )
        .await
    }

    /// Developer Platform operator API (`/internal/v1/operator/**`).
    pub async fn dp(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Resp> {
        self.call(
            ServiceId::developer_platform(),
            &self.dp,
            method,
            &format!("/internal/v1{path}"),
            body,
        )
        .await
    }

    async fn pa_created(&self, path: &str, body: Value) -> anyhow::Result<Resp> {
        let r = self.pa(Method::POST, path, Some(body)).await?;
        if !(200..300).contains(&r.status) {
            bail!("POST {path}: {} {}", r.status, r.text);
        }
        Ok(r)
    }

    pub async fn tenant(&self, classification: &str) -> anyhow::Result<Uuid> {
        let slug = format!("t{}", &Uuid::new_v4().simple().to_string()[..12]);
        Ok(self
            .pa_created(
                "/admin/tenants",
                json!({ "slug": slug, "displayName": "certification tenant", "classification": classification }),
            )
            .await?
            .uuid("/id"))
    }

    pub async fn application(&self, tenant: Uuid) -> anyhow::Result<Uuid> {
        let slug = format!("a{}", &Uuid::new_v4().simple().to_string()[..12]);
        Ok(self
            .pa_created(
                &format!("/admin/tenants/{tenant}/applications"),
                json!({ "slug": slug, "displayName": "certification application" }),
            )
            .await?
            .uuid("/id"))
    }

    /// Allows `scope` on the application (ordinary grant path).
    pub async fn grant_scope(&self, application: Uuid, scope: &str) -> anyhow::Result<Resp> {
        self.pa(
            Method::POST,
            &format!("/admin/applications/{application}/scopes"),
            Some(json!({ "scope": scope })),
        )
        .await
    }

    /// The explicit, audited privileged operator grant (FIRST_PARTY tenants only).
    pub async fn grant_privileged_scope(
        &self,
        application: Uuid,
        scope: &str,
    ) -> anyhow::Result<Resp> {
        self.pa(
            Method::POST,
            &format!("/admin/applications/{application}/scopes"),
            Some(json!({ "scope": scope, "privileged": true })),
        )
        .await
    }

    pub async fn grant_scopes(&self, application: Uuid, scopes: &[&str]) -> anyhow::Result<()> {
        for s in scopes {
            let r = self.grant_scope(application, s).await?;
            if !(200..300).contains(&r.status) && r.status != 409 {
                bail!("granting {s}: {} {}", r.status, r.text);
            }
        }
        Ok(())
    }

    pub async fn environment(&self, application: Uuid, kind: &str) -> anyhow::Result<Resp> {
        self.pa(
            Method::POST,
            &format!("/admin/applications/{application}/environments"),
            Some(json!({ "kind": kind })),
        )
        .await
    }

    pub async fn service_account(&self, environment: Uuid) -> anyhow::Result<Uuid> {
        Ok(self
            .pa_created(
                &format!("/admin/environments/{environment}/service-accounts"),
                json!({ "name": "backend", "description": "certification" }),
            )
            .await?
            .uuid("/id"))
    }

    /// Issues a credential (`(credential id, secret)`).
    pub async fn credential(
        &self,
        service_account: Uuid,
        scopes: &[&str],
    ) -> anyhow::Result<(Uuid, String)> {
        let r = self
            .pa_created(
                &format!("/admin/service-accounts/{service_account}/credentials"),
                json!({ "scopes": scopes, "label": "certification" }),
            )
            .await?;
        Ok((r.uuid("/credential/id"), r.str("/secret")))
    }

    pub async fn revoke(&self, credential: Uuid) -> anyhow::Result<()> {
        self.pa_created(
            &format!("/admin/credentials/{credential}/revoke"),
            json!({}),
        )
        .await?;
        Ok(())
    }

    /// A communication principal of the environment (`EXTERNAL_APPLICATION_PRINCIPAL`).
    pub async fn principal(&self, environment: Uuid) -> anyhow::Result<Uuid> {
        Ok(self
            .pa_created(
                &format!("/admin/environments/{environment}/principals"),
                json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL",
                        "externalReference": format!("p-{}", Uuid::new_v4().simple()) }),
            )
            .await?
            .uuid("/principal/id"))
    }

    /// A service account + credential on an EXISTING environment (e.g. one created by
    /// the Developer Platform), after allowing `scopes` on its application.
    pub async fn credential_for(
        &self,
        tenant: Uuid,
        application: Uuid,
        environment: Uuid,
        scopes: &[&str],
    ) -> anyhow::Result<Customer> {
        self.grant_scopes(application, scopes).await?;
        let sa = self.service_account(environment).await?;
        let (credential_id, secret) = self.credential(sa, scopes).await?;
        Ok(Customer {
            tenant_id: tenant,
            application_id: application,
            environment_id: environment,
            service_account_id: sa,
            credential_id,
            secret,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        })
    }

    /// A complete operator-provisioned customer chain.
    pub async fn customer(
        &self,
        classification: &str,
        environment_kind: &str,
        scopes: &[&str],
    ) -> anyhow::Result<Customer> {
        let tenant = self.tenant(classification).await?;
        let app = self.application(tenant).await?;
        self.grant_scopes(app, scopes).await?;
        let env = self.environment(app, environment_kind).await?;
        if !(200..300).contains(&env.status) {
            bail!(
                "environment {environment_kind}: {} {}",
                env.status,
                env.text
            );
        }
        let env = env.uuid("/id");
        let sa = self.service_account(env).await?;
        let (credential_id, secret) = self.credential(sa, scopes).await?;
        Ok(Customer {
            tenant_id: tenant,
            application_id: app,
            environment_id: env,
            service_account_id: sa,
            credential_id,
            secret,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        })
    }

    /// Publishes a commercial plan version with `entitlements`; returns its id.
    pub async fn publish_plan(&self, code: &str, entitlements: Value) -> anyhow::Result<Uuid> {
        let r = self
            .dp(
                Method::POST,
                "/operator/commercial-plans",
                Some(
                    json!({ "code": code, "displayName": format!("Certification {code}"),
                             "entitlementProfileRef": format!("cert-{code}"),
                             "entitlements": entitlements }),
                ),
            )
            .await?
            .expect(201, "publish commercial plan")?;
        Ok(r.uuid("/id"))
    }

    /// Assigns `plan` to an organization (`project`: None) or one of its projects.
    pub async fn assign_plan(
        &self,
        organization: Uuid,
        plan: Uuid,
        project: Option<Uuid>,
    ) -> anyhow::Result<Resp> {
        self.dp(
            Method::POST,
            &format!("/operator/organizations/{organization}/plan-assignments"),
            Some(json!({ "planId": plan, "projectId": project, "source": "CONTRACT" })),
        )
        .await
    }

    /// Registers the Developer Platform organization of an existing FIRST_PARTY tenant
    /// (returns the response: organization + one-time owner invitation token).
    pub async fn register_first_party(
        &self,
        tenant: Uuid,
        owner_email: &str,
    ) -> anyhow::Result<Resp> {
        self.dp(
            Method::POST,
            "/operator/organizations",
            Some(
                json!({ "tenantId": tenant, "displayName": "CosmicForge internal product",
                         "ownerEmail": owner_email }),
            ),
        )
        .await
    }
}

// -------------------------------------------------------------------------------------
// Public API Gateway
// -------------------------------------------------------------------------------------

/// How a public request authenticates.
#[derive(Debug, Clone, Copy)]
pub enum Auth<'a> {
    /// A Platform Access credential (`Bearer cfmk_…`).
    Credential(&'a str),
    /// A CF Meet human session (`Bearer <jwt>`): Developer Console / Meeting routes.
    Session(&'a str),
    /// A Messaging client access token (`CF-Client-Token …`).
    ClientToken(&'a str),
    /// A Calling signaling credential (`Bearer cfsig_…`).
    Signaling(&'a str),
    None,
}

#[derive(Clone)]
pub struct Gateway {
    pub url: String,
    pub http: reqwest::Client,
}

impl Gateway {
    pub fn new(url: &str, http: reqwest::Client) -> Self {
        Self {
            url: url.to_string(),
            http,
        }
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        auth: Auth<'_>,
        body: Option<Value>,
        headers: &[(&str, String)],
    ) -> Resp {
        let mut r = self
            .http
            .request(method.clone(), format!("{}{path}", self.url));
        r = match auth {
            Auth::Credential(t) | Auth::Session(t) | Auth::Signaling(t) => r.bearer_auth(t),
            Auth::ClientToken(t) => r.header("authorization", format!("CF-Client-Token {t}")),
            Auth::None => r,
        };
        if method == Method::POST && !headers.iter().any(|(k, _)| *k == "idempotency-key") {
            r = r.header("idempotency-key", Uuid::new_v4().to_string());
        }
        for (k, v) in headers {
            r = r.header(*k, v);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        match r.send().await {
            Ok(resp) => Resp::from(resp).await,
            Err(e) => panic!("{method} {path} through the gateway failed: {e}"),
        }
    }

    pub async fn get(&self, path: &str, auth: Auth<'_>) -> Resp {
        self.request(Method::GET, path, auth, None, &[]).await
    }

    pub async fn post(&self, path: &str, auth: Auth<'_>, body: Value) -> Resp {
        self.request(Method::POST, path, auth, Some(body), &[])
            .await
    }

    /// A request acting for a Messaging principal (`cf-acting-principal`).
    pub async fn acting(
        &self,
        method: Method,
        path: &str,
        auth: Auth<'_>,
        principal: Uuid,
        body: Option<Value>,
    ) -> Resp {
        self.request(
            method,
            path,
            auth,
            body,
            &[("cf-acting-principal", principal.to_string())],
        )
        .await
    }
}

// -------------------------------------------------------------------------------------
// CF Meet accounts (Meeting Service)
// -------------------------------------------------------------------------------------

/// A verified, signed-in CF Meet user.
#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub session: String,
}

fn jwt_subject(token: &str) -> anyhow::Result<Uuid> {
    let payload = token.split('.').nth(1).context("malformed session token")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .context("session token payload")?;
    let v: Value = serde_json::from_slice(&bytes)?;
    Ok(Uuid::parse_str(v["sub"].as_str().context("sub")?)?)
}

/// Registers a CF Meet account through the Meeting Service's public account API,
/// verifies its e-mail with the code the service stored for mailing (read from the
/// shared Redis — no SMTP in the stack) and signs in.
pub async fn register_user(
    meeting: &str,
    redis_url: &str,
    http: &reqwest::Client,
    email: &str,
) -> anyhow::Result<User> {
    let password = format!("Cert-{}-Pw9!", Uuid::new_v4().simple());
    let username = format!("u{}", &Uuid::new_v4().simple().to_string()[..12]);
    let r = Resp::from(
        http.post(format!("{meeting}/api/v1/auth/register"))
            .json(&json!({ "username": username, "email": email, "password": password, "confirm_password": password }))
            .send()
            .await?,
    )
    .await;
    if !(200..300).contains(&r.status) {
        bail!("register {email}: {} {}", r.status, r.text);
    }
    let key = format!("otp:verify:{}", email.to_lowercase());
    let mut code = None;
    for _ in 0..100 {
        code = crate::redis::get(redis_url, &key).await?;
        if code.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let code = code.with_context(|| format!("no verification code stored for {email}"))?;
    let r = Resp::from(
        http.post(format!("{meeting}/api/v1/auth/verify-email"))
            .json(&json!({ "email": email, "token": code }))
            .send()
            .await?,
    )
    .await;
    if !(200..300).contains(&r.status) {
        bail!("verify-email {email}: {} {}", r.status, r.text);
    }
    let r = Resp::from(
        http.post(format!("{meeting}/api/v1/auth/login"))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await?,
    )
    .await;
    if r.status != 200 {
        bail!("login {email}: {} {}", r.status, r.text);
    }
    let session = r
        .body
        .get("access_token")
        .or_else(|| r.body.pointer("/data/access_token"))
        .or_else(|| r.body.get("accessToken"))
        .and_then(Value::as_str)
        .with_context(|| format!("no access token in login response {}", r.text))?
        .to_string();
    Ok(User {
        id: jwt_subject(&session)?,
        email: email.to_string(),
        session,
    })
}

// -------------------------------------------------------------------------------------
// Scopes
// -------------------------------------------------------------------------------------

/// Standard (non-trusted, non-privileged) scopes of the Platform Access registry
/// (`services/platform-access/src/identity/scopes.rs`) that a SANDBOX environment's
/// developer credential exercises across the platform. Excluded on purpose: the Contact
/// Vault scopes (`privacy.contacts.*`, no vault in sandbox), number porting (no live
/// portability provider), the delegated identity-management and production-access
/// scopes (no customer route uses them yet), and every FIRST_PARTY-only / privileged
/// operator scope (`events.private_delivery`, `numbers.admin`, `messaging.admin`,
/// `developer_platform.admin`).
pub const SANDBOX_SCOPES: &[&str] = &[
    "principals.read",
    "principals.write",
    "calls.read",
    "calls.write",
    "privacy.contexts.read",
    "privacy.contexts.write",
    "privacy.sessions.read",
    "privacy.sessions.write",
    "privacy.evidence.read",
    "enterprise.config.read",
    "enterprise.config.write",
    "enterprise.calls.use",
    "enterprise.voicemail.read",
    "enterprise.voicemail.manage",
    "enterprise.recording.read",
    "enterprise.recording.manage",
    "webhooks.read",
    "webhooks.manage",
    "usage.read",
    "numbers.search",
    "numbers.read",
    "numbers.manage",
    "numbers.assign",
    "numbers.kyc.read",
    "numbers.kyc.manage",
    "sms.send",
    "sms.read",
    "messages.send",
    "messages.read",
    "conversations.create",
    "conversations.manage",
    "attachments.write",
    "attachments.read",
    "moderation.manage",
    "messaging.export",
    "meetings.read",
    "meetings.write",
    "organizations.read",
    "organizations.manage",
    "projects.read",
    "projects.manage",
    "audit.read",
];

/// Scopes only a FIRST_PARTY tenant can hold (trusted / privileged operator grants).
pub const FIRST_PARTY_ONLY_SCOPES: &[&str] = &["events.private_delivery"];
pub const PRIVILEGED_OPERATOR_SCOPES: &[&str] = &["messaging.admin", "numbers.admin"];

// -------------------------------------------------------------------------------------
// The external developer's sandbox (shared by `cfmeet-cert-stack up` and the journeys)
// -------------------------------------------------------------------------------------

/// A developer's self-service sandbox: CF Meet user -> Developer Platform organization
/// -> project -> SANDBOX environment (all through the public gateway with the user's
/// session; the Developer Platform delegates each to Platform Access) -> a credential.
#[derive(Debug, Clone)]
pub struct Sandbox {
    pub user: User,
    pub organization_id: Uuid,
    pub project_id: Uuid,
    pub environment_id: Uuid,
    pub customer: Customer,
}

pub async fn developer_sandbox(
    stack: &crate::Stack,
    email: &str,
    scopes: &[&str],
) -> anyhow::Result<Sandbox> {
    let user = register_user(
        stack.url(crate::stack::MEETING_SERVICE),
        &stack.config.redis_url,
        &stack.http,
        email,
    )
    .await?;
    let gw = stack.gateway();
    let session = Auth::Session(&user.session);
    let org = gw
        .post(
            "/v1/organizations",
            session,
            json!({ "displayName": "Certification Developer Org" }),
        )
        .await
        .expect(201, "POST /v1/organizations (developer session)")?;
    let organization_id = org.uuid("/id");
    let project = gw
        .post(
            &format!("/v1/organizations/{organization_id}/projects"),
            session,
            json!({ "displayName": "Certification Project" }),
        )
        .await
        .expect(201, "POST /v1/organizations/{id}/projects")?;
    let project_id = project.uuid("/id");
    let env = gw
        .post(
            &format!("/v1/projects/{project_id}/environments"),
            session,
            json!({ "kind": "SANDBOX" }),
        )
        .await
        .expect(201, "POST /v1/projects/{id}/environments (SANDBOX)")?;
    let environment_id = env.uuid("/id");
    // The Developer Platform has no credential self-service yet (no public credential
    // route exists in packages/contracts/platform-developer/http/v1.openapi.json): the
    // credential of the developer-created environment is issued by the operator console
    // through the Platform Access operator API.
    let customer = stack
        .operator()
        .credential_for(organization_id, project_id, environment_id, scopes)
        .await?;
    Ok(Sandbox {
        user,
        organization_id,
        project_id,
        environment_id,
        customer,
    })
}

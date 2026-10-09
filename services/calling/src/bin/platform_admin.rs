//! `platform_admin` — operational administration of the Calling platform trust
//! foundation (tenants, applications, environments, service accounts, scopes, API
//! credentials, principals and limit policy).
//!
//! Uses the same `PlatformService` domain operations as future administration APIs and
//! connects directly to the Calling database (`DATABASE_URL`, `API_CREDENTIAL_PEPPER`).
//! Output is JSON on stdout; operator audit lines go to stderr. `credential create` and
//! `credential rotate` print the raw credential exactly once; no command can recover it.
//!
//! All arguments are generic: there are no product-specific commands or defaults.

use std::{process::ExitCode, sync::Arc, time::Duration as StdDuration};

use calling::{
    config::CallingConfig,
    domain::{
        ApplicationStatus, Clock, CredentialId, EnvironmentKind, EnvironmentStatus, PrincipalId,
        PrincipalKind, PrincipalStatus, ServiceAccountStatus, SystemClock, TenantClassification,
        TenantStatus,
    },
    platform::{
        applications::{ApplicationView, NewApplication},
        credentials::{CredentialFilter, IssueCredential, IssuedCredential, RotateCredential},
        environments::EnvironmentView,
        error::summarize_db_error,
        limits::LimitValues,
        principals::NewPrincipal,
        scopes::ScopeRegistry,
        service_accounts::{NewServiceAccount, ServiceAccountView},
        tenants::{NewTenant, TenantView},
        PlatformError, PlatformService,
    },
    telemetry,
};
use chrono::Duration;
use clap::{Args, Parser, Subcommand, ValueEnum};
use sea_orm::{ConnectOptions, Database};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "platform_admin",
    version,
    about = "CF Meet Calling platform administration"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Tenants (platform customers).
    #[command(subcommand)]
    Tenant(TenantCmd),
    /// Applications / projects of a tenant.
    #[command(subcommand)]
    Application(ApplicationCmd),
    /// SANDBOX / TEST / PRODUCTION environments of an application.
    #[command(subcommand)]
    Environment(EnvironmentCmd),
    /// Service accounts (machine identities) of an environment.
    #[command(subcommand)]
    ServiceAccount(ServiceAccountCmd),
    /// Scope registry and application allowed scopes.
    #[command(subcommand)]
    Scope(ScopeCmd),
    /// API credentials.
    #[command(subcommand)]
    Credential(CredentialCmd),
    /// Communication principals.
    #[command(subcommand)]
    Principal(PrincipalCmd),
    /// Application / credential limit policy.
    #[command(subcommand)]
    Limits(LimitsCmd),
    /// Typed routing policy of an environment (Part E §37–§38).
    #[command(subcommand)]
    RoutingPolicy(RoutingPolicyCmd),
    /// Telecom spend / velocity policy of an environment (Part E §153–§158). No policy =
    /// no paid public-network leg.
    #[command(subcommand)]
    SpendPolicy(SpendPolicyCmd),
    /// The credential inbound public calls (phone-to-app) are recorded against (Part E
    /// §45). No owner = inbound public calls are refused.
    #[command(subcommand)]
    InboundOwner(InboundOwnerCmd),
    /// Enterprise calling administrative capabilities of an environment (Part F §18):
    /// the organization-level second key. High-risk capabilities (public forwarding,
    /// recording administration) are granted here only, never by the customer API.
    #[command(subcommand)]
    EnterpriseCapabilities(EnterpriseCapabilitiesCmd),
}

#[derive(Clone, Copy, ValueEnum)]
#[allow(clippy::enum_variant_names)] // the CLI values mirror MANAGE_* capabilities
enum CapabilityArg {
    ManageRouting,
    ManagePublicForwarding,
    ManageVoicemail,
    ManageRecording,
}

impl From<CapabilityArg> for calling::enterprise::policy::AdminCapability {
    fn from(c: CapabilityArg) -> Self {
        use calling::enterprise::policy::AdminCapability as C;
        match c {
            CapabilityArg::ManageRouting => C::ManageRouting,
            CapabilityArg::ManagePublicForwarding => C::ManagePublicForwarding,
            CapabilityArg::ManageVoicemail => C::ManageVoicemail,
            CapabilityArg::ManageRecording => C::ManageRecording,
        }
    }
}

#[derive(Subcommand)]
enum EnterpriseCapabilitiesCmd {
    /// Replace the capability set of the environment's baseline policy (audited as
    /// OPERATOR).
    Set {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[arg(long = "capability", value_enum)]
        capabilities: Vec<CapabilityArg>,
    },
    /// The environment's effective baseline policy.
    Show(EnvironmentPath),
}

#[derive(Subcommand)]
enum SpendPolicyCmd {
    Set {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[arg(long)]
        currency: String,
        /// Reserved allowance per paid leg (minor units).
        #[arg(long)]
        leg_allowance_minor: i64,
        #[arg(long, default_value_t = 3600)]
        max_leg_seconds: i32,
        #[arg(long)]
        daily_limit_minor: i64,
        #[arg(long)]
        monthly_limit_minor: i64,
        #[arg(long, default_value_t = 10)]
        max_concurrent_legs: i32,
        #[arg(long, default_value_t = 30)]
        max_originations_per_minute: i32,
        #[arg(long, default_value_t = 10)]
        max_per_party_per_hour: i32,
        /// Optional: paid legs per requesting principal per hour.
        #[arg(long)]
        max_per_principal_per_hour: Option<i32>,
        /// Optional: paid legs per service account per hour.
        #[arg(long)]
        max_per_service_account_per_hour: Option<i32>,
        /// Optional: paid legs per communication context per hour.
        #[arg(long)]
        max_per_context_per_hour: Option<i32>,
        /// Store the policy disabled (every paid leg refused).
        #[arg(long)]
        disabled: bool,
    },
    Show(EnvironmentPath),
}

#[derive(Subcommand)]
enum InboundOwnerCmd {
    Set {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[command(flatten)]
        credential: CredentialRef,
        #[arg(long)]
        disabled: bool,
    },
    Show(EnvironmentPath),
}

#[derive(Subcommand)]
enum RoutingPolicyCmd {
    /// Replace the routing policy of an environment with a validated JSON document
    /// (camelCase RoutingPolicy; unknown fields are refused).
    Set {
        #[command(flatten)]
        environment: EnvironmentPath,
        /// Path of the policy document.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// The effective routing policy (the restrictive default when none is stored).
    Show(EnvironmentPath),
}

#[derive(Clone, Copy, ValueEnum)]
enum ClassificationArg {
    FirstParty,
    External,
}

impl From<ClassificationArg> for TenantClassification {
    fn from(c: ClassificationArg) -> Self {
        match c {
            ClassificationArg::FirstParty => TenantClassification::FirstParty,
            ClassificationArg::External => TenantClassification::External,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum EnvironmentArg {
    Sandbox,
    Test,
    Production,
}

impl From<EnvironmentArg> for EnvironmentKind {
    fn from(e: EnvironmentArg) -> Self {
        match e {
            EnvironmentArg::Sandbox => EnvironmentKind::Sandbox,
            EnvironmentArg::Test => EnvironmentKind::Test,
            EnvironmentArg::Production => EnvironmentKind::Production,
        }
    }
}

/// Principal categories registrable from an opaque reference (SERVICE_ACCOUNT principals
/// use `principal register-service-account`).
#[derive(Clone, Copy, ValueEnum)]
enum PrincipalKindArg {
    NativeCfUser,
    ExternalApplicationPrincipal,
    EnterpriseMember,
    TelephoneEndpoint,
    TemporaryExternalParticipant,
    BotOrService,
}

impl From<PrincipalKindArg> for PrincipalKind {
    fn from(k: PrincipalKindArg) -> Self {
        match k {
            PrincipalKindArg::NativeCfUser => PrincipalKind::NativeCfUser,
            PrincipalKindArg::ExternalApplicationPrincipal => {
                PrincipalKind::ExternalApplicationPrincipal
            }
            PrincipalKindArg::EnterpriseMember => PrincipalKind::EnterpriseMember,
            PrincipalKindArg::TelephoneEndpoint => PrincipalKind::TelephoneEndpoint,
            PrincipalKindArg::TemporaryExternalParticipant => {
                PrincipalKind::TemporaryExternalParticipant
            }
            PrincipalKindArg::BotOrService => PrincipalKind::BotOrService,
        }
    }
}

#[derive(Args, Clone)]
struct TenantPath {
    /// Tenant slug.
    #[arg(long)]
    tenant: String,
}

#[derive(Args, Clone)]
struct ApplicationPath {
    #[command(flatten)]
    tenant: TenantPath,
    /// Application slug.
    #[arg(long)]
    application: String,
}

#[derive(Args, Clone)]
struct EnvironmentPath {
    #[command(flatten)]
    application: ApplicationPath,
    #[arg(long, value_enum)]
    environment: EnvironmentArg,
}

#[derive(Args, Clone)]
struct ServiceAccountPath {
    #[command(flatten)]
    environment: EnvironmentPath,
    /// Service account name.
    #[arg(long = "service-account")]
    service_account: String,
}

#[derive(Args, Clone)]
struct CredentialRef {
    /// Credential id (UUID) or public id (24 hex characters). Never the secret.
    #[arg(long)]
    credential: String,
}

#[derive(Args, Clone)]
struct PrincipalRef {
    /// Principal id (UUID).
    #[arg(long)]
    principal: Uuid,
}

#[derive(Subcommand)]
enum TenantCmd {
    Create {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        classification: ClassificationArg,
        /// ISO 4217 currency of telecom-spend limits (default USD; immutable).
        #[arg(long)]
        billing_currency: Option<String>,
    },
    List,
    Show(TenantPath),
    Suspend(TenantPath),
    Reactivate(TenantPath),
}

#[derive(Subcommand)]
enum ApplicationCmd {
    Create {
        #[command(flatten)]
        tenant: TenantPath,
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: String,
    },
    List(TenantPath),
    Suspend(ApplicationPath),
    Reactivate(ApplicationPath),
}

#[derive(Subcommand)]
enum EnvironmentCmd {
    Create(EnvironmentPath),
    List(ApplicationPath),
    Suspend(EnvironmentPath),
    Reactivate(EnvironmentPath),
    /// Allow / forbid real public-network telecom effects (PRODUCTION only).
    SetLiveTelecom {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[arg(long, action = clap::ArgAction::Set)]
        allowed: bool,
    },
}

#[derive(Subcommand)]
enum ServiceAccountCmd {
    Create {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    List(EnvironmentPath),
    Suspend(ServiceAccountPath),
    Reactivate(ServiceAccountPath),
    /// Permanently revoke (terminal; all its credentials stop authenticating).
    Revoke(ServiceAccountPath),
}

#[derive(Subcommand)]
enum ScopeCmd {
    /// Show the server-authoritative scope registry.
    Registry,
    /// Allow an application to grant `scope` to its credentials.
    Grant {
        #[command(flatten)]
        application: ApplicationPath,
        #[arg(long)]
        scope: String,
    },
    /// Withdraw `scope` from the application and every credential of it.
    Revoke {
        #[command(flatten)]
        application: ApplicationPath,
        #[arg(long)]
        scope: String,
    },
    List(ApplicationPath),
}

#[derive(Subcommand)]
enum CredentialCmd {
    /// Issue a credential. The raw credential is printed once.
    Create {
        #[command(flatten)]
        service_account: ServiceAccountPath,
        /// Scope to grant (repeatable); must be allowed for the application.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        expires_in_days: Option<i64>,
    },
    /// Rotate a credential. The successor's raw credential is printed once.
    Rotate {
        #[command(flatten)]
        credential: CredentialRef,
        /// Minutes the predecessor keeps working (default from configuration).
        #[arg(long)]
        overlap_minutes: Option<i64>,
        /// Replacement scope set (repeatable); default: the predecessor's scopes.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        expires_in_days: Option<i64>,
    },
    /// Revoke a credential immediately (idempotent).
    Revoke(CredentialRef),
    Show(CredentialRef),
    List {
        #[command(flatten)]
        application: ApplicationPath,
        #[arg(long, value_enum)]
        environment: Option<EnvironmentArg>,
        /// Requires --environment.
        #[arg(long = "service-account", requires = "environment")]
        service_account: Option<String>,
    },
}

#[derive(Subcommand)]
enum PrincipalCmd {
    /// Register a principal identified by an opaque reference.
    Register {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[arg(long, value_enum)]
        kind: PrincipalKindArg,
        /// Opaque reference (no plaintext phone numbers / contact data).
        #[arg(long)]
        reference: String,
    },
    /// Register a SERVICE_ACCOUNT principal explicitly linked to a service account.
    RegisterServiceAccount(ServiceAccountPath),
    List(EnvironmentPath),
    Show(PrincipalRef),
    Suspend(PrincipalRef),
    Reactivate(PrincipalRef),
    Deactivate(PrincipalRef),
}

#[derive(Args, Clone)]
struct LimitArgs {
    #[arg(long)]
    requests_per_minute: Option<i32>,
    #[arg(long)]
    max_concurrent_communications: Option<i32>,
    /// Daily telecom-spend ceiling in minor units of the tenant billing currency.
    #[arg(long)]
    telecom_spend_daily_minor: Option<i64>,
    /// Monthly telecom-spend ceiling in minor units of the tenant billing currency.
    #[arg(long)]
    telecom_spend_monthly_minor: Option<i64>,
}

impl From<LimitArgs> for LimitValues {
    fn from(a: LimitArgs) -> Self {
        LimitValues {
            requests_per_minute: a.requests_per_minute,
            max_concurrent_communications: a.max_concurrent_communications,
            telecom_spend_daily_limit_minor: a.telecom_spend_daily_minor,
            telecom_spend_monthly_limit_minor: a.telecom_spend_monthly_minor,
        }
    }
}

#[derive(Subcommand)]
enum LimitsCmd {
    /// Replace the tenant-level limit policy (omitted = not configured).
    SetTenant {
        #[command(flatten)]
        tenant: TenantPath,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Replace the application-level limit policy (omitted = not configured).
    SetApplication {
        #[command(flatten)]
        application: ApplicationPath,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Replace the service-account-level limit policy (omitted = not configured): an
    /// aggregate ceiling across every credential of the service account.
    SetServiceAccount {
        #[command(flatten)]
        service_account: ServiceAccountPath,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Replace a principal's own limit policy (omitted = not configured), shared by
    /// every credential acting for the principal.
    SetPrincipal {
        #[command(flatten)]
        principal: PrincipalRef,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Replace the credential-level limit policy (omitted = not configured).
    SetCredential {
        #[command(flatten)]
        credential: CredentialRef,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Effective (resolved) policy of a credential.
    Effective(CredentialRef),
}

struct Admin {
    platform: PlatformService,
    db: sea_orm::DatabaseConnection,
    clock: SystemClock,
}

impl Admin {
    async fn tenant(&self, path: &TenantPath) -> Result<TenantView, PlatformError> {
        self.platform.tenant_by_slug(&path.tenant).await
    }

    async fn application(&self, path: &ApplicationPath) -> Result<ApplicationView, PlatformError> {
        let tenant = self.tenant(&path.tenant).await?;
        self.platform
            .application_by_slug(tenant.id, &path.application)
            .await
    }

    async fn environment(&self, path: &EnvironmentPath) -> Result<EnvironmentView, PlatformError> {
        let application = self.application(&path.application).await?;
        self.platform
            .environment_by_kind(application.id, path.environment.into())
            .await
    }

    async fn service_account(
        &self,
        path: &ServiceAccountPath,
    ) -> Result<ServiceAccountView, PlatformError> {
        let environment = self.environment(&path.environment).await?;
        self.platform
            .service_account_by_name(environment.id, &path.service_account)
            .await
    }

    async fn credential_id(
        &self,
        reference: &CredentialRef,
    ) -> Result<CredentialId, PlatformError> {
        let value = reference.credential.trim();
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(CredentialId::from_uuid(id));
        }
        if value.len() == 24 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(self
                .platform
                .credential_by_public_id(&value.to_ascii_lowercase())
                .await?
                .id);
        }
        Err(PlatformError::Validation(
            "--credential must be a credential id (UUID) or public id (24 hex characters)"
                .to_string(),
        ))
    }

    fn expiry(
        &self,
        days: Option<i64>,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, PlatformError> {
        match days {
            None => Ok(None),
            Some(d) if d > 0 && d <= 3650 => Ok(Some(self.clock.now() + Duration::days(d))),
            Some(_) => Err(PlatformError::Validation(
                "--expires-in-days must be between 1 and 3650".to_string(),
            )),
        }
    }
}

fn print<T: Serialize>(value: &T) -> Result<(), PlatformError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|_| PlatformError::Validation("output serialization failed".to_string()))?;
    println!("{text}");
    Ok(())
}

/// The single place the raw credential is written: stdout, once, at creation/rotation.
fn print_issued(issued: IssuedCredential) -> Result<(), PlatformError> {
    let output = json!({
        "credential": issued.credential,
        "secret": issued.secret.expose_once(),
        "notice": "Store this secret now. It is shown only once and cannot be recovered; \
                   if lost, rotate or issue a new credential.",
    });
    print(&output)
}

async fn run(admin: &Admin, command: Command) -> Result<(), PlatformError> {
    let p = &admin.platform;
    match command {
        Command::Tenant(cmd) => match cmd {
            TenantCmd::Create {
                slug,
                name,
                classification,
                billing_currency,
            } => print(
                &p.create_tenant(NewTenant {
                    slug,
                    display_name: name,
                    classification: classification.into(),
                    billing_currency,
                })
                .await?,
            ),
            TenantCmd::List => print(&p.list_tenants().await?),
            TenantCmd::Show(path) => print(&admin.tenant(&path).await?),
            TenantCmd::Suspend(path) => {
                let t = admin.tenant(&path).await?;
                print(&p.set_tenant_status(t.id, TenantStatus::Suspended).await?)
            }
            TenantCmd::Reactivate(path) => {
                let t = admin.tenant(&path).await?;
                print(&p.set_tenant_status(t.id, TenantStatus::Active).await?)
            }
        },
        Command::Application(cmd) => match cmd {
            ApplicationCmd::Create { tenant, slug, name } => {
                let t = admin.tenant(&tenant).await?;
                print(
                    &p.create_application(
                        t.id,
                        NewApplication {
                            slug,
                            display_name: name,
                        },
                    )
                    .await?,
                )
            }
            ApplicationCmd::List(tenant) => {
                let t = admin.tenant(&tenant).await?;
                print(&p.list_applications(t.id).await?)
            }
            ApplicationCmd::Suspend(path) => {
                let a = admin.application(&path).await?;
                print(
                    &p.set_application_status(a.id, ApplicationStatus::Suspended)
                        .await?,
                )
            }
            ApplicationCmd::Reactivate(path) => {
                let a = admin.application(&path).await?;
                print(
                    &p.set_application_status(a.id, ApplicationStatus::Active)
                        .await?,
                )
            }
        },
        Command::Environment(cmd) => match cmd {
            EnvironmentCmd::Create(path) => {
                let a = admin.application(&path.application).await?;
                print(&p.create_environment(a.id, path.environment.into()).await?)
            }
            EnvironmentCmd::List(path) => {
                let a = admin.application(&path).await?;
                print(&p.list_environments(a.id).await?)
            }
            EnvironmentCmd::Suspend(path) => {
                let e = admin.environment(&path).await?;
                print(
                    &p.set_environment_status(e.id, EnvironmentStatus::Suspended)
                        .await?,
                )
            }
            EnvironmentCmd::Reactivate(path) => {
                let e = admin.environment(&path).await?;
                print(
                    &p.set_environment_status(e.id, EnvironmentStatus::Active)
                        .await?,
                )
            }
            EnvironmentCmd::SetLiveTelecom {
                environment,
                allowed,
            } => {
                let e = admin.environment(&environment).await?;
                print(&p.set_live_telecom_allowed(e.id, allowed).await?)
            }
        },
        Command::ServiceAccount(cmd) => match cmd {
            ServiceAccountCmd::Create {
                environment,
                name,
                description,
            } => {
                let e = admin.environment(&environment).await?;
                print(
                    &p.create_service_account(e.id, NewServiceAccount { name, description })
                        .await?,
                )
            }
            ServiceAccountCmd::List(path) => {
                let e = admin.environment(&path).await?;
                print(&p.list_service_accounts(e.id).await?)
            }
            ServiceAccountCmd::Suspend(path) => {
                let sa = admin.service_account(&path).await?;
                print(
                    &p.set_service_account_status(sa.id, ServiceAccountStatus::Suspended)
                        .await?,
                )
            }
            ServiceAccountCmd::Reactivate(path) => {
                let sa = admin.service_account(&path).await?;
                print(
                    &p.set_service_account_status(sa.id, ServiceAccountStatus::Active)
                        .await?,
                )
            }
            ServiceAccountCmd::Revoke(path) => {
                let sa = admin.service_account(&path).await?;
                print(
                    &p.set_service_account_status(sa.id, ServiceAccountStatus::Revoked)
                        .await?,
                )
            }
        },
        Command::Scope(cmd) => match cmd {
            ScopeCmd::Registry => print(&p.registry().definitions().collect::<Vec<_>>()),
            ScopeCmd::Grant { application, scope } => {
                let a = admin.application(&application).await?;
                print(&p.grant_application_scope(a.id, &scope).await?)
            }
            ScopeCmd::Revoke { application, scope } => {
                let a = admin.application(&application).await?;
                let removed = p.revoke_application_scope(a.id, &scope).await?;
                print(&json!({ "application_id": a.id, "scope": scope, "removed": removed }))
            }
            ScopeCmd::List(path) => {
                let a = admin.application(&path).await?;
                print(&p.list_application_scopes(a.id).await?)
            }
        },
        Command::Credential(cmd) => match cmd {
            CredentialCmd::Create {
                service_account,
                scopes,
                label,
                expires_in_days,
            } => {
                let sa = admin.service_account(&service_account).await?;
                let issued = p
                    .issue_credential(
                        sa.id,
                        IssueCredential {
                            scopes,
                            label,
                            not_before: None,
                            expires_at: admin.expiry(expires_in_days)?,
                        },
                    )
                    .await?;
                print_issued(issued)
            }
            CredentialCmd::Rotate {
                credential,
                overlap_minutes,
                scopes,
                label,
                expires_in_days,
            } => {
                let id = admin.credential_id(&credential).await?;
                let issued = p
                    .rotate_credential(
                        id,
                        RotateCredential {
                            overlap: overlap_minutes.map(Duration::minutes),
                            scopes: (!scopes.is_empty()).then_some(scopes),
                            label,
                            expires_at: admin.expiry(expires_in_days)?,
                        },
                    )
                    .await?;
                print_issued(issued)
            }
            CredentialCmd::Revoke(reference) => {
                let id = admin.credential_id(&reference).await?;
                print(&p.revoke_credential(id).await?)
            }
            CredentialCmd::Show(reference) => {
                let id = admin.credential_id(&reference).await?;
                print(&p.credential(id).await?)
            }
            CredentialCmd::List {
                application,
                environment,
                service_account,
            } => {
                let a = admin.application(&application).await?;
                let filter = match (environment, service_account) {
                    (None, _) => CredentialFilter::Application(a.id),
                    (Some(kind), None) => {
                        let e = p.environment_by_kind(a.id, kind.into()).await?;
                        CredentialFilter::Environment(e.id)
                    }
                    (Some(kind), Some(name)) => {
                        let e = p.environment_by_kind(a.id, kind.into()).await?;
                        let sa = p.service_account_by_name(e.id, &name).await?;
                        CredentialFilter::ServiceAccount(sa.id)
                    }
                };
                print(&p.list_credentials(filter).await?)
            }
        },
        Command::Principal(cmd) => match cmd {
            PrincipalCmd::Register {
                environment,
                kind,
                reference,
            } => {
                let e = admin.environment(&environment).await?;
                let (principal, created) = p
                    .register_principal(
                        e.id,
                        NewPrincipal::External {
                            kind: kind.into(),
                            external_reference: reference,
                        },
                    )
                    .await?;
                print(&json!({ "principal": principal, "created": created }))
            }
            PrincipalCmd::RegisterServiceAccount(path) => {
                let sa = admin.service_account(&path).await?;
                let (principal, created) = p
                    .register_principal(sa.environment_id, NewPrincipal::ServiceAccount(sa.id))
                    .await?;
                print(&json!({ "principal": principal, "created": created }))
            }
            PrincipalCmd::List(path) => {
                let e = admin.environment(&path).await?;
                print(&p.list_principals(e.id).await?)
            }
            PrincipalCmd::Show(r) => {
                print(&p.principal(PrincipalId::from_uuid(r.principal)).await?)
            }
            PrincipalCmd::Suspend(r) => print(
                &p.set_principal_status(
                    PrincipalId::from_uuid(r.principal),
                    PrincipalStatus::Suspended,
                )
                .await?,
            ),
            PrincipalCmd::Reactivate(r) => print(
                &p.set_principal_status(
                    PrincipalId::from_uuid(r.principal),
                    PrincipalStatus::Active,
                )
                .await?,
            ),
            PrincipalCmd::Deactivate(r) => print(
                &p.set_principal_status(
                    PrincipalId::from_uuid(r.principal),
                    PrincipalStatus::Deactivated,
                )
                .await?,
            ),
        },
        Command::Limits(cmd) => match cmd {
            LimitsCmd::SetTenant { tenant, limits } => {
                let t = admin.tenant(&tenant).await?;
                print(&p.set_tenant_limits(t.id, limits.into()).await?)
            }
            LimitsCmd::SetApplication {
                application,
                limits,
            } => {
                let a = admin.application(&application).await?;
                print(&p.set_application_limits(a.id, limits.into()).await?)
            }
            LimitsCmd::SetServiceAccount {
                service_account,
                limits,
            } => {
                let sa = admin.service_account(&service_account).await?;
                print(&p.set_service_account_limits(sa.id, limits.into()).await?)
            }
            LimitsCmd::SetPrincipal { principal, limits } => print(
                &p.set_principal_limits(PrincipalId::from_uuid(principal.principal), limits.into())
                    .await?,
            ),
            LimitsCmd::SetCredential { credential, limits } => {
                let id = admin.credential_id(&credential).await?;
                print(&p.set_credential_limits(id, limits.into()).await?)
            }
            LimitsCmd::Effective(reference) => {
                let id = admin.credential_id(&reference).await?;
                print(&p.effective_limits(id).await?)
            }
        },
        Command::RoutingPolicy(cmd) => {
            use calling::routing::{policy::RoutingPolicy, store};
            let db_error = |_| PlatformError::Database {
                summary: "routing policy".to_string(),
                connectivity: false,
            };
            match cmd {
                RoutingPolicyCmd::Set { environment, file } => {
                    let env = admin.environment(&environment).await?;
                    let text = std::fs::read_to_string(&file).map_err(|_| {
                        PlatformError::Validation("cannot read the policy file".to_string())
                    })?;
                    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                        PlatformError::Validation("the policy file is not JSON".to_string())
                    })?;
                    let policy = RoutingPolicy::parse(&value)
                        .map_err(|e| PlatformError::Validation(e.to_string()))?;
                    let revision = store::store_policy(
                        &admin.db,
                        env.tenant_id.as_uuid(),
                        env.application_id.as_uuid(),
                        env.id.as_uuid(),
                        &policy,
                        admin.clock.now(),
                    )
                    .await
                    .map_err(|e| match e {
                        store::PolicyStoreError::Invalid(m) => {
                            PlatformError::Validation(m.to_string())
                        }
                        store::PolicyStoreError::Database(_) => PlatformError::Database {
                            summary: "routing policy".to_string(),
                            connectivity: false,
                        },
                    })?;
                    print(
                        &json!({ "environmentId": env.id, "revision": revision, "policy": policy }),
                    )
                }
                RoutingPolicyCmd::Show(environment) => {
                    let env = admin.environment(&environment).await?;
                    let effective = store::load_policy(&admin.db, env.id.as_uuid())
                        .await
                        .map_err(db_error)?;
                    print(&json!({
                        "environmentId": env.id,
                        "revision": effective.revision,
                        "policyVersion": effective.version_label(),
                        "policy": effective.policy,
                    }))
                }
            }
        }
        Command::EnterpriseCapabilities(cmd) => {
            let enterprise = calling::enterprise::EnterpriseService::new(
                admin.db.clone(),
                std::sync::Arc::new(calling::domain::SystemClock),
                calling::enterprise::EnterpriseFlags::default(),
            );
            let refused = |e: calling::enterprise::EnterpriseError| match e {
                calling::enterprise::EnterpriseError::Validation(m) => {
                    PlatformError::Validation(m.to_string())
                }
                calling::enterprise::EnterpriseError::NotFound => {
                    PlatformError::NotFound("environment")
                }
                _ => PlatformError::Database {
                    summary: "enterprise policy".to_string(),
                    connectivity: false,
                },
            };
            match cmd {
                EnterpriseCapabilitiesCmd::Set {
                    environment,
                    capabilities,
                } => {
                    let env = admin.environment(&environment).await?;
                    let scope = calling::calls::command::CallScope::of_environment(&env);
                    let view = enterprise
                        .operator_set_capabilities(
                            scope,
                            capabilities.into_iter().map(Into::into).collect(),
                        )
                        .await
                        .map_err(refused)?;
                    print(&view)
                }
                EnterpriseCapabilitiesCmd::Show(environment) => {
                    let env = admin.environment(&environment).await?;
                    let scope = calling::calls::command::CallScope::of_environment(&env);
                    print(
                        &enterprise
                            .effective_default_policy(scope)
                            .await
                            .map_err(refused)?,
                    )
                }
            }
        }
        Command::SpendPolicy(cmd) => {
            use calling::telephony::spend;
            let db_error = |_| PlatformError::Database {
                summary: "spend policy".to_string(),
                connectivity: false,
            };
            match cmd {
                SpendPolicyCmd::Set {
                    environment,
                    currency,
                    leg_allowance_minor,
                    max_leg_seconds,
                    daily_limit_minor,
                    monthly_limit_minor,
                    max_concurrent_legs,
                    max_originations_per_minute,
                    max_per_party_per_hour,
                    max_per_principal_per_hour,
                    max_per_service_account_per_hour,
                    max_per_context_per_hour,
                    disabled,
                } => {
                    let env = admin.environment(&environment).await?;
                    let currency = currency.trim().to_ascii_uppercase();
                    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
                        return Err(PlatformError::Validation(
                            "currency must be an ISO 4217 code".to_string(),
                        ));
                    }
                    let policy = spend::SpendPolicy {
                        currency,
                        leg_allowance_minor,
                        max_leg_seconds,
                        daily_limit_minor,
                        monthly_limit_minor,
                        max_concurrent_legs,
                        max_originations_per_minute,
                        max_per_party_per_hour,
                        max_per_principal_per_hour,
                        max_per_service_account_per_hour,
                        max_per_context_per_hour,
                        enabled: !disabled,
                    };
                    let revision = spend::store_policy(
                        &admin.db,
                        spend::SpendScope {
                            tenant_id: env.tenant_id.as_uuid(),
                            application_id: env.application_id.as_uuid(),
                            environment_id: env.id.as_uuid(),
                        },
                        &policy,
                        admin.clock.now(),
                    )
                    .await
                    .map_err(|_| {
                        PlatformError::Validation(
                            "the spend policy was refused (limits must be positive and consistent)"
                                .to_string(),
                        )
                    })?;
                    print(&json!({ "environmentId": env.id, "revision": revision }))
                }
                SpendPolicyCmd::Show(environment) => {
                    let env = admin.environment(&environment).await?;
                    let p = spend::policy(&admin.db, env.id.as_uuid())
                        .await
                        .map_err(db_error)?;
                    print(&json!({
                        "environmentId": env.id,
                        "policy": p.map(|p| json!({
                            "currency": p.currency,
                            "legAllowanceMinor": p.leg_allowance_minor,
                            "maxLegSeconds": p.max_leg_seconds,
                            "dailyLimitMinor": p.daily_limit_minor,
                            "monthlyLimitMinor": p.monthly_limit_minor,
                            "maxConcurrentLegs": p.max_concurrent_legs,
                            "maxOriginationsPerMinute": p.max_originations_per_minute,
                            "maxPerPartyPerHour": p.max_per_party_per_hour,
                            "maxPerPrincipalPerHour": p.max_per_principal_per_hour,
                            "maxPerServiceAccountPerHour": p.max_per_service_account_per_hour,
                            "maxPerContextPerHour": p.max_per_context_per_hour,
                            "enabled": p.enabled,
                        })),
                    }))
                }
            }
        }
        Command::InboundOwner(cmd) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            let db_error = |_| PlatformError::Database {
                summary: "inbound owner".to_string(),
                connectivity: false,
            };
            match cmd {
                InboundOwnerCmd::Set {
                    environment,
                    credential,
                    disabled,
                } => {
                    let env = admin.environment(&environment).await?;
                    let credential_id = admin.credential_id(&credential).await?;
                    let row = admin
                        .db
                        .query_one(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, \
                               service_account_id, credential_id, enabled, revision, created_at, updated_at) \
                             SELECT c.tenant_id, c.application_id, c.environment_id, c.service_account_id, c.id, \
                               $3, 1, $4, $4 FROM platform_api_credentials c \
                             WHERE c.id = $1 AND c.environment_id = $2 \
                             ON CONFLICT (environment_id) DO UPDATE SET service_account_id = EXCLUDED.service_account_id, \
                               credential_id = EXCLUDED.credential_id, enabled = EXCLUDED.enabled, \
                               revision = call_inbound_owners.revision + 1, \
                               updated_at = GREATEST(EXCLUDED.updated_at, call_inbound_owners.updated_at) \
                             RETURNING revision",
                            vec![
                                credential_id.as_uuid().into(),
                                env.id.as_uuid().into(),
                                (!disabled).into(),
                                admin.clock.now().into(),
                            ],
                        ))
                        .await
                        .map_err(db_error)?
                        .ok_or(PlatformError::NotFound("credential in this environment"))?;
                    let revision: i32 = row.try_get_by_index(0).map_err(db_error)?;
                    print(
                        &json!({ "environmentId": env.id, "credentialId": credential_id, "revision": revision }),
                    )
                }
                InboundOwnerCmd::Show(environment) => {
                    let env = admin.environment(&environment).await?;
                    let row = admin
                        .db
                        .query_one(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "SELECT credential_id, enabled, revision FROM call_inbound_owners WHERE environment_id = $1",
                            vec![env.id.as_uuid().into()],
                        ))
                        .await
                        .map_err(db_error)?;
                    let owner = match row {
                        Some(r) => Some(json!({
                            "credentialId": r.try_get_by_index::<Uuid>(0).map_err(db_error)?,
                            "enabled": r.try_get_by_index::<bool>(1).map_err(db_error)?,
                            "revision": r.try_get_by_index::<i32>(2).map_err(db_error)?,
                        })),
                        None => None,
                    };
                    print(&json!({ "environmentId": env.id, "inboundOwner": owner }))
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    dotenvy::dotenv().ok();
    let config = match CallingConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("platform_admin: configuration error: {err}");
            return ExitCode::from(2);
        }
    };
    telemetry::init_cli();

    let mut options = ConnectOptions::new(config.database_url().to_string());
    options
        .max_connections(2)
        .connect_timeout(StdDuration::from_secs(10))
        .sqlx_logging(false);
    let db = match Database::connect(options).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!(
                "platform_admin: cannot connect to the Calling database ({})",
                summarize_db_error(&err)
            );
            return ExitCode::from(2);
        }
    };
    let admin = Admin {
        db: db.clone(),
        platform: PlatformService::new(
            db,
            Arc::new(SystemClock),
            Arc::new(config.keyring),
            Arc::new(ScopeRegistry::standard()),
            config.credential_policy,
            config.limit_defaults,
        ),
        clock: SystemClock,
    };
    match run(&admin, cli.command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("platform_admin: {err}");
            ExitCode::FAILURE
        }
    }
}

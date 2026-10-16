//! `platform-access-admin` — operational administration of the shared platform identity
//! authority (tenants, applications, environments, service accounts, scopes, API
//! credentials, principals) and the controlled identity hand-off (Part IV §B2).
//!
//! Uses the same audited `PlatformService` operations as the operator administration API
//! and connects directly to the Platform Access database (`DATABASE_URL`,
//! `API_CREDENTIAL_PEPPER`). Output is JSON on stdout. `credential create` and
//! `credential rotate` print the raw credential exactly once; no command can recover it.
//!
//! Operator audit: every mutating command requires the acting operator — `--operator
//! <id>` or `CF_OPERATOR_ID` — and is refused (exit 2, nothing changed) without one. Each
//! mutation writes exactly one `platform_access_audit` row in the same transaction.
//!
//! All arguments are generic: there are no product-specific commands or defaults.
//! Calling-owned policy (limits, routing / spend policy, enterprise capabilities, …)
//! stays in Calling's `platform_admin`.

use std::{io::Write as _, process::ExitCode, sync::Arc, time::Duration as StdDuration};

use chrono::Duration;
use clap::{Args, Parser, Subcommand, ValueEnum};
use platform_access::{
    config::PlatformAccessConfig,
    domain::{
        ApplicationStatus, Clock, CredentialId, EnvironmentKind, EnvironmentStatus, PrincipalId,
        PrincipalKind, PrincipalStatus, ServiceAccountStatus, SystemClock, TenantClassification,
        TenantStatus,
    },
    identity::{
        applications::{ApplicationView, NewApplication},
        changes,
        credentials::{CredentialFilter, IssueCredential, IssuedCredential, RotateCredential},
        environments::EnvironmentView,
        error::summarize_db_error,
        handoff,
        operator::OperatorContext,
        principals::NewPrincipal,
        scopes::ScopeRegistry,
        service_accounts::{NewServiceAccount, ServiceAccountView},
        tenants::{NewTenant, TenantView},
        PlatformError, PlatformService,
    },
};
use sea_orm::{ConnectOptions, Database};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "platform-access-admin",
    version,
    about = "CF Meet Platform Access administration"
)]
struct Cli {
    /// The acting operator (opaque id, `^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$`); required by
    /// every mutating command and recorded in the operator audit.
    #[arg(long, global = true, env = "CF_OPERATOR_ID")]
    operator: Option<String>,
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
    /// Controlled identity hand-off: import from the previous authority, export for
    /// recovery, receipts, change feed.
    #[command(subcommand)]
    Identity(IdentityCmd),
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
        /// Restrict the scope to these environment kinds (repeatable; default: all).
        #[arg(long = "environment-kind", value_enum)]
        environment_kinds: Vec<EnvironmentArg>,
    },
    /// Grant a PRIVILEGED operator scope (`numbers.admin`) to an application of a
    /// FIRST_PARTY tenant. Audited as PRIVILEGED_SCOPE_GRANTED.
    GrantPrivileged {
        #[command(flatten)]
        application: ApplicationPath,
        #[arg(long)]
        scope: String,
        #[arg(long = "environment-kind", value_enum)]
        environment_kinds: Vec<EnvironmentArg>,
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
    /// Restrict a credential to act only for the given principals (repeatable; none =
    /// remove the restriction).
    RestrictPrincipals {
        #[command(flatten)]
        credential: CredentialRef,
        #[arg(long = "principal")]
        principals: Vec<Uuid>,
    },
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
    /// Minimize a principal for good (Part H §80.42, audited): DEACTIVATED for ever, its
    /// reference replaced by a non-identifying tombstone. Consumers (Calling) apply it
    /// from the change feed; Calling's own participation minimization runs through
    /// Calling's `platform_admin retention tombstone`.
    Tombstone(PrincipalRef),
}

#[derive(Subcommand)]
enum IdentityCmd {
    /// Import an identity bundle exported by the previous authority (one transaction;
    /// idempotent per bundle digest). Prints the receipt.
    Import {
        /// Bundle file (never logged; contains keyed verifiers, no secrets).
        #[arg(long)]
        bundle: std::path::PathBuf,
    },
    /// Export every identity row (recovery / rollback format) to a 0600 file.
    Export {
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Show the receipt of an imported bundle.
    Receipt {
        #[arg(long)]
        digest: String,
    },
    /// Read a page of the identity change feed.
    Changes {
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 100)]
        limit: u64,
    },
}

struct Admin {
    platform: PlatformService,
    clock: SystemClock,
    /// The acting operator; present for every mutating command (checked before connect).
    operator: Option<OperatorContext>,
}

impl Admin {
    fn op(&self) -> Result<&OperatorContext, PlatformError> {
        self.operator.as_ref().ok_or_else(|| {
            PlatformError::Validation(
                "this command changes state: --operator (or CF_OPERATOR_ID) is required"
                    .to_string(),
            )
        })
    }

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
                &p.operator_create_tenant(
                    admin.op()?,
                    NewTenant {
                        slug,
                        display_name: name,
                        classification: classification.into(),
                        billing_currency,
                    },
                )
                .await?,
            ),
            TenantCmd::List => print(&p.list_tenants().await?),
            TenantCmd::Show(path) => print(&admin.tenant(&path).await?),
            TenantCmd::Suspend(path) => {
                let t = admin.tenant(&path).await?;
                print(
                    &p.operator_set_tenant_status(admin.op()?, t.id, TenantStatus::Suspended)
                        .await?,
                )
            }
            TenantCmd::Reactivate(path) => {
                let t = admin.tenant(&path).await?;
                print(
                    &p.operator_set_tenant_status(admin.op()?, t.id, TenantStatus::Active)
                        .await?,
                )
            }
        },
        Command::Application(cmd) => match cmd {
            ApplicationCmd::Create { tenant, slug, name } => {
                let t = admin.tenant(&tenant).await?;
                print(
                    &p.operator_create_application(
                        admin.op()?,
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
                    &p.operator_set_application_status(
                        admin.op()?,
                        a.id,
                        ApplicationStatus::Suspended,
                    )
                    .await?,
                )
            }
            ApplicationCmd::Reactivate(path) => {
                let a = admin.application(&path).await?;
                print(
                    &p.operator_set_application_status(
                        admin.op()?,
                        a.id,
                        ApplicationStatus::Active,
                    )
                    .await?,
                )
            }
        },
        Command::Environment(cmd) => match cmd {
            EnvironmentCmd::Create(path) => {
                let a = admin.application(&path.application).await?;
                print(
                    &p.operator_create_environment(admin.op()?, a.id, path.environment.into())
                        .await?,
                )
            }
            EnvironmentCmd::List(path) => {
                let a = admin.application(&path).await?;
                print(&p.list_environments(a.id).await?)
            }
            EnvironmentCmd::Suspend(path) => {
                let e = admin.environment(&path).await?;
                print(
                    &p.operator_set_environment_status(
                        admin.op()?,
                        e.id,
                        EnvironmentStatus::Suspended,
                    )
                    .await?,
                )
            }
            EnvironmentCmd::Reactivate(path) => {
                let e = admin.environment(&path).await?;
                print(
                    &p.operator_set_environment_status(
                        admin.op()?,
                        e.id,
                        EnvironmentStatus::Active,
                    )
                    .await?,
                )
            }
            EnvironmentCmd::SetLiveTelecom {
                environment,
                allowed,
            } => {
                let e = admin.environment(&environment).await?;
                print(
                    &p.operator_set_live_telecom_allowed(admin.op()?, e.id, allowed)
                        .await?,
                )
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
                    &p.operator_create_service_account(
                        admin.op()?,
                        e.id,
                        NewServiceAccount { name, description },
                    )
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
                    &p.operator_set_service_account_status(
                        admin.op()?,
                        sa.id,
                        ServiceAccountStatus::Suspended,
                    )
                    .await?,
                )
            }
            ServiceAccountCmd::Reactivate(path) => {
                let sa = admin.service_account(&path).await?;
                print(
                    &p.operator_set_service_account_status(
                        admin.op()?,
                        sa.id,
                        ServiceAccountStatus::Active,
                    )
                    .await?,
                )
            }
            ServiceAccountCmd::Revoke(path) => {
                let sa = admin.service_account(&path).await?;
                print(
                    &p.operator_set_service_account_status(
                        admin.op()?,
                        sa.id,
                        ServiceAccountStatus::Revoked,
                    )
                    .await?,
                )
            }
        },
        Command::Scope(cmd) => match cmd {
            ScopeCmd::Registry => print(&p.registry().definitions().collect::<Vec<_>>()),
            ScopeCmd::Grant {
                application,
                scope,
                environment_kinds,
            } => {
                let a = admin.application(&application).await?;
                let kinds = kinds(&environment_kinds);
                print(
                    &p.operator_grant_application_scope(admin.op()?, a.id, &scope, kinds.as_deref())
                        .await?,
                )
            }
            ScopeCmd::GrantPrivileged {
                application,
                scope,
                environment_kinds,
            } => {
                let a = admin.application(&application).await?;
                let kinds = kinds(&environment_kinds);
                print(
                    &p.operator_grant_privileged_scope(admin.op()?, a.id, &scope, kinds.as_deref())
                        .await?,
                )
            }
            ScopeCmd::Revoke { application, scope } => {
                let a = admin.application(&application).await?;
                let removed = p
                    .operator_revoke_application_scope(admin.op()?, a.id, &scope)
                    .await?;
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
                    .operator_issue_credential(
                        admin.op()?,
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
                    .operator_rotate_credential(
                        admin.op()?,
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
                print(&p.operator_revoke_credential(admin.op()?, id).await?)
            }
            CredentialCmd::RestrictPrincipals {
                credential,
                principals,
            } => {
                let id = admin.credential_id(&credential).await?;
                let ids: Vec<PrincipalId> =
                    principals.into_iter().map(PrincipalId::from_uuid).collect();
                let restricted = p
                    .operator_restrict_credential_principals(admin.op()?, id, &ids)
                    .await?;
                print(&json!({
                    "credentialId": id,
                    "principalIds": restricted.iter().map(|p| p.as_uuid()).collect::<Vec<_>>(),
                }))
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
                    .operator_register_principal(
                        admin.op()?,
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
                    .operator_register_principal(
                        admin.op()?,
                        sa.environment_id,
                        NewPrincipal::ServiceAccount(sa.id),
                    )
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
                &p.operator_set_principal_status(
                    admin.op()?,
                    PrincipalId::from_uuid(r.principal),
                    PrincipalStatus::Suspended,
                )
                .await?,
            ),
            PrincipalCmd::Reactivate(r) => print(
                &p.operator_set_principal_status(
                    admin.op()?,
                    PrincipalId::from_uuid(r.principal),
                    PrincipalStatus::Active,
                )
                .await?,
            ),
            PrincipalCmd::Deactivate(r) => print(
                &p.operator_set_principal_status(
                    admin.op()?,
                    PrincipalId::from_uuid(r.principal),
                    PrincipalStatus::Deactivated,
                )
                .await?,
            ),
            PrincipalCmd::Tombstone(r) => print(
                &p.operator_tombstone_principal(admin.op()?, PrincipalId::from_uuid(r.principal))
                    .await?,
            ),
        },
        Command::Identity(cmd) => match cmd {
            IdentityCmd::Import { bundle } => {
                let bytes = std::fs::read(&bundle).map_err(|_| {
                    PlatformError::Validation("the bundle file cannot be read".to_string())
                })?;
                let parsed = handoff::parse_bundle(&bytes)?;
                drop(bytes);
                print(&p.operator_import_bundle(admin.op()?, &parsed).await?)
            }
            IdentityCmd::Export { out } => {
                let body = p.export_bundle_body().await?;
                let bytes =
                    handoff::write_bundle("cfmeet.platform-access", admin.clock.now(), &body)?;
                write_private(&out, &bytes)?;
                let digest = handoff::body_digest(
                    &serde_json::to_value(&body).unwrap_or_default(),
                );
                print(&json!({
                    "file": out.display().to_string(),
                    "bundleDigest": digest,
                    "verifierDigest": body.verifier_digest(),
                    "counts": body.counts(),
                }))
            }
            IdentityCmd::Receipt { digest } => print(
                &handoff::receipt(p.db(), &digest)
                    .await?
                    .ok_or(PlatformError::NotFound("import receipt"))?,
            ),
            IdentityCmd::Changes { after, limit } => {
                print(&changes::read_page(p.db(), after, limit).await?)
            }
        },
    }
}

fn kinds(args: &[EnvironmentArg]) -> Option<Vec<EnvironmentKind>> {
    (!args.is_empty()).then(|| args.iter().map(|k| (*k).into()).collect())
}

/// Writes `bytes` to a new file readable by the owner only.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), PlatformError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| {
        PlatformError::Validation(
            "the output file cannot be created (it must not exist yet)".to_string(),
        )
    })?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| PlatformError::Validation("the output file cannot be written".to_string()))
}

/// Whether `command` changes state (and therefore needs an operator).
fn is_mutating(command: &Command) -> bool {
    match command {
        Command::Tenant(c) => !matches!(c, TenantCmd::List | TenantCmd::Show(_)),
        Command::Application(c) => !matches!(c, ApplicationCmd::List(_)),
        Command::Environment(c) => !matches!(c, EnvironmentCmd::List(_)),
        Command::ServiceAccount(c) => !matches!(c, ServiceAccountCmd::List(_)),
        Command::Scope(c) => !matches!(c, ScopeCmd::Registry | ScopeCmd::List(_)),
        Command::Credential(c) => {
            !matches!(c, CredentialCmd::Show(_) | CredentialCmd::List { .. })
        }
        Command::Principal(c) => !matches!(c, PrincipalCmd::List(_) | PrincipalCmd::Show(_)),
        Command::Identity(c) => matches!(c, IdentityCmd::Import { .. }),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let operator = match cli.operator.as_deref().map(OperatorContext::new).transpose() {
        Ok(op) => op,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if is_mutating(&cli.command) && operator.is_none() {
        eprintln!("error: this command changes state: --operator (or CF_OPERATOR_ID) is required");
        return ExitCode::from(2);
    }
    let config = match PlatformAccessConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let mut options = ConnectOptions::new(config.database_url().to_string());
    options
        .max_connections(2)
        .connect_timeout(StdDuration::from_secs(10))
        .sqlx_logging(false);
    let db = match Database::connect(options).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "error: cannot connect to the Platform Access database ({})",
                summarize_db_error(&e)
            );
            return ExitCode::from(1);
        }
    };
    let clock = SystemClock;
    let platform = PlatformService::new(
        db,
        Arc::new(clock),
        Arc::new(config.keyring),
        Arc::new(ScopeRegistry::standard()),
        config.credential_policy,
    );
    let admin = Admin {
        platform,
        clock,
        operator,
    };
    match run(&admin, cli.command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

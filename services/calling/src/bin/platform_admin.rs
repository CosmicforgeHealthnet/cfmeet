//! `platform_admin` — operational administration of the Calling platform trust
//! foundation (tenants, applications, environments, service accounts, scopes, API
//! credentials, principals and limit policy).
//!
//! Uses the same `PlatformService` domain operations as future administration APIs and
//! connects directly to the Calling database (`DATABASE_URL`, `API_CREDENTIAL_PEPPER`).
//! Output is JSON on stdout; operator audit lines go to stderr. `credential create` and
//! `credential rotate` print the raw credential exactly once; no command can recover it.
//!
//! Operator audit (Part H §72.8): every mutating command requires the acting operator —
//! `--operator <id>` or `CF_OPERATOR_ID` — and is refused (exit 2, nothing changed)
//! without one. Each mutation writes exactly one `platform_operator_audit` row (operator,
//! generated correlation id, bounded action, target) in the same transaction as the
//! change. Read-only commands (`list`, `show`, `registry`, `effective`, `key-status`,
//! `retention list|policies`) need no operator. Retention holds, principal tombstones and
//! webhook key reseals are audited too (Part H §80, ADR 0030).
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
        operator::{audited, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
        principals::NewPrincipal,
        scopes::ScopeRegistry,
        service_accounts::{NewServiceAccount, ServiceAccountView},
        tenants::{NewTenant, TenantView},
        PlatformError, PlatformService,
    },
    retention::{
        self,
        holds::{self, HoldOwner, HoldReason, HoldScope},
        tombstone, DataClass, RetentionSettings,
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
    /// Compliance eligibility of an environment for compliance-required destination
    /// countries (Part H §78). Opaque evidence references only, never documents.
    #[command(subcommand)]
    Compliance(ComplianceCmd),
    /// Operator review of abuse reports (Part H §77).
    #[command(subcommand)]
    AbuseReport(AbuseReportCmd),
    /// Operator handling of block rules (Part H §77).
    #[command(subcommand)]
    Block(BlockCmd),
    /// Customer webhook signing-secret key rotation (Part H §74.12): reads the key ring
    /// from `CALLING_WEBHOOK_SECRET_KEY[_FILE]` / `_ID` / `_PREVIOUS[_FILE]` / `_PREVIOUS_ID`.
    #[command(subcommand)]
    Webhooks(WebhooksCmd),
    /// Retention policies and holds (Part H §80, ADR 0030). Holds are operator-audited.
    #[command(subcommand)]
    Retention(RetentionCmd),
}

#[derive(Clone, Copy, ValueEnum)]
enum HoldReasonArg {
    PolicyReview,
    Dispute,
    SecurityInvestigation,
    Other,
}

impl From<HoldReasonArg> for HoldReason {
    fn from(r: HoldReasonArg) -> Self {
        match r {
            HoldReasonArg::PolicyReview => HoldReason::PolicyReview,
            HoldReasonArg::Dispute => HoldReason::Dispute,
            HoldReasonArg::SecurityInvestigation => HoldReason::SecurityInvestigation,
            HoldReasonArg::Other => HoldReason::Other,
        }
    }
}

#[derive(Subcommand)]
enum RetentionCmd {
    /// Place a retention hold (audited): every retention action skips what it covers.
    /// A scope hold names --tenant, --application AND --environment; --platform-wide
    /// holds every class of every scope.
    Hold {
        /// A data class (e.g. CALL_RECORD, WEBHOOK_DELIVERY_HISTORY) or `*` for every class.
        #[arg(long, default_value = "*")]
        data_class: String,
        /// One resource (Call, delivery, event, incident, recording, principal, … id).
        #[arg(long)]
        resource: Option<Uuid>,
        /// Tenant slug.
        #[arg(long)]
        tenant: Option<String>,
        /// Application slug.
        #[arg(long)]
        application: Option<String>,
        #[arg(long, value_enum)]
        environment: Option<EnvironmentArg>,
        /// Hold everything, platform-wide (no scope, every class).
        #[arg(long)]
        platform_wide: bool,
        #[arg(long, value_enum)]
        reason: HoldReasonArg,
    },
    /// Release a hold (audited; idempotent).
    Release {
        #[arg(long)]
        hold: Uuid,
    },
    /// Holds (active first). `--all` includes released ones.
    List {
        #[arg(long)]
        all: bool,
    },
    /// The effective retention policies of this deployment's configuration.
    Policies,
}

#[derive(Clone, Copy, ValueEnum)]
enum EligibilityArg {
    Eligible,
    Ineligible,
    NotAssessed,
}

impl From<EligibilityArg> for calling::routing::compliance::EligibilityStatus {
    fn from(s: EligibilityArg) -> Self {
        use calling::routing::compliance::EligibilityStatus as S;
        match s {
            EligibilityArg::Eligible => S::Eligible,
            EligibilityArg::Ineligible => S::Ineligible,
            EligibilityArg::NotAssessed => S::NotAssessed,
        }
    }
}

#[derive(Subcommand)]
enum ComplianceCmd {
    /// Replace the environment's assessment (audited).
    Set {
        #[command(flatten)]
        environment: EnvironmentPath,
        #[arg(long, value_enum)]
        status: EligibilityArg,
        /// Covered destination countries, comma separated ISO 3166-1 alpha-2 (empty =
        /// every country).
        #[arg(long, value_delimiter = ',')]
        countries: Vec<String>,
        /// RFC 3339 expiry (must be in the future).
        #[arg(long)]
        expires_at: Option<String>,
        /// Opaque reference to the operator's record (`^[A-Za-z0-9._:-]{1,200}$`).
        #[arg(long)]
        evidence_ref: Option<String>,
    },
    /// Mark the environment INELIGIBLE (audited).
    Revoke(EnvironmentPath),
    /// Safe view: status, countries, expiry, whether an evidence reference exists.
    Show(EnvironmentPath),
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportStatusArg {
    Open,
    Reviewed,
    Dismissed,
}

#[derive(Subcommand)]
enum AbuseReportCmd {
    /// Move a report to OPEN / REVIEWED / DISMISSED (audited).
    SetStatus {
        #[arg(long)]
        report: Uuid,
        #[arg(long, value_enum)]
        status: ReportStatusArg,
    },
    Show {
        #[arg(long)]
        report: Uuid,
    },
}

#[derive(Subcommand)]
enum BlockCmd {
    /// Release a block rule of any scope (audited; idempotent).
    Release {
        #[arg(long)]
        block: Uuid,
    },
    Show {
        #[arg(long)]
        block: Uuid,
    },
}

impl Command {
    /// Whether the command changes state (and therefore needs an operator).
    fn is_mutating(&self) -> bool {
        match self {
            Command::Tenant(c) => !matches!(c, TenantCmd::List | TenantCmd::Show(_)),
            Command::Application(c) => !matches!(c, ApplicationCmd::List(_)),
            Command::Environment(c) => !matches!(c, EnvironmentCmd::List(_)),
            Command::ServiceAccount(c) => !matches!(c, ServiceAccountCmd::List(_)),
            Command::Scope(c) => !matches!(c, ScopeCmd::Registry | ScopeCmd::List(_)),
            Command::Credential(c) => {
                !matches!(c, CredentialCmd::Show(_) | CredentialCmd::List { .. })
            }
            Command::Principal(c) => !matches!(c, PrincipalCmd::List(_) | PrincipalCmd::Show(_)),
            Command::Limits(c) => !matches!(c, LimitsCmd::Effective(_)),
            Command::RoutingPolicy(c) => !matches!(c, RoutingPolicyCmd::Show(_)),
            Command::SpendPolicy(c) => !matches!(c, SpendPolicyCmd::Show(_)),
            Command::InboundOwner(c) => !matches!(c, InboundOwnerCmd::Show(_)),
            Command::EnterpriseCapabilities(c) => !matches!(c, EnterpriseCapabilitiesCmd::Show(_)),
            Command::Compliance(c) => !matches!(c, ComplianceCmd::Show(_)),
            Command::AbuseReport(c) => !matches!(c, AbuseReportCmd::Show { .. }),
            Command::Block(c) => !matches!(c, BlockCmd::Show { .. }),
            Command::Webhooks(c) => !matches!(c, WebhooksCmd::KeyStatus),
            Command::Retention(c) => {
                !matches!(c, RetentionCmd::List { .. } | RetentionCmd::Policies)
            }
        }
    }
}

#[derive(Subcommand)]
enum WebhooksCmd {
    /// Subscriptions per sealing key id (current / previous / unknown). Never a secret.
    KeyStatus,
    /// Re-seal every subscription secret not under the current key, in bounded batches
    /// (`FOR UPDATE SKIP LOCKED`; safe while the service runs; idempotent).
    Reseal {
        /// Rows per batch (1-1000).
        #[arg(long, default_value_t = 100)]
        batch: u64,
    },
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
    /// Minimize a principal for good (Part H §80.42, audited): DEACTIVATED for ever, its
    /// reference replaced by a non-identifying tombstone, its participations' display
    /// names erased. Calls and evidence keep the opaque id.
    Tombstone(PrincipalRef),
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
            ScopeCmd::Grant { application, scope } => {
                let a = admin.application(&application).await?;
                print(
                    &p.operator_grant_application_scope(admin.op()?, a.id, &scope)
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
                &tombstone::operator_tombstone(
                    &admin.db,
                    admin.op()?,
                    r.principal,
                    admin.clock.now(),
                )
                .await?,
            ),
        },
        Command::Limits(cmd) => match cmd {
            LimitsCmd::SetTenant { tenant, limits } => {
                let t = admin.tenant(&tenant).await?;
                print(
                    &p.operator_set_tenant_limits(admin.op()?, t.id, limits.into())
                        .await?,
                )
            }
            LimitsCmd::SetApplication {
                application,
                limits,
            } => {
                let a = admin.application(&application).await?;
                print(
                    &p.operator_set_application_limits(admin.op()?, a.id, limits.into())
                        .await?,
                )
            }
            LimitsCmd::SetServiceAccount {
                service_account,
                limits,
            } => {
                let sa = admin.service_account(&service_account).await?;
                print(
                    &p.operator_set_service_account_limits(admin.op()?, sa.id, limits.into())
                        .await?,
                )
            }
            LimitsCmd::SetPrincipal { principal, limits } => print(
                &p.operator_set_principal_limits(
                    admin.op()?,
                    PrincipalId::from_uuid(principal.principal),
                    limits.into(),
                )
                .await?,
            ),
            LimitsCmd::SetCredential { credential, limits } => {
                let id = admin.credential_id(&credential).await?;
                print(
                    &p.operator_set_credential_limits(admin.op()?, id, limits.into())
                        .await?,
                )
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
                    let now = admin.clock.now();
                    let (stored, target) = (policy.clone(), env.clone());
                    let revision = audited(&admin.db, admin.op()?, now, move |txn| {
                        Box::pin(async move {
                            let revision = store::store_policy(
                                txn,
                                target.tenant_id.as_uuid(),
                                target.application_id.as_uuid(),
                                target.id.as_uuid(),
                                &stored,
                                now,
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
                            let entry =
                                environment_entry(OperatorAction::RoutingPolicySet, &target)
                                    .detail(json!({ "revision": revision }));
                            Ok((revision, entry))
                        })
                    })
                    .await?;
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
                        .operator_set_capabilities_by(
                            admin.op()?,
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
                    let now = admin.clock.now();
                    let target = env.clone();
                    let revision = audited(&admin.db, admin.op()?, now, move |txn| {
                        Box::pin(async move {
                            let revision = spend::store_policy(
                                txn,
                                spend::SpendScope {
                                    tenant_id: target.tenant_id.as_uuid(),
                                    application_id: target.application_id.as_uuid(),
                                    environment_id: target.id.as_uuid(),
                                },
                                &policy,
                                now,
                            )
                            .await
                            .map_err(|_| {
                                PlatformError::Validation(
                                    "the spend policy was refused (limits must be positive and consistent)"
                                        .to_string(),
                                )
                            })?;
                            let entry = environment_entry(OperatorAction::SpendPolicySet, &target)
                                .detail(json!({
                                    "revision": revision,
                                    "enabled": policy.enabled,
                                    "currency": policy.currency,
                                    "legAllowanceMinor": policy.leg_allowance_minor,
                                    "maxLegSeconds": policy.max_leg_seconds,
                                }));
                            Ok((revision, entry))
                        })
                    })
                    .await?;
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
        Command::Webhooks(cmd) => {
            let settings =
                calling::webhooks::WebhookSettings::secret_box_from_lookup(&|key: &str| {
                    std::env::var(key).ok()
                })
                .map_err(|e| PlatformError::Validation(e.to_string()))?
                .ok_or_else(|| {
                    PlatformError::Validation(
                        "CALLING_WEBHOOK_SECRET_KEY (or _FILE) is not configured".to_string(),
                    )
                })?;
            let service = calling::webhooks::WebhookService::new(
                admin.db.clone(),
                Arc::new(SystemClock),
                calling::webhooks::WebhookSettings {
                    secret_box: Some(Arc::new(settings)),
                    ..Default::default()
                },
            );
            let failed = |e: calling::error::CallingError| {
                PlatformError::Validation(format!("webhook key operation failed ({e})"))
            };
            match cmd {
                WebhooksCmd::KeyStatus => print(&service.key_status().await.map_err(failed)?),
                WebhooksCmd::Reseal { batch } => {
                    if !(1..=1000).contains(&batch) {
                        return Err(PlatformError::Validation(
                            "--batch must be between 1 and 1000".to_string(),
                        ));
                    }
                    // Every batch, then ONE operator-audit row (WEBHOOK_KEYS_RESEALED:
                    // counts and batch size only).
                    let summary = service
                        .operator_reseal_all(admin.op()?, batch)
                        .await
                        .map_err(failed)?;
                    eprintln!(
                        "platform_admin: webhooks reseal resealed={} unopenable={} batches={}",
                        summary.resealed, summary.unopenable, summary.batches
                    );
                    let status = service.key_status().await.map_err(failed)?;
                    print(&json!({
                        "resealed": summary.resealed,
                        "unopenable": summary.unopenable,
                        "batches": summary.batches,
                        "keyStatus": status,
                    }))
                }
            }
        }
        Command::Retention(cmd) => match cmd {
            RetentionCmd::Hold {
                data_class,
                resource,
                tenant,
                application,
                environment,
                platform_wide,
                reason,
            } => {
                let data_class = match data_class.trim() {
                    "*" => None,
                    other => Some(DataClass::parse(other).ok_or_else(|| {
                        PlatformError::Validation(format!(
                            "--data-class must be `*` or one of {}",
                            DataClass::ALL
                                .iter()
                                .map(|c| c.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?),
                };
                let owner = match (platform_wide, tenant, application, environment) {
                    (true, None, None, None) => None,
                    (false, Some(tenant), Some(application), Some(environment)) => {
                        let env = admin
                            .environment(&EnvironmentPath {
                                application: ApplicationPath {
                                    tenant: TenantPath { tenant },
                                    application,
                                },
                                environment,
                            })
                            .await?;
                        Some(HoldOwner {
                            tenant_id: env.tenant_id.as_uuid(),
                            application_id: env.application_id.as_uuid(),
                            environment_id: env.id.as_uuid(),
                        })
                    }
                    _ => {
                        return Err(PlatformError::Validation(
                            "a hold names --tenant, --application and --environment, or is --platform-wide"
                                .to_string(),
                        ))
                    }
                };
                print(
                    &holds::operator_place(
                        &admin.db,
                        admin.op()?,
                        HoldScope {
                            data_class,
                            resource_id: resource,
                            owner,
                        },
                        reason.into(),
                        admin.clock.now(),
                    )
                    .await?,
                )
            }
            RetentionCmd::Release { hold } => print(
                &holds::operator_release(&admin.db, admin.op()?, hold, admin.clock.now()).await?,
            ),
            RetentionCmd::List { all } => print(&holds::list(&admin.db, !all).await?),
            RetentionCmd::Policies => {
                let settings = RetentionSettings::from_env()
                    .map_err(|e| PlatformError::Validation(e.to_string()))?;
                print(&json!({
                    "policies": retention::policies(&settings)
                        .iter()
                        .map(|p| json!({
                            "dataClass": p.class,
                            "action": p.action,
                            "days": p.days(),
                            "configuredBy": p.configured_by,
                            "description": p.description,
                        }))
                        .collect::<Vec<_>>(),
                    "intervalSeconds": settings.interval.as_secs(),
                }))
            }
        },
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
                    let now = admin.clock.now();
                    let target = env.clone();
                    let revision = audited(&admin.db, admin.op()?, now, move |txn| {
                        Box::pin(async move {
                    let row = txn
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
                                target.id.as_uuid().into(),
                                (!disabled).into(),
                                now.into(),
                            ],
                        ))
                        .await
                        .map_err(db_error)?
                        .ok_or(PlatformError::NotFound("credential in this environment"))?;
                    let revision: i32 = row.try_get_by_index(0).map_err(db_error)?;
                    let entry = environment_entry(OperatorAction::InboundOwnerSet, &target)
                        .detail(json!({
                            "credentialId": credential_id,
                            "enabled": !disabled,
                            "revision": revision,
                        }));
                    Ok((revision, entry))
                        })
                    })
                    .await?;
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
        Command::Compliance(cmd) => {
            use calling::routing::compliance;
            match cmd {
                ComplianceCmd::Set {
                    environment,
                    status,
                    countries,
                    expires_at,
                    evidence_ref,
                } => {
                    let env = admin.environment(&environment).await?;
                    let expires_at = match expires_at {
                        None => None,
                        Some(text) => Some(
                            chrono::DateTime::parse_from_rfc3339(text.trim())
                                .map_err(|_| {
                                    PlatformError::Validation(
                                        "--expires-at must be an RFC 3339 timestamp".to_string(),
                                    )
                                })?
                                .with_timezone(&chrono::Utc),
                        ),
                    };
                    let countries: Vec<String> = countries
                        .iter()
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect();
                    let now = admin.clock.now();
                    let record = compliance::operator_set(
                        &admin.db,
                        admin.op()?,
                        &env,
                        status.into(),
                        countries,
                        evidence_ref.map(|e| e.trim().to_string()),
                        expires_at,
                        now,
                    )
                    .await?;
                    print(&compliance::safe_view(&record, now))
                }
                ComplianceCmd::Revoke(environment) => {
                    let env = admin.environment(&environment).await?;
                    let now = admin.clock.now();
                    let record =
                        compliance::operator_revoke(&admin.db, admin.op()?, &env, now).await?;
                    print(&compliance::safe_view(&record, now))
                }
                ComplianceCmd::Show(environment) => {
                    let env = admin.environment(&environment).await?;
                    let record = compliance::load_eligibility(&admin.db, env.id.as_uuid())
                        .await
                        .map_err(|_| PlatformError::Database {
                            summary: "compliance eligibility".to_string(),
                            connectivity: false,
                        })?;
                    let now = admin.clock.now();
                    print(&json!({
                        "environmentId": env.id,
                        "eligibility": record.map(|r| compliance::safe_view(&r, now)),
                    }))
                }
            }
        }
        Command::AbuseReport(cmd) => {
            use calling::calls::app::abuse_reports::{self, AbuseReportStatus};
            match cmd {
                AbuseReportCmd::SetStatus { report, status } => {
                    let status = match status {
                        ReportStatusArg::Open => AbuseReportStatus::Open,
                        ReportStatusArg::Reviewed => AbuseReportStatus::Reviewed,
                        ReportStatusArg::Dismissed => AbuseReportStatus::Dismissed,
                    };
                    print(
                        &abuse_reports::operator_set_status(
                            &admin.db,
                            admin.op()?,
                            report,
                            status,
                            admin.clock.now(),
                        )
                        .await?,
                    )
                }
                AbuseReportCmd::Show { report } => print(
                    &abuse_reports::report(&admin.db, report)
                        .await
                        .map_err(|_| PlatformError::Database {
                            summary: "abuse report".to_string(),
                            connectivity: false,
                        })?
                        .ok_or(PlatformError::NotFound("abuse report"))?,
                ),
            }
        }
        Command::Block(cmd) => {
            use calling::calls::app::blocks;
            match cmd {
                BlockCmd::Release { block } => print(
                    &blocks::operator_release(&admin.db, admin.op()?, block, admin.clock.now())
                        .await?,
                ),
                BlockCmd::Show { block } => print(
                    &blocks::rule(&admin.db, block)
                        .await
                        .map_err(|_| PlatformError::Database {
                            summary: "block rule".to_string(),
                            connectivity: false,
                        })?
                        .ok_or(PlatformError::NotFound("block rule"))?,
                ),
            }
        }
    }
}

/// The audit entry of an environment-level administration change.
fn environment_entry(action: OperatorAction, env: &EnvironmentView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Environment, env.id.as_uuid()).scope(
        Some(env.tenant_id.as_uuid()),
        Some(env.application_id.as_uuid()),
        Some(env.id.as_uuid()),
    )
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // The operator is checked before anything else: a mutating command without one is
    // refused before the database is even reached (nothing can change).
    let operator = match cli.operator.as_deref().map(OperatorContext::new) {
        Some(Ok(op)) => Some(op),
        Some(Err(err)) => {
            eprintln!("platform_admin: {err}");
            return ExitCode::from(2);
        }
        None => None,
    };
    if operator.is_none() && cli.command.is_mutating() {
        eprintln!(
            "platform_admin: this command changes state: --operator <id> (or CF_OPERATOR_ID) is required"
        );
        return ExitCode::from(2);
    }
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
        operator,
    };
    if let Some(op) = &admin.operator {
        eprintln!(
            "platform_admin: operator {} correlation {}",
            op.operator(),
            op.correlation_id()
        );
    }
    match run(&admin, cli.command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("platform_admin: {err}");
            ExitCode::FAILURE
        }
    }
}

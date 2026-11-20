//! Contract vocabulary: every enum the HQ interface v1 fixes, and one [`Capability`]
//! per bridge route. The catalog (`catalog.rs`) is the single source of truth; the
//! runtime route table and every file in `docs/contract/` are derived from it.

use serde::Serialize;

/// Bridge base path. Every contract path is relative to it.
pub const BRIDGE_BASE: &str = "/internal/cfmeet-admin/v1";
/// Envelope `contract_version` (major.minor of the wire envelope; independent of the
/// artifact version below).
pub const ENVELOPE_VERSION: &str = "1.0";
/// Contract artifact version (`openapi.info.version`). Minor: additive (new capability,
/// state change, a schema defined for the first time). Major: anything breaking.
pub const CONTRACT_VERSION: &str = "1.12.0";
pub const CONTRACT_NAME: &str = "cfmeet-admin-bridge";
pub const PRODUCT: &str = "cfmeet";
/// The only HQ principal the bridge accepts (HMAC `X-Service-Name`, client certificate).
pub const HQ_PRINCIPAL: &str = "cosmicforge-hq-cfmeet-admin";
/// Owning CF Meet service of the bridge's own routes.
pub const BRIDGE_OWNER: &str = "admin-bridge";

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($wire => Some($name::$variant),)+ _ => None }
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

wire_enum!(
    /// First segment of every capability id: the HQ module, or a platform namespace.
    Module {
        Accounts => "accounts", Meetings => "meetings", Messaging => "messaging",
        Calling => "calling", Communications => "communications",
        Organizations => "organizations", Partners => "partners", Identity => "identity",
        Apps => "apps", Verification => "verification", Pricing => "pricing",
        Finance => "finance", Notifications => "notifications", Privacy => "privacy",
        Operations => "operations", Audit => "audit", Telecom => "telecom",
        Bridge => "bridge", Hq => "hq",
    }
);

impl Module {
    /// Runtime switch of the module (`ADMIN_BRIDGE_MODULE_<NAME>`); platform namespaces
    /// have none beyond `ADMIN_BRIDGE_ENABLED`.
    pub fn flag(self) -> Option<String> {
        match self {
            Module::Bridge | Module::Hq => None,
            m => Some(format!(
                "ADMIN_BRIDGE_MODULE_{}",
                m.as_str().to_ascii_uppercase()
            )),
        }
    }
}

wire_enum!(Kind { Read => "READ", Command => "COMMAND", Sync => "SYNC" });

wire_enum!(Method { Get => "GET", Post => "POST" });

wire_enum!(
    /// Artifact state. Never runtime authorization by itself.
    State {
        ReadOnly => "READ_ONLY", Available => "AVAILABLE", Disabled => "DISABLED",
        BlockedDependency => "BLOCKED_DEPENDENCY",
    }
);

wire_enum!(Risk { R0 => "R0", R1 => "R1", R2 => "R2", R3 => "R3", R4 => "R4" });

wire_enum!(
    Privacy {
        Public => "PUBLIC", Internal => "INTERNAL",
        OperationsMinimized => "OPERATIONS_MINIMIZED", Personal => "PERSONAL",
        FinancialSensitive => "FINANCIAL_SENSITIVE", SafetyEvidence => "SAFETY_EVIDENCE",
    }
);

wire_enum!(Approval { None => "NONE", TwoPerson => "TWO_PERSON" });

wire_enum!(
    /// The exact, ordered query parameter lists (HQ signs the exact URI).
    QueryShape {
        Scoped => "SCOPED", ScopedPage => "SCOPED_PAGE", ScopedWindow => "SCOPED_WINDOW",
        ScopedSearch => "SCOPED_SEARCH",
    }
);

impl QueryShape {
    pub const fn params(self) -> &'static [&'static str] {
        const S: &[&str] = &["market", "organization", "zone", "tenant", "purpose"];
        match self {
            QueryShape::Scoped => S,
            QueryShape::ScopedPage => &[
                "market",
                "organization",
                "zone",
                "tenant",
                "purpose",
                "limit",
                "cursor",
            ],
            QueryShape::ScopedWindow => &[
                "market",
                "organization",
                "zone",
                "tenant",
                "purpose",
                "limit",
                "cursor",
                "start",
                "end",
            ],
            QueryShape::ScopedSearch => &[
                "market",
                "organization",
                "zone",
                "tenant",
                "purpose",
                "limit",
                "cursor",
                "q",
            ],
        }
    }
    /// Parameters every request of the shape must carry.
    pub const fn required(self) -> &'static [&'static str] {
        match self {
            QueryShape::ScopedWindow => &[
                "market",
                "organization",
                "zone",
                "tenant",
                "purpose",
                "start",
                "end",
            ],
            QueryShape::ScopedSearch => {
                &["market", "organization", "zone", "tenant", "purpose", "q"]
            }
            _ => &["market", "organization", "zone", "tenant", "purpose"],
        }
    }
}

wire_enum!(
    /// Fixed purpose list (`purpose` query parameter).
    Purpose {
        SupportReview => "support-review", VerificationReview => "verification-review",
        AbuseReview => "abuse-review", BillingReview => "billing-review",
        PricingChange => "pricing-change", OpsIncident => "ops-incident",
        ReleaseRequest => "release-request", PrivacyRequest => "privacy-request",
        AdminUiCapabilities => "admin-ui-capabilities", MetricsRead => "metrics-read",
        EventSync => "event-sync",
    }
);

wire_enum!(
    /// Bridge roles. Identical to HQ's cfmeet product-grant roles (identity role map),
    /// plus the HQ worker's feed identity.
    Role {
        Viewer => "cfmeet.viewer", Support => "cfmeet.support",
        TrustSafety => "cfmeet.trust_safety",
        VerificationReviewer => "cfmeet.verification.reviewer",
        PartnerOps => "cfmeet.partner_ops", Finance => "cfmeet.finance",
        PricingEditor => "cfmeet.pricing.editor", PricingApprover => "cfmeet.pricing.approver",
        PlatformOps => "cfmeet.platform_ops", SecurityAdmin => "cfmeet.security_admin",
        ReleaseManager => "cfmeet.release_manager", SourceSync => "cfmeet.source_sync",
    }
);

impl Role {
    /// Purposes a role may ever use, when narrower than its capabilities allow. The HQ
    /// worker's feed identity syncs events and pulls datasets, nothing else.
    pub fn purpose_limit(self) -> Option<&'static [Purpose]> {
        match self {
            Role::SourceSync => Some(&[Purpose::EventSync, Purpose::MetricsRead]),
            _ => None,
        }
    }

    pub fn allows_purpose(self, purpose: Purpose) -> bool {
        self.purpose_limit().is_none_or(|l| l.contains(&purpose))
    }
}

wire_enum!(
    /// Owning CF Meet service of a capability.
    Owner {
        AdminBridge => "admin-bridge", MeetingService => "meeting-service",
        Messaging => "messaging", Calling => "calling", Communications => "communications",
        Organizations => "organizations", DeveloperPlatform => "developer-platform",
        PlatformAccess => "platform-access", Notifications => "notifications",
        Numbering => "numbering",
    }
);

wire_enum!(
    /// Where the owner side stands today (design §6 status column).
    OwnerStatus {
        Local => "LOCAL", Exists => "EXISTS", ExistsNotGranted => "EXISTS_NOT_GRANTED",
        New => "NEW", CliToApi => "CLI_TO_API", DbToApi => "DB_TO_API",
        Future => "FUTURE", TelecomOff => "TELECOM_OFF",
    }
);

wire_enum!(
    Phase {
        A1 => "A1", A2 => "A2", A3 => "A3", A4 => "A4", A5 => "A5", A6 => "A6",
        A7 => "A7", A8 => "A8", A9 => "A9", Future => "FUTURE",
    }
);

/// How a path parameter value is validated. No value may contain `/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamFormat {
    Uuid,
    /// `OPAQUE_ID_PATTERN`.
    Opaque,
    /// `DATASET_ID_PATTERN`.
    DatasetId,
}

pub const OPAQUE_ID_PATTERN: &str = "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$";
pub const DATASET_ID_PATTERN: &str = "^cfmeet\\.[a-z_]{1,48}$";
pub const SCOPE_VALUE_PATTERN: &str = "^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$";
pub const CURSOR_PATTERN: &str = "^[A-Za-z0-9._~:@+=-]{1,512}$";
pub const SEARCH_PATTERN: &str = "^[\\p{L}\\p{N} ._@:-]{1,128}$";
pub const MAX_LIMIT: u32 = 100;
pub const MAX_WINDOW_DAYS: u32 = 31;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathParam {
    pub name: &'static str,
    pub format: ParamFormat,
}

/// One bridge route. Constructed only in `catalog.rs`.
#[derive(Debug, Clone)]
pub struct Capability {
    pub id: &'static str,
    pub version: &'static str,
    pub kind: Kind,
    pub method: Method,
    /// Path relative to [`BRIDGE_BASE`], with `{name}` parameters.
    pub path: &'static str,
    pub path_params: Vec<PathParam>,
    pub shape: QueryShape,
    pub owner: Owner,
    pub owner_status: OwnerStatus,
    /// Owner endpoint the bridge calls, once wired (`METHOD /path`).
    pub source_endpoint: Option<&'static str>,
    pub permission: String,
    pub risk: Risk,
    pub privacy: Privacy,
    pub roles: Vec<Role>,
    pub purposes: Vec<Purpose>,
    /// Request body schema (component name), commands only.
    pub body: Option<&'static str>,
    /// Result schema (component name). `None`: not defined yet, no 2xx is possible.
    pub result: Option<&'static str>,
    pub phase: Phase,
    /// True once the bridge serves this route in this commit.
    pub served: bool,
    /// True when `q` may carry an email address or phone number (published as
    /// `x-contact-search`; HQ sends those searches through its POST search path).
    pub contact_search: bool,
    pub tests: Vec<&'static str>,
}

impl Capability {
    pub fn module(&self) -> Module {
        let first = self.id.split('.').next().unwrap_or_default();
        Module::parse(first).expect("capability id must start with a module")
    }

    pub fn approval(&self) -> Approval {
        if self.kind == Kind::Command && self.risk >= Risk::R3 {
            Approval::TwoPerson
        } else {
            Approval::None
        }
    }

    pub fn source_version_required(&self) -> bool {
        self.kind == Kind::Command && self.risk >= Risk::R3
    }

    /// HQ never retries; every command is idempotent on `Idempotency-Key`.
    pub fn idempotency_required(&self) -> bool {
        self.kind == Kind::Command
    }

    pub fn mutating(&self) -> bool {
        self.kind != Kind::Read
    }

    pub fn state(&self) -> State {
        match (self.served, self.kind) {
            (true, Kind::Command) => State::Available,
            (true, _) => State::ReadOnly,
            (false, Kind::Command) => State::Disabled,
            (false, _) => State::BlockedDependency,
        }
    }

    pub fn feature_flag(&self) -> String {
        let mut flags = vec!["ADMIN_BRIDGE_ENABLED".to_string()];
        if let Some(f) = self.module().flag() {
            flags.push(f);
        }
        if self.kind == Kind::Command {
            flags.push("ADMIN_BRIDGE_WRITES_ENABLED".to_string());
        }
        flags.join("+")
    }

    pub fn blocker(&self) -> Option<String> {
        if self.served {
            return None;
        }
        let owner = self.owner.as_str();
        let why = match self.owner_status {
            OwnerStatus::Local => "bridge route not implemented yet".to_string(),
            OwnerStatus::Exists => format!("{owner} endpoint exists; bridge wiring pending"),
            OwnerStatus::ExistsNotGranted => {
                format!("{owner} endpoint exists; grant to cfmeet.admin-bridge and operator attribution pending")
            }
            OwnerStatus::New => format!("new {owner} operator endpoint required"),
            OwnerStatus::CliToApi => {
                format!("{owner} has a server command-line tool only; operator API required")
            }
            OwnerStatus::DbToApi => {
                format!("{owner} keeps this in its database only; operator read API required")
            }
            OwnerStatus::Future => "future capability; no owner decision yet".to_string(),
            OwnerStatus::TelecomOff => "telecom is switched off".to_string(),
        };
        let gate = if self.kind == Kind::Command && self.risk >= Risk::R3 {
            "; two-person certification required before enabling"
        } else {
            ""
        };
        Some(format!("Phase {}: {why}{gate}", self.phase.as_str()))
    }

    pub fn scope_rule(&self) -> &'static str {
        match self.module() {
            Module::Bridge | Module::Hq => {
                "cfmeet + exact market, organization, zone and tenant inside the staff assertion scopes; bridge-local, no owner call"
            }
            _ => {
                "cfmeet + exact market, organization, zone and tenant inside the staff assertion scopes; the owner re-checks the tenant on every call"
            }
        }
    }

    pub fn full_path(&self) -> String {
        format!("{BRIDGE_BASE}{}", self.path)
    }
}

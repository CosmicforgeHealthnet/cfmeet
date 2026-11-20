//! Owner commands as data (A4+). Each table row is one served command: the owner route,
//! the body fields HQ sends (snake_case) and the owner receives (camelCase), and the closed
//! resource the owner answers with. The request and result schemas, the strict translation
//! and the catalog entry are all derived from the row (`routes::owner_commands`,
//! `schemas_commands`), so they cannot drift apart.
//!
//! Every owner follows the same contract (design doc §17.6): `Idempotency-Key` (= body
//! `operation_id`), `expectedVersion` when the resource is versioned, HQ's approver for
//! R3/R4, the change recorded with the key in the same transaction, and the answer
//! `{"status", "version" (string|null), "resource"}`.

use super::model::Owner;

/// A body field's type (HQ side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum In {
    Enum(&'static [&'static str]),
    Bool,
    Int(i64, i64),
    Uuid,
    /// A UUID or null.
    NullableUuid,
    /// An UPPER_SNAKE code (1–64).
    Code,
    /// A string of `min..=max` characters without control characters.
    Str(usize, usize),
    /// A string as [`In::Str`] or null.
    NullableStr(usize, usize),
    /// Distinct UUIDs, at most this many.
    UuidList(usize),
    /// A JSON object the owner validates (e.g. a partial policy document).
    Object,
    /// Distinct values of the list, at most this many.
    EnumList(&'static [&'static str], usize),
    /// An RFC 3339 UTC time (`...Z`).
    Time,
    /// A calendar day `YYYY-MM-DD`.
    Date,
    /// A JSON object the owner validates, or null (e.g. "remove the override").
    NullableObject,
}

/// Where a body field goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The owner body.
    Body,
    /// The owner path placeholder of the same HQ name only (and the ledger target).
    Path,
    /// The owner body and the ledger target (the resource the version belongs to).
    BodyAndTarget,
    /// Never sent by HQ: the owner body always carries this value.
    Fixed(&'static str),
    /// Never sent by HQ: the owner body carries the request's `tenant` scope (a Platform
    /// Access tenant id, lowercase UUID; any other tenant names nothing). It also names
    /// the resource in the ledger target.
    Tenant,
}

/// What becomes of the command core's optional `reason_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Optional; forwarded as `reasonCode` when present.
    Forward,
    /// Required; forwarded as `reasonCode` (the owner requires a reason).
    Required,
    /// Optional; kept in the bridge's record only (the owner's body has no reason field).
    Ledger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyField {
    pub hq: &'static str,
    pub owner: &'static str,
    pub kind: In,
    pub required: bool,
    pub place: Place,
}

/// A resource field's type (owner answer, re-extracted strictly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    /// An identifier-like string (`OPAQUE` characters, ≤ 128).
    Str,
    /// An UPPER_SNAKE code.
    Code,
    Uuid,
    Int,
    Bool,
    Time,
    /// Any string up to 500 characters without control characters (an owner reason).
    Text,
    /// Identifier-like strings ([`Out::Str`]), at most 64.
    StrList,
    Obj(&'static [OutField]),
    /// Objects of these fields, at most 100.
    ObjList(&'static [OutField]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutField {
    pub hq: &'static str,
    pub owner: &'static str,
    pub kind: Out,
    pub nullable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerCommand {
    pub id: &'static str,
    pub owner: Owner,
    /// `POST /internal/v1/...`; `{name}` is the bridge path parameter of that name.
    pub source: &'static str,
    /// `Some(min)`: the resource has a row version; `expected_version` (integer ≥ min; 0
    /// means "none yet" where the owner allows it) is required and forwarded as
    /// `expectedVersion`. `None`: unversioned, every command is its own ledger slot.
    pub version: Option<i64>,
    /// The command core's `reason_code`.
    pub reason: Reason,
    pub body: &'static [BodyField],
    /// The owner's possible `status` values (empty: any UPPER_SNAKE code).
    pub statuses: &'static [&'static str],
    pub resource: &'static [OutField],
    /// Component names (request body, result).
    pub body_schema: &'static str,
    pub result_schema: &'static str,
}

impl OwnerCommand {
    pub fn owner_path(&self) -> &'static str {
        self.source.strip_prefix("POST ").unwrap_or(self.source)
    }
}

const fn b(hq: &'static str, owner: &'static str, kind: In, required: bool) -> BodyField {
    BodyField {
        hq,
        owner,
        kind,
        required,
        place: Place::Body,
    }
}

/// A body field that fills the owner path placeholder `{hq}` (always required).
const fn p(hq: &'static str, kind: In) -> BodyField {
    BodyField {
        hq,
        owner: hq,
        kind,
        required: true,
        place: Place::Path,
    }
}

/// A value the bridge always sends to the owner (HQ cannot send this field).
const fn fixed(owner: &'static str, value: &'static str) -> BodyField {
    BodyField {
        hq: owner,
        owner,
        kind: In::Code,
        required: false,
        place: Place::Fixed(value),
    }
}

/// A body field that also names the resource in the ledger target (always required).
const fn bt(hq: &'static str, owner: &'static str, kind: In) -> BodyField {
    BodyField {
        hq,
        owner,
        kind,
        required: true,
        place: Place::BodyAndTarget,
    }
}

const fn o(hq: &'static str, owner: &'static str, kind: Out, nullable: bool) -> OutField {
    OutField {
        hq,
        owner,
        kind,
        nullable,
    }
}

// ---- Communications (A4 / A5) ------------------------------------------------------

const COMM_HOLD: &[OutField] = &[
    o("reason", "reason", Out::Code, false),
    o("active", "active", Out::Bool, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const COMM_PROFILE: &[OutField] = &[
    o("cfmeet_id", "cfMeetId", Out::Str, false),
    o("activation_state", "activationState", Out::Code, false),
    o("held", "held", Out::Bool, false),
    o("hold", "hold", Out::Obj(COMM_HOLD), true),
    o("version", "version", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const COMM_REPORT: &[OutField] = &[
    o("report_id", "reportId", Out::Uuid, false),
    o("target_cfmeet_id", "targetCfMeetId", Out::Str, true),
    o("decision", "decision", Out::Code, true),
    o("reason", "reason", Out::Code, true),
    o("held", "held", Out::Bool, false),
    o("reviewed_at", "reviewedAt", Out::Time, true),
    o("version", "version", Out::Int, false),
];

// ---- Notifications (A5) ------------------------------------------------------------

const NOTIF_DELIVERY: &[OutField] = &[
    o("delivery_id", "deliveryId", Out::Uuid, false),
    o("notification_id", "notificationId", Out::Uuid, false),
    o("channel", "channel", Out::Code, false),
    o("lane", "lane", Out::Code, false),
    o("delivery_state", "deliveryStatus", Out::Code, false),
    o("attempts", "attempts", Out::Int, false),
    o("last_error_class", "lastErrorClass", Out::Code, true),
    o("next_attempt_at", "nextAttemptAt", Out::Time, true),
];
const NOTIF_ROUTING: &[OutField] = &[
    o("category", "category", Out::Code, false),
    o("channel", "channel", Out::Code, false),
    o("mode", "mode", Out::Code, false),
    o("version", "version", Out::Int, false),
    o("canary_percent", "canaryPercent", Out::Int, false),
    o("fence_at", "fenceAt", Out::Time, false),
    o("changed_at", "changedAt", Out::Time, false),
];
const NOTIF_DECISION: &[OutField] = &[
    o("decision_id", "decisionId", Out::Uuid, false),
    o("category", "category", Out::Code, false),
    o("event_state", "eventState", Out::Code, false),
    o("reject_reason", "rejectReason", Out::Str, true),
    o("processed_at", "processedAt", Out::Time, true),
];

// ---- Organizations (A4 / A5) -------------------------------------------------------

const ORG: &[OutField] = &[
    o("organization_id", "organizationId", Out::Uuid, false),
    o("lifecycle_state", "lifecycleStatus", Out::Code, false),
    o("lifecycle_reason", "lifecycleReason", Out::Text, true),
    o("provisioning_state", "provisioningState", Out::Code, false),
    o("version", "version", Out::Int, false),
    o("created_at", "createdAt", Out::Time, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const ORG_PROVISIONING: &[OutField] = &[
    o("organization_id", "organizationId", Out::Uuid, false),
    o("provisioning_state", "provisioningState", Out::Code, false),
    o("lifecycle_state", "lifecycleStatus", Out::Code, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const ORG_BINDING: &[OutField] = &[
    o("organization_id", "organizationId", Out::Uuid, false),
    o("service", "service", Out::Str, false),
    o("capability", "capability", Out::Str, false),
    o("source", "source", Out::Code, true),
    o("version", "version", Out::Int, true),
    o("verified_at", "verifiedAt", Out::Time, true),
    o("updated_at", "updatedAt", Out::Time, true),
];
const ORG_MEMBER: &[OutField] = &[
    o("organization_id", "organizationId", Out::Uuid, false),
    o("membership_id", "membershipId", Out::Uuid, true),
    o("user_id", "userId", Out::Uuid, false),
    o("role", "role", Out::Code, true),
    o("membership_state", "status", Out::Code, true),
    o("version", "version", Out::Int, true),
    o("updated_at", "updatedAt", Out::Time, true),
];
const ORG_TEAM: &[OutField] = &[
    o("organization_id", "organizationId", Out::Uuid, false),
    o("team_id", "teamId", Out::Uuid, false),
    o("team_state", "status", Out::Code, false),
    o("version", "version", Out::Int, false),
    o("parent_team_id", "parentTeamId", Out::Uuid, true),
    o("member_count", "memberCount", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const ORG_POLICIES: &[OutField] = &[
    o("organization_id", "organizationId", Out::Uuid, false),
    o("revision", "revision", Out::Int, false),
    o("changed_keys", "changedKeys", Out::StrList, false),
    o("updated_at", "updatedAt", Out::Time, true),
];
const ORG_REPLAY: &[OutField] = &[
    o("destination", "destination", Out::Str, true),
    o(
        "requested_event_count",
        "requestedEventCount",
        Out::Int,
        false,
    ),
    o("replayed_count", "replayedCount", Out::Int, true),
];
const ROLES: &[&str] = &["OWNER", "ADMIN", "MANAGER", "MEMBER", "GUEST"];

const fn org_lifecycle(
    id: &'static str,
    source: &'static str,
    statuses: &'static [&'static str],
    body_schema: &'static str,
) -> OwnerCommand {
    OwnerCommand {
        id,
        owner: Owner::Organizations,
        source,
        version: Some(1),
        reason: Reason::Forward,
        body: &[],
        statuses,
        resource: ORG,
        body_schema,
        result_schema: "OrganizationCommandResult",
    }
}

// ---- Platform Access (A4): identity and partner commands -----------------------------

const PA_STATUS: &[OutField] = &[
    o("id", "id", Out::Uuid, false),
    o("tenant_id", "tenantId", Out::Uuid, true),
    o("application_id", "applicationId", Out::Uuid, true),
    o("environment_id", "environmentId", Out::Uuid, true),
    o("classification", "classification", Out::Code, true),
    o("kind", "kind", Out::Code, true),
    o("resource_state", "status", Out::Code, false),
    o("revoked_at", "revokedAt", Out::Time, true),
    o("revision", "revision", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const PA_CREDENTIAL: &[OutField] = &[
    o("id", "id", Out::Uuid, false),
    o("public_id", "publicId", Out::Str, false),
    o("tenant_id", "tenantId", Out::Uuid, false),
    o("application_id", "applicationId", Out::Uuid, true),
    o("environment_id", "environmentId", Out::Uuid, true),
    o("service_account_id", "serviceAccountId", Out::Uuid, true),
    o("credential_state", "status", Out::Code, false),
    o("suspended_at", "suspendedAt", Out::Time, true),
    o("suspension_reason", "suspensionReason", Out::Code, true),
    o("revoked_at", "revokedAt", Out::Time, true),
    o("revision", "revision", Out::Int, false),
    o("version", "version", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const PA_PRINCIPAL: &[OutField] = &[
    o("id", "id", Out::Uuid, false),
    o("tenant_id", "tenantId", Out::Uuid, false),
    o("application_id", "applicationId", Out::Uuid, true),
    o("environment_id", "environmentId", Out::Uuid, true),
    o("kind", "kind", Out::Code, false),
    o("principal_state", "status", Out::Code, false),
    o("tombstoned_at", "tombstonedAt", Out::Time, true),
    o("revision", "revision", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const PA_APP_GRANT: &[OutField] = &[
    o("id", "id", Out::Uuid, false),
    o("app_id", "appId", Out::Uuid, false),
    o("target", "target", Out::Str, true),
    o("tenant_id", "tenantId", Out::Uuid, true),
    o("environment_id", "environmentId", Out::Uuid, true),
    o("principal_id", "principalId", Out::Uuid, true),
    o("grant_state", "status", Out::Code, false),
    o("revoked_at", "revokedAt", Out::Time, true),
    o("revoke_reason", "revokeReason", Out::Code, true),
    o("revision", "revision", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, false),
];
const PA_ORIGINS: &[OutField] = &[
    o("environment_id", "environmentId", Out::Uuid, false),
    o("tenant_id", "tenantId", Out::Uuid, false),
    o("application_id", "applicationId", Out::Uuid, true),
    o("environment_kind", "environmentKind", Out::Code, false),
    o("origin_count", "originCount", Out::Int, false),
    o("revision", "revision", Out::Int, false),
    o("updated_at", "updatedAt", Out::Time, true),
];
const PA_SCOPE: &[OutField] = &[
    o("application_id", "applicationId", Out::Uuid, false),
    o("tenant_id", "tenantId", Out::Uuid, false),
    o("scope", "scope", Out::Str, false),
    o("granted", "granted", Out::Bool, false),
    o(
        "allowed_environment_kinds",
        "allowedEnvironmentKinds",
        Out::StrList,
        true,
    ),
    o("grant_basis", "grantBasis", Out::Code, true),
    o("granted_at", "grantedAt", Out::Time, true),
];
const PA_SIGNED_OUT: &[OutField] = &[
    o("principal_id", "principalId", Out::Uuid, false),
    o("tenant_id", "tenantId", Out::Uuid, false),
    o("application_id", "applicationId", Out::Uuid, true),
    o("environment_id", "environmentId", Out::Uuid, true),
    o("kind", "kind", Out::Code, false),
    o("tokens_not_before", "tokensNotBefore", Out::Time, true),
];
const SCOPE_BODY: &[BodyField] = &[
    p("application_id", In::Uuid),
    b("action", "action", In::Enum(&["GRANT", "REVOKE"]), true),
    b("scope", "scope", In::Str(1, 128), true),
    b(
        "environment_kinds",
        "environmentKinds",
        In::EnumList(&["SANDBOX", "TEST", "PRODUCTION"], 3),
        false,
    ),
];

#[allow(clippy::too_many_arguments)]
const fn pa(
    id: &'static str,
    source: &'static str,
    version: Option<i64>,
    body: &'static [BodyField],
    statuses: &'static [&'static str],
    resource: &'static [OutField],
    body_schema: &'static str,
    result_schema: &'static str,
) -> OwnerCommand {
    OwnerCommand {
        id,
        owner: Owner::PlatformAccess,
        source,
        version,
        reason: Reason::Forward,
        body,
        statuses,
        resource,
        body_schema,
        result_schema,
    }
}

// ---- Developer Platform (A4): partner and app commands -----------------------------

const DP_WORKSPACE: &[OutField] = &[
    o("workspace_id", "organizationId", Out::Uuid, false),
    o("state", "state", Out::Code, false),
    o("state_reason", "stateReason", Out::Code, true),
    o("updated_at", "updatedAt", Out::Time, false),
    o("closed_at", "closedAt", Out::Time, true),
];
const DP_INCIDENT: &[OutField] = &[
    o("reference", "reference", Out::Text, false),
    o("occurred_at", "occurredAt", Out::Time, true),
];
const DP_SUPPORT: &[OutField] = &[
    o("workspace_id", "organizationId", Out::Uuid, false),
    o("support_tier", "supportTier", Out::Code, false),
    o("last_incident", "lastIncident", Out::Obj(DP_INCIDENT), true),
    o("updated_at", "updatedAt", Out::Time, true),
];
const DP_PUSH_KEY: &[OutField] = &[
    o("push_key_id", "pushKeyId", Out::Uuid, false),
    o("workspace_id", "organizationId", Out::Uuid, false),
    o("project_id", "projectId", Out::Uuid, false),
    o("environment_id", "environmentId", Out::Uuid, false),
    o("provider", "provider", Out::Code, false),
    o("state", "state", Out::Code, false),
    o("disabled_at", "disabledAt", Out::Time, true),
    o("disabled_reason", "disabledReason", Out::Code, true),
    o("updated_at", "updatedAt", Out::Time, false),
];
const DP_DELIVERY: &[OutField] = &[
    o("delivery_id", "deliveryId", Out::Uuid, false),
    o("retry_of", "retryOf", Out::Uuid, true),
    o("subscription_id", "subscriptionId", Out::Uuid, false),
    o("environment_id", "environmentId", Out::Uuid, false),
    o("state", "state", Out::Code, false),
    o("created_at", "createdAt", Out::Time, false),
];
const DP_REPROCESS: &[OutField] = &[
    o("environment_id", "environmentId", Out::Uuid, true),
    o("from", "from", Out::Str, false),
    o("to", "to", Out::Str, false),
    o("removed", "removed", Out::Int, true),
    o("rebuilt", "rebuilt", Out::Int, true),
];
const DP_APP: &[OutField] = &[
    o("app_id", "appId", Out::Uuid, false),
    o("workspace_id", "organizationId", Out::Uuid, false),
    o("state", "state", Out::Code, false),
    o("published_version", "publishedVersion", Out::Int, true),
    o("updated_at", "updatedAt", Out::Time, false),
];
const WORKSPACE_STATES: &[&str] = &["INVITED", "ACTIVE", "RESTRICTED", "SUSPENDED", "CLOSED"];
const SUPPORT_TIERS: &[&str] = &["COMMUNITY", "STANDARD", "PREMIUM", "ENTERPRISE", "INTERNAL"];

#[allow(clippy::too_many_arguments)]
const fn dp(
    id: &'static str,
    source: &'static str,
    version: Option<i64>,
    reason: Reason,
    body: &'static [BodyField],
    statuses: &'static [&'static str],
    resource: &'static [OutField],
    body_schema: &'static str,
    result_schema: &'static str,
) -> OwnerCommand {
    OwnerCommand {
        id,
        owner: Owner::DeveloperPlatform,
        source,
        version,
        reason,
        body,
        statuses,
        resource,
        body_schema,
        result_schema,
    }
}

// ---- Developer Platform (A6): pricing and plans --------------------------------------

const PRICE_VERSION: &[OutField] = &[
    o("version_id", "versionId", Out::Uuid, false),
    o("price_book_id", "priceBookId", Out::Uuid, false),
    o("seq", "seq", Out::Int, false),
    o("version_state", "state", Out::Code, false),
    o("rates_digest", "ratesDigest", Out::Str, false),
    o("submitted_at", "submittedAt", Out::Time, true),
    o("approved_at", "approvedAt", Out::Time, true),
    o("approval_expires_at", "approvalExpiresAt", Out::Time, true),
    o("rejected_reason", "rejectedReason", Out::Code, true),
    o("decided_at", "decidedAt", Out::Time, true),
    o("created_at", "createdAt", Out::Time, false),
    o("updated_at", "updatedAt", Out::Time, false),
    o("version", "version", Out::Int, false),
];
const PRICE_BOOK: &[OutField] = &[
    o("price_book_id", "priceBookId", Out::Uuid, false),
    o("product_line", "productLine", Out::Code, false),
    o("currency", "currency", Out::Code, false),
    o("market_code", "marketCode", Out::Code, false),
    o(
        "effective_version_id",
        "effectiveVersionId",
        Out::Uuid,
        true,
    ),
    o("updated_at", "updatedAt", Out::Time, false),
    o("version", "version", Out::Int, false),
];
const MARKET_FREEZE: &[OutField] = &[
    o("market_code", "marketCode", Out::Code, false),
    o("freeze_state", "state", Out::Code, false),
    o("reason_code", "reasonCode", Out::Code, true),
    o("frozen_at", "frozenAt", Out::Time, true),
    o("updated_at", "updatedAt", Out::Time, true),
    o("version", "version", Out::Int, false),
];
const PLAN_HOLDER: &[OutField] = &[
    o("holder_kind", "holderKind", Out::Code, false),
    o("holder_id", "holderId", Out::Uuid, false),
    o("plan_code", "planCode", Out::Code, false),
    o("price_book_id", "priceBookId", Out::Uuid, true),
    o("updated_at", "updatedAt", Out::Time, true),
    o("version", "version", Out::Int, false),
];
/// The limits a CF Meet user plan controls (an override sets a subset).
pub const PLAN_LIMITS: &[OutField] = &[
    o("meeting_minutes", "meetingMinutes", Out::Int, true),
    o("participants", "participants", Out::Int, true),
    o("recording", "recording", Out::Bool, true),
    o("storage_bytes", "storageBytes", Out::Int, true),
    o("attachment_bytes", "attachmentBytes", Out::Int, true),
    o("webinars", "webinars", Out::Bool, true),
];
const PLAN_OVERRIDE: &[OutField] = &[
    o("override_id", "overrideId", Out::Uuid, true),
    o("holder_kind", "holderKind", Out::Code, false),
    o("holder_id", "holderId", Out::Uuid, false),
    o("override_state", "state", Out::Code, false),
    o("limits", "limits", Out::Obj(PLAN_LIMITS), true),
    o("reason_code", "reasonCode", Out::Code, true),
    o("updated_at", "updatedAt", Out::Time, true),
    o("version", "version", Out::Int, false),
];
const HOLDER_KINDS: &[&str] = &["USER", "ORGANIZATION"];

/// A command on one price version (`/pricing/versions/{version_id}/<verb>`), at the
/// version's row version.
const fn price_version(
    id: &'static str,
    source: &'static str,
    reason: Reason,
    body: &'static [BodyField],
    statuses: &'static [&'static str],
    body_schema: &'static str,
) -> OwnerCommand {
    dp(
        id,
        source,
        Some(1),
        reason,
        body,
        statuses,
        PRICE_VERSION,
        body_schema,
        "PricingVersionCommandResult",
    )
}

pub const COMMANDS: &[OwnerCommand] = &[
    OwnerCommand {
        id: "communications.profile.suspend",
        owner: Owner::Communications,
        source: "POST /internal/v1/communications/operator/profiles/{cfmeet_id}/suspend",
        version: Some(1),
        reason: Reason::Forward,
        body: &[b(
            "reason",
            "reason",
            In::Enum(&["ABUSE", "SECURITY", "LEGAL", "OTHER"]),
            true,
        )],
        statuses: &["SUSPENDED", "RESTORED"],
        resource: COMM_PROFILE,
        body_schema: "CommunicationsProfileSuspendRequest",
        result_schema: "CommunicationsProfileCommandResult",
    },
    OwnerCommand {
        id: "communications.profile.restore",
        owner: Owner::Communications,
        source: "POST /internal/v1/communications/operator/profiles/{cfmeet_id}/restore",
        version: Some(1),
        reason: Reason::Forward,
        body: &[],
        statuses: &["RESTORED", "SUSPENDED"],
        resource: COMM_PROFILE,
        body_schema: "CommunicationsProfileRestoreRequest",
        result_schema: "CommunicationsProfileCommandResult",
    },
    OwnerCommand {
        id: "communications.report.review",
        owner: Owner::Communications,
        source: "POST /internal/v1/communications/operator/reports/{report_id}/review",
        version: Some(1),
        reason: Reason::Forward,
        body: &[
            b(
                "decision",
                "decision",
                In::Enum(&["DISMISSED", "ACTIONED"]),
                true,
            ),
            b(
                "reason",
                "reason",
                In::Enum(&["NO_VIOLATION", "ABUSE", "SECURITY", "LEGAL", "OTHER"]),
                true,
            ),
            b("hold_target", "holdTarget", In::Bool, false),
        ],
        statuses: &["DISMISSED", "ACTIONED"],
        resource: COMM_REPORT,
        body_schema: "CommunicationsReportReviewRequest",
        result_schema: "CommunicationsReportReviewResult",
    },
    OwnerCommand {
        id: "communications.backfill.run",
        owner: Owner::Communications,
        source: "POST /internal/v1/communications/operator/backfill",
        version: None,
        reason: Reason::Forward,
        body: &[
            b("limit", "limit", In::Int(1, 100), true),
            b("cursor", "cursor", In::NullableUuid, false),
            b("environment_id", "environmentId", In::NullableUuid, false),
        ],
        statuses: &["COMPLETED"],
        resource: &[
            o("environment_id", "environmentId", Out::Uuid, true),
            o("created", "created", Out::Int, false),
            o("next_cursor", "nextCursor", Out::Uuid, true),
        ],
        body_schema: "CommunicationsBackfillRequest",
        result_schema: "CommunicationsBackfillResult",
    },
    OwnerCommand {
        id: "communications.events.replay",
        owner: Owner::Communications,
        source: "POST /internal/v1/communications/operator/replay",
        version: None,
        reason: Reason::Forward,
        body: &[b("event_id", "eventId", In::Uuid, true)],
        statuses: &["REQUEUED"],
        resource: &[
            o("event_id", "eventId", Out::Uuid, false),
            o("requeued", "requeued", Out::Int, false),
        ],
        body_schema: "CommunicationsEventReplayRequest",
        result_schema: "CommunicationsEventReplayResult",
    },
    // Notifications.
    OwnerCommand {
        id: "notifications.delivery.retry",
        owner: Owner::Notifications,
        source: "POST /internal/v1/operator/deliveries/{delivery_id}/retry",
        version: None,
        reason: Reason::Forward,
        body: &[],
        statuses: &["RETRIED"],
        resource: NOTIF_DELIVERY,
        body_schema: "NotificationsDeliveryRetryRequest",
        result_schema: "NotificationsDeliveryRetryResult",
    },
    OwnerCommand {
        id: "notifications.suppression.remove",
        owner: Owner::Notifications,
        source: "POST /internal/v1/operator/email-suppressions/{suppression_id}/remove",
        version: Some(1),
        reason: Reason::Forward,
        body: &[],
        statuses: &["REMOVED"],
        resource: &[
            o("suppression_id", "digest", Out::Str, false),
            o("suppressed", "suppressed", Out::Bool, false),
        ],
        body_schema: "NotificationsSuppressionRemoveRequest",
        result_schema: "NotificationsSuppressionRemoveResult",
    },
    OwnerCommand {
        id: "notifications.routing.change",
        owner: Owner::Notifications,
        source: "POST /internal/v1/operator/routing/{category}/{channel}/mode",
        version: Some(1),
        reason: Reason::Forward,
        body: &[
            p("category", In::Code),
            p("channel", In::Enum(&["EMAIL", "PUSH", "IN_APP", "OPERATOR"])),
            b("mode", "mode", In::Enum(&["OFF", "SHADOW", "ACTIVE"]), true),
        ],
        statuses: &["OFF", "SHADOW", "ACTIVE"],
        resource: NOTIF_ROUTING,
        body_schema: "NotificationsRoutingChangeRequest",
        result_schema: "NotificationsRoutingChangeResult",
    },
    OwnerCommand {
        id: "notifications.reroute.cancel",
        owner: Owner::Notifications,
        source: "POST /internal/v1/operator/reroute/{reroute_id}/cancel",
        version: None,
        reason: Reason::Forward,
        body: &[],
        statuses: &["CANCELLED"],
        resource: NOTIF_DECISION,
        body_schema: "NotificationsRerouteCancelRequest",
        result_schema: "NotificationsRerouteCancelResult",
    },
    // Organizations.
    OwnerCommand {
        id: "organizations.organization.create",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations",
        version: None,
        reason: Reason::Forward,
        body: &[
            b("name", "name", In::Str(1, 120), true),
            b("slug", "slug", In::Str(1, 64), true),
            b("owner_user_id", "ownerUserId", In::Uuid, false),
        ],
        statuses: &["ACTIVE"],
        resource: ORG,
        body_schema: "OrganizationCreateRequest",
        result_schema: "OrganizationCommandResult",
    },
    org_lifecycle(
        "organizations.organization.suspend",
        "POST /internal/v1/organizations/{organization_id}/suspend",
        &["SUSPENDED"],
        "OrganizationSuspendRequest",
    ),
    org_lifecycle(
        "organizations.organization.archive",
        "POST /internal/v1/organizations/{organization_id}/archive",
        &["ARCHIVED"],
        "OrganizationArchiveRequest",
    ),
    org_lifecycle(
        "organizations.organization.reactivate",
        "POST /internal/v1/organizations/{organization_id}/reactivate",
        &["ACTIVE"],
        "OrganizationReactivateRequest",
    ),
    OwnerCommand {
        id: "organizations.ownership.recover",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations/{organization_id}/owners",
        // The user's current membership version, 0 when they have none.
        version: Some(0),
        reason: Reason::Forward,
        body: &[
            bt("user_id", "userId", In::Uuid),
            b("reason", "reason", In::Str(1, 500), true),
        ],
        statuses: &["ACTIVE"],
        resource: ORG_MEMBER,
        body_schema: "OrganizationOwnershipRecoverRequest",
        result_schema: "OrganizationMemberCommandResult",
    },
    OwnerCommand {
        id: "organizations.binding.change",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations/{organization_id}/scopes/primary/service-bindings/{service}/change",
        // The binding version, 0 when there is none.
        version: Some(0),
        reason: Reason::Forward,
        body: &[
            b("action", "action", In::Enum(&["SET", "CLEAR"]), true),
            b("service_account_id", "serviceAccountId", In::Uuid, false),
            b("credential_id", "credentialId", In::Uuid, false),
        ],
        statuses: &["BOUND", "REMOVED", "REVOKED"],
        resource: ORG_BINDING,
        body_schema: "OrganizationBindingChangeRequest",
        result_schema: "OrganizationBindingChangeResult",
    },
    OwnerCommand {
        id: "organizations.provisioning.retry",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations/{organization_id}/provisioning/retry",
        version: None,
        reason: Reason::Forward,
        body: &[],
        statuses: &[],
        resource: ORG_PROVISIONING,
        body_schema: "OrganizationProvisioningRetryRequest",
        result_schema: "OrganizationProvisioningRetryResult",
    },
    OwnerCommand {
        id: "organizations.member.change",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations/{organization_id}/members/{member_id}/change",
        version: Some(1),
        reason: Reason::Forward,
        body: &[
            b("role", "role", In::Enum(ROLES), false),
            b("membership_state", "status", In::Enum(&["ACTIVE", "SUSPENDED", "REMOVED"]), false),
        ],
        statuses: &["ACTIVE", "SUSPENDED", "REMOVED"],
        resource: ORG_MEMBER,
        body_schema: "OrganizationMemberChangeRequest",
        result_schema: "OrganizationMemberCommandResult",
    },
    OwnerCommand {
        id: "organizations.team.change",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations/{organization_id}/teams/{team_id}/change",
        version: Some(1),
        reason: Reason::Forward,
        body: &[
            b("name", "name", In::Str(1, 120), false),
            b("description", "description", In::NullableStr(0, 500), false),
            b("team_state", "status", In::Enum(&["ARCHIVED"]), false),
        ],
        statuses: &["ACTIVE", "ARCHIVED"],
        resource: ORG_TEAM,
        body_schema: "OrganizationTeamChangeRequest",
        result_schema: "OrganizationTeamChangeResult",
    },
    OwnerCommand {
        id: "organizations.policies.change",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/organizations/{organization_id}/policies/change",
        // The policy revision, 0 while on defaults.
        version: Some(0),
        reason: Reason::Forward,
        body: &[b("policy", "policy", In::Object, true)],
        statuses: &["UPDATED", "UNCHANGED"],
        resource: ORG_POLICIES,
        body_schema: "OrganizationPoliciesChangeRequest",
        result_schema: "OrganizationPoliciesChangeResult",
    },
    OwnerCommand {
        id: "organizations.events.replay",
        owner: Owner::Organizations,
        source: "POST /internal/v1/admin/event-delivery/replay",
        version: None,
        reason: Reason::Forward,
        body: &[
            b("destination", "destination", In::Str(1, 64), false),
            b("event_ids", "eventIds", In::UuidList(500), false),
        ],
        statuses: &["REPLAYED"],
        resource: ORG_REPLAY,
        body_schema: "OrganizationEventsReplayRequest",
        result_schema: "OrganizationEventsReplayResult",
    },
    // Platform Access.
    pa(
        "identity.status.change",
        "POST /internal/v1/operator/{resource_type}/{resource_id}/status",
        Some(1),
        &[b("resource_state", "status", In::Enum(&["ACTIVE", "SUSPENDED", "REVOKED"]), true)],
        &["ACTIVE", "SUSPENDED", "REVOKED"],
        PA_STATUS,
        "IdentityStatusChangeRequest",
        "IdentityStatusChangeResult",
    ),
    pa(
        "identity.scope-grant.change",
        "POST /internal/v1/operator/applications/{application_id}/scope-grants",
        None,
        SCOPE_BODY,
        &["GRANTED", "REVOKED"],
        PA_SCOPE,
        "IdentityScopeGrantChangeRequest",
        "IdentityScopeGrantChangeResult",
    ),
    pa(
        "identity.privileged-scope-grant.change",
        "POST /internal/v1/operator/applications/{application_id}/privileged-scope-grants",
        None,
        SCOPE_BODY,
        &["GRANTED", "REVOKED"],
        PA_SCOPE,
        "IdentityPrivilegedScopeGrantChangeRequest",
        "IdentityScopeGrantChangeResult",
    ),
    pa(
        "identity.key.revoke",
        "POST /internal/v1/operator/credentials/{key_id}/revoke",
        Some(1),
        &[],
        &["REVOKED", "ACTIVE", "SUSPENDED"],
        PA_CREDENTIAL,
        "IdentityKeyRevokeRequest",
        "IdentityKeyCommandResult",
    ),
    pa(
        "identity.key.suspend",
        "POST /internal/v1/operator/credentials/{key_id}/suspend",
        Some(1),
        &[b("reason_code", "reasonCode", In::Code, true)],
        &["SUSPENDED"],
        PA_CREDENTIAL,
        "IdentityKeySuspendRequest",
        "IdentityKeyCommandResult",
    ),
    pa(
        "identity.key.resume",
        "POST /internal/v1/operator/credentials/{key_id}/resume",
        Some(1),
        &[],
        &["ACTIVE"],
        PA_CREDENTIAL,
        "IdentityKeyResumeRequest",
        "IdentityKeyCommandResult",
    ),
    pa(
        "identity.record.register",
        "POST /internal/v1/operator/environments/{environment_id}/principals",
        None,
        &[
            p("environment_id", In::Uuid),
            b("kind", "kind", In::Code, false),
            b("external_reference", "externalReference", In::Str(1, 256), false),
            b("service_account_id", "serviceAccountId", In::Uuid, false),
        ],
        &["REGISTERED"],
        PA_PRINCIPAL,
        "IdentityRecordRegisterRequest",
        "IdentityRecordCommandResult",
    ),
    pa(
        "identity.record.status.change",
        "POST /internal/v1/operator/principals/{record_id}/status",
        Some(1),
        &[b("principal_state", "status", In::Enum(&["ACTIVE", "SUSPENDED", "DEACTIVATED"]), true)],
        &["ACTIVE", "SUSPENDED", "DEACTIVATED"],
        PA_PRINCIPAL,
        "IdentityRecordStatusChangeRequest",
        "IdentityRecordCommandResult",
    ),
    pa(
        "identity.record.remove",
        "POST /internal/v1/operator/principals/{record_id}/remove",
        Some(1),
        &[],
        &["TOMBSTONED"],
        PA_PRINCIPAL,
        "IdentityRecordRemoveRequest",
        "IdentityRecordCommandResult",
    ),
    pa(
        "identity.app-grant.revoke",
        "POST /internal/v1/operator/app-grants/{grant_id}/revoke",
        Some(1),
        &[],
        &["REVOKED"],
        PA_APP_GRANT,
        "IdentityAppGrantRevokeRequest",
        "IdentityAppGrantRevokeResult",
    ),
    pa(
        "partners.key.revoke",
        "POST /internal/v1/operator/credentials/{key_id}/revoke",
        Some(1),
        &[],
        &["REVOKED", "ACTIVE", "SUSPENDED"],
        PA_CREDENTIAL,
        "PartnerKeyRevokeRequest",
        "IdentityKeyCommandResult",
    ),
    pa(
        "partners.service-account.suspend",
        "POST /internal/v1/operator/service-accounts/{account_id}/status",
        Some(1),
        &[fixed("status", "SUSPENDED")],
        &["SUSPENDED"],
        PA_STATUS,
        "PartnerServiceAccountSuspendRequest",
        "IdentityStatusChangeResult",
    ),
    pa(
        "partners.website.remove",
        "POST /internal/v1/operator/environments/{environment_id}/origins/remove",
        Some(1),
        &[
            p("environment_id", In::Uuid),
            b("origin", "origin", In::Str(1, 270), true),
        ],
        &["REMOVED"],
        PA_ORIGINS,
        "PartnerWebsiteRemoveRequest",
        "PartnerWebsiteRemoveResult",
    ),
    pa(
        "partners.user.sign-out",
        "POST /internal/v1/operator/principals/{user_id}/sign-out",
        None,
        &[],
        &["SIGNED_OUT"],
        PA_SIGNED_OUT,
        "PartnerUserSignOutRequest",
        "PartnerUserSignOutResult",
    ),
    // Developer Platform: partners and apps.
    dp(
        "partners.workspace.state.change",
        "POST /internal/v1/operator/organizations/{workspace_id}/state",
        Some(1),
        Reason::Ledger,
        &[
            b("state", "state", In::Enum(WORKSPACE_STATES), true),
            b("reason", "reason", In::Code, true),
        ],
        WORKSPACE_STATES,
        DP_WORKSPACE,
        "PartnerWorkspaceStateChangeRequest",
        "PartnerWorkspaceStateChangeResult",
    ),
    dp(
        "partners.push-key.disable",
        "POST /internal/v1/operator/organizations/{workspace_id}/push-providers/{push_key_id}/disable",
        Some(1),
        Reason::Forward,
        &[],
        &["DISABLED"],
        DP_PUSH_KEY,
        "PartnerPushKeyDisableRequest",
        "PartnerPushKeyDisableResult",
    ),
    dp(
        "partners.webhook-delivery.retry",
        "POST /internal/v1/operator/organizations/{workspace_id}/webhook-deliveries/{delivery_id}/retry",
        None,
        Reason::Ledger,
        &[],
        &["RETRIED"],
        DP_DELIVERY,
        "PartnerWebhookDeliveryRetryRequest",
        "PartnerWebhookDeliveryRetryResult",
    ),
    dp(
        "partners.usage.reprocess",
        "POST /internal/v1/operator/usage/rollups/rebuild",
        None,
        Reason::Ledger,
        &[
            b("from", "from", In::Date, true),
            b("to", "to", In::Date, true),
            b("environment_id", "environmentId", In::Uuid, false),
        ],
        &["REPROCESSED"],
        DP_REPROCESS,
        "PartnerUsageReprocessRequest",
        "PartnerUsageReprocessResult",
    ),
    dp(
        "partners.support-tier.change",
        "POST /internal/v1/operator/organizations/{workspace_id}/support-profile",
        // The support profile version, 0 while the workspace has none.
        Some(0),
        Reason::Ledger,
        &[
            b("support_tier", "supportTier", In::Enum(SUPPORT_TIERS), true),
            b("support_reference", "supportReference", In::Str(1, 160), false),
        ],
        SUPPORT_TIERS,
        DP_SUPPORT,
        "PartnerSupportTierChangeRequest",
        "PartnerSupportProfileCommandResult",
    ),
    dp(
        "partners.incident.record",
        "POST /internal/v1/operator/organizations/{workspace_id}/incidents",
        // The workspace's support profile version, 0 while it has none.
        Some(0),
        Reason::Ledger,
        &[
            p("workspace_id", In::Uuid),
            b("incident_ref", "incidentRef", In::Str(1, 160), true),
            b("occurred_at", "occurredAt", In::Time, true),
        ],
        &["RECORDED"],
        DP_SUPPORT,
        "PartnerIncidentRecordRequest",
        "PartnerSupportProfileCommandResult",
    ),
    dp(
        "apps.app.suspend",
        "POST /internal/v1/operator/apps/{app_id}/suspend",
        Some(1),
        Reason::Forward,
        &[],
        &["SUSPENDED"],
        DP_APP,
        "AppSuspendRequest",
        "AppCommandResult",
    ),
    dp(
        "apps.app.lift-suspension",
        "POST /internal/v1/operator/apps/{app_id}/lift-suspension",
        // Back to the state the app had before its suspension.
        Some(1),
        Reason::Forward,
        &[],
        &[],
        DP_APP,
        "AppLiftSuspensionRequest",
        "AppCommandResult",
    ),
    // Developer Platform: pricing and plans (every command two-person; nothing charges).
    dp(
        "pricing.version.draft",
        "POST /internal/v1/operator/pricing/price-books/{price_book_id}/versions",
        // The price book (lineage) version.
        Some(1),
        Reason::Ledger,
        &[b("rates", "rates", In::Object, false)],
        &["DRAFT"],
        PRICE_VERSION,
        "PricingVersionDraftRequest",
        "PricingVersionCommandResult",
    ),
    price_version(
        "pricing.version.edit",
        "POST /internal/v1/operator/pricing/versions/{version_id}/edit",
        Reason::Ledger,
        &[b("rates", "rates", In::Object, true)],
        &["DRAFT"],
        "PricingVersionEditRequest",
    ),
    price_version(
        "pricing.version.submit",
        "POST /internal/v1/operator/pricing/versions/{version_id}/submit",
        Reason::Ledger,
        &[],
        &["SUBMITTED"],
        "PricingVersionSubmitRequest",
    ),
    price_version(
        "pricing.version.approve",
        "POST /internal/v1/operator/pricing/versions/{version_id}/approve",
        Reason::Ledger,
        &[],
        &["APPROVED"],
        "PricingVersionApproveRequest",
    ),
    price_version(
        "pricing.version.reject",
        "POST /internal/v1/operator/pricing/versions/{version_id}/reject",
        Reason::Forward,
        &[],
        &["REJECTED"],
        "PricingVersionRejectRequest",
    ),
    price_version(
        "pricing.version.schedule",
        "POST /internal/v1/operator/pricing/versions/{version_id}/schedule",
        Reason::Ledger,
        &[b("effective_from", "effectiveFrom", In::Time, true)],
        &["SCHEDULED"],
        "PricingVersionScheduleRequest",
    ),
    price_version(
        "pricing.version.activate",
        "POST /internal/v1/operator/pricing/versions/{version_id}/activate",
        Reason::Ledger,
        &[],
        &["ACTIVE"],
        "PricingVersionActivateRequest",
    ),
    dp(
        "pricing.price-book.rollback",
        "POST /internal/v1/operator/pricing/price-books/{price_book_id}/rollback",
        Some(1),
        Reason::Ledger,
        &[b("version_id", "versionId", In::Uuid, true)],
        &["ROLLED_BACK"],
        PRICE_BOOK,
        "PricingPriceBookRollbackRequest",
        "PricingPriceBookRollbackResult",
    ),
    dp(
        "pricing.market.freeze",
        "POST /internal/v1/operator/pricing/markets/{market_code}/freeze",
        // The market's freeze version, 0 when it was never frozen.
        Some(0),
        Reason::Required,
        &[],
        &["FROZEN"],
        MARKET_FREEZE,
        "PricingMarketFreezeRequest",
        "PricingMarketFreezeResult",
    ),
    dp(
        "pricing.market.unfreeze",
        "POST /internal/v1/operator/pricing/markets/{market_code}/unfreeze",
        Some(0),
        Reason::Required,
        &[],
        &["UNFROZEN"],
        MARKET_FREEZE,
        "PricingMarketUnfreezeRequest",
        "PricingMarketFreezeResult",
    ),
    dp(
        "pricing.plan.assign",
        "POST /internal/v1/operator/pricing/plan-assignments",
        // The holder's version, 0 when never assigned (on Free).
        Some(0),
        Reason::Forward,
        &[
            bt("holder_kind", "holderKind", In::Enum(HOLDER_KINDS)),
            bt("holder_id", "holderId", In::Uuid),
            b("plan_code", "planCode", In::Code, true),
            b("price_book_id", "priceBookId", In::Uuid, true),
            b("billing_account_ref", "billingAccountRef", In::Str(1, 128), false),
        ],
        // The resulting state is the assigned plan code.
        &[],
        PLAN_HOLDER,
        "PricingPlanAssignRequest",
        "PricingPlanAssignResult",
    ),
    dp(
        "pricing.override.change",
        "POST /internal/v1/operator/pricing/overrides",
        // The override's version, 0 when the holder has none.
        Some(0),
        Reason::Required,
        &[
            bt("holder_kind", "holderKind", In::Enum(HOLDER_KINDS)),
            bt("holder_id", "holderId", In::Uuid),
            b("limits", "limits", In::NullableObject, true),
        ],
        &["ACTIVE", "REVOKED"],
        PLAN_OVERRIDE,
        "PricingOverrideChangeRequest",
        "PricingOverrideChangeResult",
    ),
];

pub fn command(id: &str) -> Option<&'static OwnerCommand> {
    COMMANDS.iter().find(|c| c.id == id)
}

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
        body,
        statuses,
        resource,
        body_schema,
        result_schema,
    }
}

pub const COMMANDS: &[OwnerCommand] = &[
    OwnerCommand {
        id: "communications.profile.suspend",
        owner: Owner::Communications,
        source: "POST /internal/v1/communications/operator/profiles/{cfmeet_id}/suspend",
        version: Some(1),
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
];

pub fn command(id: &str) -> Option<&'static OwnerCommand> {
    COMMANDS.iter().find(|c| c.id == id)
}

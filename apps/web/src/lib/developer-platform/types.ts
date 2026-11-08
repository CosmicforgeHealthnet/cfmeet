/**
 * Developer Platform console types, hand-written from the published contract:
 *   packages/contracts/platform-developer/http/v1.openapi.json (1.6.0)
 *   packages/contracts/platform-developer/control/v1.schema.json  ($defs)
 *   packages/contracts/platform-developer/webhooks/v1.schema.json ($defs)
 *
 * Only the fields the console reads are typed. A "developer workspace" here is a
 * Developer Platform organization (an API-customer workspace): it is NOT a first-party
 * CF Meet business organization (`/api/v2/organizations`, Organizations service).
 */

export type Role = "OWNER" | "ADMIN" | "DEVELOPER" | "ANALYST" | "BILLING";
export const ROLES: Role[] = ["OWNER", "ADMIN", "DEVELOPER", "ANALYST", "BILLING"];

export type OrganizationState = "INVITED" | "ACTIVE" | "RESTRICTED" | "SUSPENDED" | "CLOSED";

export type Page<T> = { items: T[]; nextCursor: string | null };

export type Workspace = {
  id: string;
  displayName: string;
  state: OrganizationState;
  stateReason: string | null;
  supportEmail: string | null;
  createdAt: string;
  updatedAt: string;
  closedAt: string | null;
  version: number;
  myRole: Role | null;
  classification?: "FIRST_PARTY" | "EXTERNAL";
};

export type Member = {
  id: string;
  organizationId: string;
  userId: string;
  role: Role;
  status: "ACTIVE" | "REMOVED";
  joinedVia: "CREATOR" | "INVITATION" | "OPERATOR";
  createdAt: string;
  updatedAt: string;
  version: number;
};

export type InvitationStatus = "PENDING" | "ACCEPTED" | "REVOKED" | "EXPIRED";
export type Invitation = {
  id: string;
  organizationId: string;
  email: string;
  role: Role;
  status: InvitationStatus;
  createdAt: string;
  expiresAt: string;
  decidedAt: string | null;
};
/** Returned once by createInvitation: `token` is the one-time acceptance token. */
export type InvitationWithToken = Invitation & { token?: string };

export type ProjectStatus = "ACTIVE" | "SUSPENDED" | "ARCHIVED";
export type Project = {
  id: string;
  organizationId: string;
  displayName: string;
  description: string | null;
  supportEmail: string | null;
  labels: Record<string, string>;
  status: ProjectStatus;
  statusReason: string | null;
  createdAt: string;
  updatedAt: string;
  archivedAt: string | null;
  version: number;
};

export type EnvironmentKind = "SANDBOX" | "TEST" | "PRODUCTION";
export type ProductionAccessState =
  | "NOT_APPLICABLE"
  | "SANDBOX_READY"
  | "PRODUCTION_REQUESTED"
  | "UNDER_REVIEW"
  | "APPROVED"
  | "REJECTED"
  | "SUSPENDED"
  | "LIVE";
export type Environment = {
  id: string;
  projectId: string;
  organizationId: string;
  kind: EnvironmentKind;
  displayName: string;
  status: "ACTIVE" | "SUSPENDED";
  productionAccessState: ProductionAccessState;
  createdAt: string;
  updatedAt: string;
  version: number;
  authorityStatus?: "ACTIVE" | "SUSPENDED";
  liveTelecomAllowed?: boolean;
};

export type ServiceAccount = {
  id: string;
  organizationId: string;
  projectId: string;
  environmentId: string;
  name: string;
  description: string | null;
  status: "ACTIVE" | "SUSPENDED" | "REVOKED";
  createdAt: string;
  updatedAt: string;
};

export type CredentialEffectiveState =
  | "ACTIVE"
  | "ROTATION_OVERLAP"
  | "NOT_YET_VALID"
  | "EXPIRED"
  | "SUPERSEDED"
  | "REVOKED"
  | "SUSPENDED";
export type QuotaValues = { apiRequestsPerMinute?: number | null; apiRequestsPerDay?: number | null } | null;
export type Credential = {
  id: string;
  organizationId: string;
  projectId: string;
  environmentId: string;
  environmentKind: EnvironmentKind | null;
  serviceAccountId: string | null;
  publicId: string | null;
  masked: string | null;
  label: string | null;
  scopes: string[] | null;
  status: "ACTIVE" | "REVOKED" | null;
  effectiveState: CredentialEffectiveState | null;
  notBefore: string | null;
  expiresAt: string | null;
  revokedAt: string | null;
  suspendedAt: string | null;
  rotatedFromCredentialId: string | null;
  supersededByCredentialId: string | null;
  supersessionValidUntil: string | null;
  lastAuthenticatedAt: string | null;
  expiringSoon: boolean | null;
  createdAt: string | null;
  updatedAt: string | null;
  limits: QuotaValues;
};
/** createCredential / rotateCredential: `secret` exactly once, or `secretRedacted` on replay. */
export type IssuedCredential = {
  credential: Credential;
  secret?: string;
  secretRedacted?: true;
  rotatedFromCredentialId?: string;
};

export type CredentialPolicy = {
  organizationId: string;
  configured: boolean;
  maxLifetimeDays: number | null;
  maxRotationOverlapMinutes: number | null;
};

export type ScopeRegistryEntry = {
  name: string;
  description: string | null;
  trust: "STANDARD" | "FIRST_PARTY_ONLY" | "PRIVILEGED_OPERATOR";
  selfServiceGrantable: boolean;
  productionCapability: string | null;
  regulated: boolean;
};
export type ScopeRegistry = { items: ScopeRegistryEntry[]; nextCursor: null };
/** Platform Access application scope grants (free-form objects in the contract). */
export type ProjectScopes = { projectId: string; scopes: Record<string, unknown>[] };
export type ScopeGrant = {
  projectId: string;
  scope: string;
  environmentKinds: EnvironmentKind[] | null;
  grantBasis: "STANDARD" | "PRIVILEGED_OPERATOR" | null;
};

export type LimitingLevel =
  | "PLATFORM_DEFAULT"
  | "ORGANIZATION_PLAN"
  | "OPERATOR_OVERRIDE"
  | "PROJECT_PLAN"
  | "ENVIRONMENT"
  | "SERVICE_ACCOUNT"
  | "CREDENTIAL";
export type EntitlementValues = Partial<
  Record<
    | "apiRequestsPerMinute"
    | "apiRequestsPerDay"
    | "concurrentCalls"
    | "meetingParticipants"
    | "activeNumbers"
    | "smsPerDay"
    | "messagingAttachmentBytes",
    number
  >
>;
export type Entitlements = {
  environmentId: string;
  environmentKind: EnvironmentKind | null;
  organizationState: OrganizationState | null;
  apiRequestsPerMinute: number;
  apiRequestsPerDay: number;
  limitingLevel: { apiRequestsPerMinute: LimitingLevel; apiRequestsPerDay: LimitingLevel };
  domainEntitlements: EntitlementValues;
};
export type UsageWarning = {
  limit: string;
  level: "WARNING" | "EXHAUSTED";
  current: number;
  ceiling: number;
  ratio: number;
};
export type EnvironmentLimit = {
  limit: string;
  window: "MINUTE" | "DAY" | "CURRENT";
  ceiling: number;
  limitingLevel?: LimitingLevel;
  current: number | null;
  remaining: number | null;
  resetsAt?: string;
  enforcedBy: string;
};
export type EnvironmentLimits = {
  environmentId: string;
  generatedAt: string;
  limits: EnvironmentLimit[];
  warnings: UsageWarning[];
};
export type QuotaRestriction = {
  subjectKind: "SERVICE_ACCOUNT" | "CREDENTIAL";
  subjectId: string;
  environmentId: string;
  limits: QuotaValues;
};

export type UsageType =
  | "API_OPERATION"
  | "MEETING_PARTICIPANT_MINUTE"
  | "CALL_MINUTE_APP"
  | "CALL_MINUTE_PUBLIC_NETWORK"
  | "NUMBER_ACTIVE"
  | "SMS_SEGMENT"
  | "RECORDING_MINUTE"
  | "STORAGE_BYTE_TIME";
export const USAGE_TYPES: UsageType[] = [
  "API_OPERATION",
  "MEETING_PARTICIPANT_MINUTE",
  "CALL_MINUTE_APP",
  "CALL_MINUTE_PUBLIC_NETWORK",
  "NUMBER_ACTIVE",
  "SMS_SEGMENT",
  "RECORDING_MINUTE",
  "STORAGE_BYTE_TIME",
];
export type UsageSummary = {
  environmentId: string;
  environmentKind: EnvironmentKind;
  sandbox: boolean;
  from: string;
  to: string;
  usageTypes: UsageType[];
  days: { day: string; usageType: UsageType; unit: string; quantity: number; factCount: number }[];
  totals: { usageType: UsageType; unit: string; quantity: number; factCount?: number }[];
};
export type UsageWarnings = { environmentId: string; generatedAt: string; items: UsageWarning[] };

export type ProductionCapability =
  | "PLATFORM_CONTROL"
  | "MEETINGS"
  | "MESSAGING"
  | "APP_CALLING"
  | "PUBLIC_NETWORK_CALLING"
  | "PRIVACY_RELAY"
  | "NUMBERS"
  | "SMS";
export type ProductionAccessRequest = {
  id: string;
  projectId: string;
  state:
    | "PRODUCTION_REQUESTED"
    | "UNDER_REVIEW"
    | "APPROVED"
    | "REJECTED"
    | "CANCELLED"
    | "LIVE"
    | "SUSPENDED";
  requestedCapabilities: ProductionCapability[];
  approvedCapabilities: ProductionCapability[];
  useCaseSummary: string | null;
  requestedBy: string;
  approvalExpiresAt: string | null;
  productionEnvironmentId: string | null;
  activatedAt: string | null;
  createdAt: string;
  updatedAt: string;
  version: number;
};
export type ProductionAccessStatus = {
  projectId: string;
  state: string;
  enabledCapabilities: ProductionCapability[];
  productionEnvironmentId: string | null;
  currentRequest: ProductionAccessRequest | null;
  capabilityCatalogue: { name: ProductionCapability; regulated: boolean; scopes: string[] }[];
};

export type ActivityEvent = {
  id: string;
  actorKind: "USER" | "OPERATOR" | "SERVICE";
  actorRef: string;
  action: string;
  targetType: string;
  targetId: string;
  outcome: "SUCCEEDED" | "DENIED" | "FAILED";
  requestId: string;
  correlationId: string;
  summary: Record<string, unknown>;
  occurredAt: string;
};

export type CommercialPlan = {
  id: string;
  code: string;
  version: number;
  displayName: string;
  status: "ACTIVE" | "RETIRED";
  entitlements: EntitlementValues;
  effectiveFrom: string;
  effectiveUntil: string | null;
};
export type PlanAssignment = {
  id: string;
  projectId: string | null;
  planId: string;
  planCode: string;
  planVersion: number;
  source: "INTERNAL" | "CONTRACT" | "SELF_SERVICE";
  effectiveFrom: string;
  effectiveUntil: string | null;
  plan?: CommercialPlan;
};
export type BillingAccount = {
  id: string;
  classification: "RETAIL" | "CONTRACT" | "INTERNAL_COST_CENTER";
  status: "ACTIVE" | "SUSPENDED" | "CLOSED";
  billingContactMemberId: string | null;
  currency: string | null;
  country: string | null;
  externalBillingRef: string | null;
  version: number;
};
export type OrganizationCommercial = {
  organizationId: string;
  planAssignment: PlanAssignment | null;
  projectPlanAssignments: PlanAssignment[];
  billingAccount: BillingAccount | null;
};

export type SupportProfile = {
  organizationId: string;
  supportTier: "COMMUNITY" | "STANDARD" | "PREMIUM" | "ENTERPRISE" | "INTERNAL";
  supportReference: string | null;
  primaryTechnicalContactMemberId: string | null;
  incidentContactMemberId: string | null;
  productionStatus: "SANDBOX_ONLY" | "PRODUCTION_PENDING" | "LIVE" | "SUSPENDED";
  lastIncident: Record<string, unknown> | null;
  updatedAt: string | null;
  version: number;
};

export type WebhookStatus = "ACTIVE" | "DISABLED" | "DELETED";
export type ConsoleWebhook = {
  webhookId: string;
  url: string;
  eventTypes: string[];
  description: string | null;
  status: WebhookStatus;
  secretVersion: number;
  version: number;
  createdAt: string;
  updatedAt: string;
  environmentId: string;
  environmentKind: EnvironmentKind;
  origin: "NATIVE" | "MIGRATED_FROM_CALLING";
  health: {
    consecutiveFailures: number;
    lastSuccessAt: string | null;
    lastFailureAt: string | null;
    disabledReason: "CONSECUTIVE_FAILURES" | null;
  };
};
/** createEnvironmentWebhook / rotateEnvironmentWebhookSecret: `signingSecret` once. */
export type ConsoleWebhookWithSecret = { webhook: ConsoleWebhook; signingSecret?: string; secretRedacted?: true };
export type DeliveryStatus =
  | "PENDING"
  | "DELIVERING"
  | "RETRY_SCHEDULED"
  | "SUCCEEDED"
  | "FAILED"
  | "CANCELLED"
  | "SHADOW";
export const DELIVERY_STATUSES: DeliveryStatus[] = [
  "PENDING",
  "DELIVERING",
  "RETRY_SCHEDULED",
  "SUCCEEDED",
  "FAILED",
  "CANCELLED",
  "SHADOW",
];
export type WebhookDelivery = {
  deliveryId: string;
  webhookId: string;
  eventId: string;
  eventType: string;
  replayOf: string | null;
  status: DeliveryStatus;
  attemptCount: number;
  nextAttemptAt: string | null;
  lastHttpStatus: number | null;
  failureCategory: string | null;
  createdAt: string;
  completedAt: string | null;
};
export type WebhookAttempt = {
  attemptNumber: number;
  startedAt: string;
  finishedAt: string;
  httpStatus: number | null;
  outcome: "SUCCEEDED" | "RETRY" | "FAILED";
  errorClass: string | null;
};

/** Public discovery document (`GET /discovery`, no auth): only the fields shown. */
export type Discovery = {
  documentation?: { aggregate?: string; reference?: string };
  families?: { domain: string; name: string; version?: string; enabled: boolean; operations: number; openapi: string }[];
  sdks?: {
    status?: string;
    packages?: { language?: string; package?: string; version?: string; install?: string; quickstart?: string; credentialHandling?: string }[];
  };
};

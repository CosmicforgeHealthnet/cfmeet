/**
 * Organization administration rules of the web app (Blueprint v1.0 Sections 4, 5, 8, 17;
 * ORG-003, ORG-006).
 *
 * - Every control is gated by the capabilities the server returned for the caller in that
 *   organization (`BusinessOrganization.viewer.capabilities`, re-read on every visit). The
 *   client never derives a permission from a cached role ("don't cache owner status"):
 *   the role is shown, the capability decides. The server stays authoritative.
 * - Errors are read from the stable envelope `{ error: { code, message } }`.
 *
 * Pure: type-only imports, so node tests run it directly (tests/organization-admin.test.mjs).
 */
import type {
  BusinessOrganization,
  OrganizationMemberResponse,
  OrganizationProfile,
  UpdateBusinessOrganizationRequest,
} from "../../types/generated/meeting-service";

export const ORG_CAPABILITIES = {
  read: "organization.read",
  membersRead: "organization.members.read",
  profileWrite: "organization.profile.write",
  membersInvite: "organization.members.invite",
  membersManage: "organization.members.manage",
  ownershipTransfer: "organization.ownership.transfer",
  lifecycleManage: "organization.lifecycle.manage",
  auditRead: "organization.audit.read",
  policiesManage: "organization.policies.manage",
  teamsManage: "organization.teams.manage",
} as const;

export type OrgCapability = (typeof ORG_CAPABILITIES)[keyof typeof ORG_CAPABILITIES];

/** Default deny: true only when the server listed the capability for this caller. */
export function orgCan(org: Pick<BusinessOrganization, "viewer"> | null | undefined, capability: OrgCapability): boolean {
  const viewer = org?.viewer;
  if (!viewer || viewer.status !== "ACTIVE") return false;
  return Array.isArray(viewer.capabilities) && viewer.capabilities.includes(capability);
}

/** The administration sections the caller may open in an organization (server-derived). */
export interface OrganizationSections {
  overview: boolean;
  members: boolean;
  invitations: boolean;
  audit: boolean;
  settings: boolean;
  lifecycle: boolean;
  ownership: boolean;
}

export function organizationSections(org: Pick<BusinessOrganization, "viewer"> | null | undefined): OrganizationSections {
  const member = !!org?.viewer;
  return {
    overview: member,
    members: orgCan(org, ORG_CAPABILITIES.membersRead) || orgCan(org, ORG_CAPABILITIES.membersManage),
    invitations: orgCan(org, ORG_CAPABILITIES.membersInvite),
    audit: orgCan(org, ORG_CAPABILITIES.auditRead),
    settings: orgCan(org, ORG_CAPABILITIES.profileWrite),
    lifecycle: orgCan(org, ORG_CAPABILITIES.lifecycleManage),
    ownership: orgCan(org, ORG_CAPABILITIES.ownershipTransfer),
  };
}

export const BUSINESS_ROLES = ["OWNER", "ADMIN", "MANAGER", "MEMBER", "GUEST"] as const;
export type BusinessRole = (typeof BUSINESS_ROLES)[number];
export const MEMBERSHIP_STATUSES = ["ACTIVE", "SUSPENDED", "REMOVED"] as const;

/** The organization role of a member item (`organizationRole` when served, else the legacy role). */
export function memberRole(member: Pick<OrganizationMemberResponse, "organizationRole" | "role">): string {
  return (member.organizationRole || member.role || "MEMBER").toUpperCase();
}

/**
 * Roles the caller may offer for another member, mirroring the documented rules (OWNERs
 * manage anyone; ADMINs manage MANAGER, MEMBER and GUEST; nobody raises their own role).
 * Only a hint for the form — the Organizations Service decides and answers
 * PERMISSION_DENIED / LAST_OWNER.
 */
export function assignableRoles(viewerRole: string | null | undefined, targetRole: string): BusinessRole[] {
  const actor = (viewerRole ?? "").toUpperCase();
  if (actor === "OWNER") return [...BUSINESS_ROLES];
  if (actor === "ADMIN" && ["MANAGER", "MEMBER", "GUEST"].includes(targetRole.toUpperCase())) {
    return ["MANAGER", "MEMBER", "GUEST"];
  }
  return [];
}

/** Roles an invitation may carry from this caller (OWNER: any but OWNER; others: below ADMIN). */
export function invitableRoles(viewerRole: string | null | undefined): BusinessRole[] {
  const actor = (viewerRole ?? "").toUpperCase();
  if (actor === "OWNER") return ["ADMIN", "MANAGER", "MEMBER", "GUEST"];
  if (actor === "ADMIN") return ["MANAGER", "MEMBER", "GUEST"];
  if (actor === "MANAGER") return ["MEMBER", "GUEST"];
  return [];
}

/** Whether a member row may show manage controls (never for the caller's own row). */
export function canManageMember(
  org: Pick<BusinessOrganization, "viewer"> | null | undefined,
  member: Pick<OrganizationMemberResponse, "id" | "organizationRole" | "role" | "version">,
): boolean {
  if (!orgCan(org, ORG_CAPABILITIES.membersManage)) return false;
  if (org?.viewer?.membershipId === member.id) return false;
  if (typeof member.version !== "number") return false; // legacy directory items cannot be PATCHed
  return assignableRoles(org?.viewer?.role, memberRole(member)).length > 0;
}

export interface ErrorEnvelopeInfo {
  status: number;
  code: string;
  message: string | null;
}

export function errorEnvelopeOf(error: unknown): ErrorEnvelopeInfo {
  const e = error as { response?: { status?: unknown; data?: { error?: { code?: unknown; message?: unknown } } } } | null;
  const status = typeof e?.response?.status === "number" ? e.response.status : 0;
  const body = e?.response?.data?.error;
  return {
    status,
    code: typeof body?.code === "string" ? body.code : status ? `HTTP_${status}` : "NETWORK_UNAVAILABLE",
    message: typeof body?.message === "string" ? body.message : null,
  };
}

const ORG_ERROR_TEXT: Record<string, string> = {
  INVITE_EXPIRED: "This invitation has expired. Ask the organization for a new one.",
  INVITATION_INVALID: "This invitation is no longer valid (it was used, revoked or never existed).",
  MEMBERSHIP_REQUIRED: "You are not a member of this organization.",
  LAST_OWNER: "An organization must keep at least one owner. Transfer ownership first.",
  ORGANIZATION_SUSPENDED: "This organization is suspended. Only its owners can still view it.",
  ORGANIZATION_ARCHIVED: "This organization is archived.",
  VERSION_CONFLICT: "Someone else changed this in the meantime. The latest version has been loaded — review it and try again.",
  PROVISIONING_PENDING: "The organization is still being set up. Try again in a moment.",
  SLUG_TAKEN: "That address (slug) is already used by another organization.",
  IDEMPOTENCY_CONFLICT: "This request was already sent with different details. Start again.",
  INVITATION_REQUIRED: "New members join by invitation. Send an invitation instead.",
  ORGANIZATION_NOT_FOUND: "This organization does not exist or you are no longer a member.",
  FEATURE_DISABLED: "Organization administration is not enabled on this deployment yet.",
  PERMISSION_DENIED: "Your role in this organization does not allow this.",
  RATE_LIMITED: "Too many requests. Wait a moment and try again.",
  SERVICE_UNAVAILABLE: "The organization service is temporarily unavailable. Try again.",
  VALIDATION_ERROR: "Some details are not valid.",
  AUTHENTICATION_REQUIRED: "Your session has ended. Sign in again.",
  NETWORK_UNAVAILABLE: "You appear to be offline. Check your connection and try again.",
};

/** User-facing text for an organization request failure (stable codes first, then the server message). */
export function organizationErrorText(error: unknown): string {
  const info = errorEnvelopeOf(error);
  const known = ORG_ERROR_TEXT[info.code];
  if (info.code === "VALIDATION_ERROR" && info.message) return info.message;
  return known ?? info.message ?? "The request failed. Try again.";
}

/** A conflict that requires re-reading the resource before retrying. */
export function needsReload(error: unknown): boolean {
  return errorEnvelopeOf(error).code === "VERSION_CONFLICT";
}

/** A one-time `Idempotency-Key` for a create (kept for retries of the same submission only). */
export function newIdempotencyKey(random: () => string = () => globalThis.crypto.randomUUID()): string {
  return random();
}

export const SLUG_PATTERN = /^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$/;

export function suggestSlug(name: string): string {
  return name
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 63)
    .replace(/-+$/g, "");
}

export interface OrganizationForm {
  name: string;
  slug: string;
  description: string;
  industry: string;
  organizationType: string;
  publicContactEmail: string;
  city: string;
  countryCode: string;
  operatingAddress: string;
  registeredName: string;
  registrationNumber: string;
  incorporatedOn: string;
}

const text = (v: string | null | undefined) => v ?? "";

export function organizationForm(org: Pick<BusinessOrganization, "name" | "slug" | "profile"> | null): OrganizationForm {
  const p = org?.profile ?? {};
  const legal = p.legalRegistration ?? {};
  return {
    name: text(org?.name),
    slug: text(org?.slug),
    description: text(p.description),
    industry: text(p.industry),
    organizationType: text(p.organizationType),
    publicContactEmail: text(p.publicContactEmail),
    city: text(p.city),
    countryCode: text(p.countryCode),
    operatingAddress: text(p.operatingAddress),
    registeredName: text(legal.registeredName),
    registrationNumber: text(legal.registrationNumber),
    incorporatedOn: text(legal.incorporatedOn),
  };
}

/** Client checks mirroring the documented bounds; the server stays authoritative. */
export function organizationFormProblems(form: OrganizationForm): Partial<Record<keyof OrganizationForm, string>> {
  const p: Partial<Record<keyof OrganizationForm, string>> = {};
  const len = (v: string) => [...v.trim()].length;
  if (len(form.name) < 1 || len(form.name) > 200) p.name = "1–200 characters.";
  if (!SLUG_PATTERN.test(form.slug.trim())) p.slug = "3–63 lowercase letters, digits or hyphens (not at the ends).";
  if (len(form.description) > 2000) p.description = "At most 2000 characters.";
  if (len(form.industry) > 60) p.industry = "At most 60 characters.";
  if (len(form.organizationType) > 60) p.organizationType = "At most 60 characters.";
  const email = form.publicContactEmail.trim();
  if (email && (email.length > 254 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email))) p.publicContactEmail = "A valid email address.";
  if (len(form.city) > 120) p.city = "At most 120 characters.";
  const cc = form.countryCode.trim();
  if (cc && !/^[A-Za-z]{2}$/.test(cc)) p.countryCode = "A two-letter ISO country code, e.g. GB.";
  if (len(form.operatingAddress) > 500) p.operatingAddress = "At most 500 characters.";
  if (len(form.registeredName) > 200) p.registeredName = "At most 200 characters.";
  if (len(form.registrationNumber) > 100) p.registrationNumber = "At most 100 characters.";
  const inc = form.incorporatedOn.trim();
  if (inc && !/^\d{4}-\d{2}-\d{2}$/.test(inc)) p.incorporatedOn = "A date (YYYY-MM-DD).";
  return p;
}

/** The profile document of a form (empty → null; legal `verification` is read-only and never sent). */
export function profileOfForm(form: OrganizationForm): OrganizationProfile {
  const v = (s: string) => (s.trim() === "" ? null : s.trim());
  const registeredName = v(form.registeredName);
  const registrationNumber = v(form.registrationNumber);
  const incorporatedOn = v(form.incorporatedOn);
  return {
    description: v(form.description),
    industry: v(form.industry),
    organizationType: v(form.organizationType),
    publicContactEmail: v(form.publicContactEmail),
    city: v(form.city),
    countryCode: v(form.countryCode)?.toUpperCase() ?? null,
    operatingAddress: v(form.operatingAddress),
    legalRegistration:
      registeredName || registrationNumber || incorporatedOn ? { registeredName, registrationNumber, incorporatedOn } : null,
  };
}

/**
 * `PATCH /api/v2/organizations/{id}` body: `expectedVersion` plus only what changed.
 * Returns null when nothing changed.
 */
export function organizationPatch(org: BusinessOrganization, form: OrganizationForm): UpdateBusinessOrganizationRequest | null {
  const patch: UpdateBusinessOrganizationRequest = { expectedVersion: org.version };
  if (form.name.trim() !== org.name) patch.name = form.name.trim();
  if (form.slug.trim() !== org.slug) patch.slug = form.slug.trim();
  const before = JSON.stringify(profileOfForm(organizationForm(org)));
  const next = profileOfForm(form);
  if (JSON.stringify(next) !== before) patch.profile = next;
  return Object.keys(patch).length > 1 ? patch : null;
}

/** ORG-003: legal fields are self-declared unless an operator verified them. */
export function legalVerificationLabel(verification: string | null | undefined): string {
  return (verification ?? "").toUpperCase() === "VERIFIED" ? "Verified by CF Meet" : "Self-declared (not verified)";
}

export function provisioningLabel(state: string | null | undefined): string {
  switch ((state ?? "").toUpperCase()) {
    case "NOT_REQUIRED":
      return "Not required";
    case "PENDING":
      return "Pending";
    case "IN_PROGRESS":
      return "In progress";
    case "READY":
      return "Ready";
    case "FAILED":
      return "Failed — the CF Meet team has been notified";
    default:
      return "Unknown";
  }
}

/** Whether the provisioning read should keep polling. */
export function provisioningSettled(state: string | null | undefined): boolean {
  return ["NOT_REQUIRED", "READY", "FAILED"].includes((state ?? "").toUpperCase());
}

/** Lifecycle actions the documented state machine allows (ACTIVE → SUSPENDED → ACTIVE | ARCHIVED). */
export function lifecycleActions(status: string | null | undefined): Array<"suspend" | "reactivate" | "archive"> {
  switch ((status ?? "").toUpperCase()) {
    case "ACTIVE":
      return ["suspend"];
    case "SUSPENDED":
      return ["reactivate", "archive"];
    default:
      return [];
  }
}

/** Short, non-identifying label for an id the API serves without a display name. */
export function shortId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

/** Pending-invitation token from the link fragment (`/organizations/invitations#<token>`). */
export function invitationTokenFromHash(hash: string | null | undefined): string | null {
  const raw = (hash ?? "").replace(/^#/, "").trim();
  if (!raw || raw.length > 512 || !/^[A-Za-z0-9_\-.~]+$/.test(raw)) return null;
  return raw;
}

/** Same-origin return paths the sign-in page may follow (open-redirect safe). */
const RETURN_PATHS = [/^\/organizations\/invitations$/, /^\/dashboard(\/[A-Za-z0-9_\-/]*)?$/];

export function safeReturnPath(value: string | null | undefined): string | null {
  if (!value || value.length > 200 || !value.startsWith("/") || value.startsWith("//")) return null;
  return RETURN_PATHS.some((p) => p.test(value)) ? value : null;
}

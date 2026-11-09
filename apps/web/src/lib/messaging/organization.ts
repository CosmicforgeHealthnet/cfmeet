/**
 * Organization-scope Messaging (final closure, J-10): pure rules shared by the client and
 * its tests. Organization conversations live in the organization's OWN Messaging scope
 * behind `/api/v2/organizations/{id}/messaging/...`; they never mix with personal
 * conversations, and they are MANAGED only (readable by CF Meet's servers, labelled so).
 */

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function isOrganizationId(value: unknown): value is string {
  return typeof value === "string" && UUID.test(value);
}

/** Base path (under `/api/v2`) of one organization's Messaging operations. */
export function organizationMessagingBase(organizationId: string): string {
  if (!isOrganizationId(organizationId)) throw new Error("Not an organization id");
  return `/organizations/${organizationId}/messaging`;
}

/** The relay path of a Messaging operation (the caller checks the allowlist first). */
export function organizationRelayPath(organizationId: string, path: string): string {
  if (path.startsWith("/") || path.includes("..") || !path.startsWith("v1/")) throw new Error("Not a Messaging relay path");
  return `${organizationMessagingBase(organizationId)}/relay/${path}`;
}

/** What to tell a member when the organization refuses Messaging (never a raw code). */
export function organizationMessagingProblem(code: string | null | undefined, status = 0): string | null {
  switch (code) {
    case "ORGANIZATION_POLICY_RESTRICTED":
      return "Messaging is turned off by your organization.";
    case "FEATURE_DISABLED":
      return "Organization messaging isn't set up for this organization yet. Ask an administrator.";
    case "ORGANIZATION_SUSPENDED":
      return "This organization is suspended. Its conversations are unavailable until it is reactivated.";
    case "PROVISIONING_PENDING":
      return "This organization is still being set up. Try again in a few minutes.";
    case "ORGANIZATION_NOT_FOUND":
    case "MEMBERSHIP_REQUIRED":
    case "PERMISSION_DENIED":
      return "You are no longer a member of this organization.";
    case "SERVICE_UNAVAILABLE":
    case "CHAT_UNAVAILABLE":
      return "Messaging is temporarily unavailable. Please try again.";
    default:
      return status === 401 ? "Your session has expired. Sign in again to continue." : null;
  }
}

export type OrganizationMember = { userId: string; displayName?: string | null; username?: string | null; status?: string };

/** Colleagues that can be added to an organization conversation: ACTIVE members other than me. */
export function addableColleagues(members: readonly OrganizationMember[], me: string | null): OrganizationMember[] {
  return members.filter((m) => m.userId !== me && (m.status ?? "ACTIVE") === "ACTIVE");
}

export function colleagueLabel(m: OrganizationMember): string {
  return m.displayName || m.username || "Colleague";
}

/**
 * Workspace context rules of the web app (Blueprint v1.0 Section 8 / Section 17).
 *
 * A workspace is `personal` or an organization the caller is an ACTIVE member of
 * (`GET /api/v2/me/workspaces`). The selection is a client preference of one browser tab:
 * it is never sent to a "switch" endpoint (none exists, ADR 0056 §5) and never persisted
 * server-side. Organization-scoped calls keep naming their organization explicitly; the
 * server re-derives capabilities on every request (`/me/workspaces/{id}/capabilities`).
 *
 * Pure: type-only imports, so node tests run it directly (tests/account-workspace.test.mjs).
 */
import type {
  Workspace,
  WorkspaceCapabilities,
  WorkspaceCapabilityReason,
  WorkspaceFeatures,
} from "../../types/generated/meeting-service";

export const PERSONAL_WORKSPACE_ID = "personal";
/** sessionStorage: this tab's selection. */
export const TAB_WORKSPACE_KEY = "cfmeet.workspace.tab";
/** localStorage: the default a new tab starts from (a client preference only). */
export const DEFAULT_WORKSPACE_KEY = "cfmeet.workspace.default";
/**
 * Every query that belongs to one workspace context starts with this key
 * (`[WORKSPACE_QUERY_ROOT, workspaceId, ...]`) so it can be dropped when the membership
 * disappears or the user switches context.
 */
export const WORKSPACE_QUERY_ROOT = "workspace";

export type CapabilityDecision = { granted: boolean; reason: WorkspaceCapabilityReason | "UNKNOWN" };

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Whether a stored workspace id is syntactically acceptable (`personal` or a UUID). */
export function isWorkspaceId(value: unknown): value is string {
  return typeof value === "string" && (value === PERSONAL_WORKSPACE_ID || UUID.test(value));
}

/**
 * Only workspaces that can be selected: Personal and ACTIVE organizations. A SUSPENDED
 * organization is listed by the server (so its owners see the status) but grants nothing,
 * so it is shown as unavailable rather than selected.
 */
export function selectableWorkspaces(items: readonly Workspace[]): Workspace[] {
  return items.filter(
    (w) => w.kind === "PERSONAL" || (w.kind === "ORGANIZATION" && (w.organizationStatus ?? "ACTIVE") === "ACTIVE"),
  );
}

/**
 * The workspace a tab should use: the requested one when it is still selectable,
 * otherwise Personal (a removed membership or a suspended organization falls back).
 */
export function resolveWorkspaceId(requested: string | null | undefined, items: readonly Workspace[] | null | undefined): string {
  if (!requested || !isWorkspaceId(requested) || requested === PERSONAL_WORKSPACE_ID) return PERSONAL_WORKSPACE_ID;
  if (!items) return requested; // list still loading: keep the tab's choice
  return selectableWorkspaces(items).some((w) => w.id === requested) ? requested : PERSONAL_WORKSPACE_ID;
}

/** Workspace ids present before and missing now: their cached data must be dropped. */
export function removedWorkspaceIds(previous: readonly Workspace[] | null | undefined, next: readonly Workspace[]): string[] {
  if (!previous) return [];
  const still = new Set(selectableWorkspaces(next).map((w) => w.id));
  return selectableWorkspaces(previous)
    .map((w) => w.id)
    .filter((id) => id !== PERSONAL_WORKSPACE_ID && !still.has(id));
}

/** The decision for one capability; unknown capabilities are denied (default deny). */
export function capabilityDecision(
  capabilities: Pick<WorkspaceCapabilities, "capabilities"> | null | undefined,
  name: string,
): CapabilityDecision {
  const found = capabilities?.capabilities.find((c) => c.capability === name);
  if (!found) return { granted: false, reason: "UNKNOWN" };
  return { granted: found.granted === true, reason: found.reason };
}

/** Human explanation of a denial reason (never claims a permission the server did not give). */
export function capabilityReasonText(reason: CapabilityDecision["reason"]): string {
  switch (reason) {
    case "ALLOWED":
      return "Allowed.";
    case "CAPABILITY_NOT_GRANTED":
      return "Your role in this organization does not include this.";
    case "POLICY_RESTRICTED":
      return "Your organization's policy turns this off for your role.";
    case "ORGANIZATION_SUSPENDED":
      return "This organization is suspended.";
    case "FEATURE_DISABLED":
      return "This feature is not enabled for this workspace.";
    default:
      return "Not available in this workspace.";
  }
}

export type NavigationFeature = keyof WorkspaceFeatures;

export interface WorkspaceAwareNavItem {
  /** Hidden when the workspace's features answer turns this feature off. */
  feature?: NavigationFeature;
  /** Shown only when the capability is granted in the selected workspace. */
  capability?: string;
  /** Shown only in an organization workspace. */
  organizationOnly?: boolean;
}

/**
 * Navigation driven by the server's capabilities read. While capabilities are loading or
 * unavailable, feature items stay visible (owner services still enforce access) and
 * capability-gated items stay hidden (default deny).
 */
export function visibleNavigation<T extends WorkspaceAwareNavItem>(
  items: readonly T[],
  context: { kind: "PERSONAL" | "ORGANIZATION"; capabilities: WorkspaceCapabilities | null },
): T[] {
  return items.filter((item) => {
    if (item.organizationOnly && context.kind !== "ORGANIZATION") return false;
    if (item.feature && context.capabilities && context.capabilities.features[item.feature] === false) return false;
    if (item.capability && !capabilityDecision(context.capabilities, item.capability).granted) return false;
    return true;
  });
}

/** Capabilities that make the organization administration area worth showing. */
export const ORGANIZATION_ADMIN_CAPABILITIES = [
  "organization.profile.write",
  "organization.members.invite",
  "organization.members.manage",
  "organization.audit.read",
  "organization.policies.manage",
  "organization.teams.manage",
  "organization.lifecycle.manage",
  "organization.ownership.transfer",
] as const;

export function canAdministerOrganization(capabilities: WorkspaceCapabilities | null | undefined): boolean {
  if (!capabilities || capabilities.kind !== "ORGANIZATION") return false;
  return ORGANIZATION_ADMIN_CAPABILITIES.some((c) => capabilityDecision(capabilities, c).granted);
}

/**
 * Developer workspace role -> allowed console actions.
 *
 * A mirror of the Developer Platform's single authorization table
 * (services/developer-platform/src/domain/rbac.rs, Section 50 §6.1 / §6.2) used ONLY to
 * hide actions a member cannot perform. The server re-checks the role on every call and
 * its 403 is always handled; this table never grants anything.
 *
 * Roles are Developer Platform roles (OWNER, ADMIN, DEVELOPER, ANALYST, BILLING). They are
 * never inferred from a first-party CF Meet organization role.
 *
 * Pure module (node tests load it with --experimental-strip-types).
 */
import type { OrganizationState, Role } from "./types";

export type ConsoleAction =
  | "OrganizationRead"
  | "OrganizationUpdate"
  | "OrganizationClose"
  | "MembersRead"
  | "MembersManage"
  | "OwnershipTransfer"
  | "ProjectsRead"
  | "ProjectsManage"
  | "EnvironmentsManage"
  | "WebhooksRead"
  | "WebhooksManage"
  | "UsageRead"
  | "UsageExport"
  | "LimitsRead"
  | "ActivityRead"
  | "CommercialRead"
  | "BillingAccountManage"
  | "ProductionAccessRequest"
  | "CredentialsManage"
  | "ServiceAccountsRead"
  | "ServiceAccountsManage"
  | "ServiceAccountsSuspend"
  | "CredentialsRead"
  | "CredentialsRevoke"
  | "ProductionAccessRead"
  | "SupportProfileManage";

const ALL: Role[] = ["OWNER", "ADMIN", "DEVELOPER", "ANALYST", "BILLING"];
const OWNER: Role[] = ["OWNER"];
const OWNER_ADMIN: Role[] = ["OWNER", "ADMIN"];
const BUILDERS: Role[] = ["OWNER", "ADMIN", "DEVELOPER"];
const OBSERVERS: Role[] = ["OWNER", "ADMIN", "DEVELOPER", "ANALYST"];

export const ROLE_MATRIX: Record<ConsoleAction, Role[]> = {
  OrganizationRead: ALL,
  MembersRead: ALL,
  ProjectsRead: ALL,
  OrganizationUpdate: OWNER_ADMIN,
  OrganizationClose: OWNER,
  MembersManage: OWNER,
  OwnershipTransfer: OWNER,
  ProductionAccessRequest: OWNER,
  ProjectsManage: OWNER_ADMIN,
  EnvironmentsManage: OWNER_ADMIN,
  ServiceAccountsManage: OWNER_ADMIN,
  ServiceAccountsSuspend: OWNER_ADMIN,
  ServiceAccountsRead: BUILDERS,
  CredentialsManage: BUILDERS,
  WebhooksManage: BUILDERS,
  CredentialsRead: BUILDERS,
  CredentialsRevoke: BUILDERS,
  ProductionAccessRead: ALL,
  SupportProfileManage: OWNER_ADMIN,
  WebhooksRead: OBSERVERS,
  LimitsRead: OBSERVERS,
  UsageRead: ALL,
  UsageExport: ALL,
  ActivityRead: ["OWNER", "ADMIN", "ANALYST"],
  CommercialRead: ["OWNER", "ADMIN", "BILLING"],
  BillingAccountManage: ["OWNER", "BILLING"],
};

const READS: ConsoleAction[] = [
  "OrganizationRead",
  "MembersRead",
  "ProjectsRead",
  "WebhooksRead",
  "UsageRead",
  "UsageExport",
  "LimitsRead",
  "ActivityRead",
  "CommercialRead",
  "ServiceAccountsRead",
  "CredentialsRead",
  "ProductionAccessRead",
];

const ALLOWED_WHILE_RESTRICTED: ConsoleAction[] = [
  "MembersManage",
  "BillingAccountManage",
  "OrganizationClose",
  "ServiceAccountsSuspend",
  "CredentialsRevoke",
];

export function isMutation(action: ConsoleAction): boolean {
  return !READS.includes(action);
}

export function roleAllows(role: Role | null | undefined, action: ConsoleAction): boolean {
  if (!role) return false;
  return ROLE_MATRIX[action].includes(role);
}

/** Section 50 §6.2: what the workspace lifecycle state permits. */
export function statePermits(state: OrganizationState | null | undefined, action: ConsoleAction): boolean {
  switch (state) {
    case "ACTIVE":
      return true;
    case "RESTRICTED":
      return !isMutation(action) || ALLOWED_WHILE_RESTRICTED.includes(action);
    case "SUSPENDED":
    case "CLOSED":
      return !isMutation(action);
    default:
      return false;
  }
}

/** Whether to SHOW a control for `action` in `workspace` (the server still decides). */
export function can(
  workspace: { myRole: Role | null; state: OrganizationState } | null | undefined,
  action: ConsoleAction,
): boolean {
  if (!workspace) return false;
  return roleAllows(workspace.myRole, action) && statePermits(workspace.state, action);
}

/** Everything `role` may do in an ACTIVE workspace (for the role help table). */
export function actionsFor(role: Role): ConsoleAction[] {
  return (Object.keys(ROLE_MATRIX) as ConsoleAction[]).filter((action) => ROLE_MATRIX[action].includes(role));
}

export const ROLE_DESCRIPTIONS: Record<Role, string> = {
  OWNER: "Workspace ownership, members and invitations, production access, billing reference and all developer configuration.",
  ADMIN: "Projects, environments, service accounts, credentials, scopes, webhooks and usage configuration. No membership or ownership changes.",
  DEVELOPER: "Integration work: service accounts (read), credentials, webhooks, SDKs and usage reads.",
  ANALYST: "Read-only usage, limits, delivery history and activity.",
  BILLING: "Billing-account reference, commercial plan visibility and usage export.",
};

/**
 * Member-row controls (display only; the server decides): owners change roles, remove
 * members and transfer ownership; anyone may leave (self-removal). The last owner cannot
 * leave or be demoted (server answers 409 LAST_OWNER), so it is not offered.
 */
export function memberActions(
  workspace: { myRole: Role | null; state: OrganizationState } | null | undefined,
  member: { userId: string; role: Role; status: string },
  viewerUserId: string | null | undefined,
  ownerCount: number,
): { changeRole: boolean; remove: boolean; leave: boolean; transferTo: boolean } {
  const none = { changeRole: false, remove: false, leave: false, transferTo: false };
  if (!workspace || member.status !== "ACTIVE") return none;
  const self = Boolean(viewerUserId) && member.userId === viewerUserId;
  const lastOwner = member.role === "OWNER" && ownerCount <= 1;
  const manage = can(workspace, "MembersManage");
  return {
    changeRole: manage && !lastOwner,
    remove: manage && !self && !lastOwner,
    leave: self && !lastOwner && statePermits(workspace.state, "MembersManage"),
    transferTo: can(workspace, "OwnershipTransfer") && !self && member.role !== "OWNER",
  };
}

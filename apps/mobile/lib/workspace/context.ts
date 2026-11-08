/**
 * Workspace context (ORG-016, Section 17 UI security). Pure; runs under `node --test`.
 *
 * The server stores no "active organization": every organization-scoped call names its
 * organization. The app keeps the user's chosen context locally and reconciles it with
 * `GET /api/v2/me/workspaces` on every refresh. When the chosen organization is no longer in
 * the list (membership removed, organization gone), the app falls back to Personal and must
 * drop every cached response of that context.
 */
import type { Workspace } from "@/lib/api/generated/meeting-service";

export const PERSONAL_WORKSPACE_ID = "personal";

export interface Reconciled {
  /** The context to use now. */
  workspaceId: string;
  /** The previously selected organization context that disappeared, if any. */
  removedWorkspaceId: string | null;
}

export function reconcileWorkspace(selectedId: string | null | undefined, workspaces: Pick<Workspace, "id">[]): Reconciled {
  const selected = selectedId || PERSONAL_WORKSPACE_ID;
  if (selected === PERSONAL_WORKSPACE_ID) return { workspaceId: PERSONAL_WORKSPACE_ID, removedWorkspaceId: null };
  if (workspaces.some((w) => w.id === selected)) return { workspaceId: selected, removedWorkspaceId: null };
  return { workspaceId: PERSONAL_WORKSPACE_ID, removedWorkspaceId: selected };
}

/** The organization id to pass to organization-scoped calls, or undefined for Personal. */
export function organizationIdOf(workspace: Pick<Workspace, "kind" | "organizationId"> | null | undefined): string | undefined {
  if (!workspace || workspace.kind !== "ORGANIZATION") return undefined;
  return workspace.organizationId ?? undefined;
}

/** Every per-context query key starts with this prefix, so a context can be purged at once. */
export function contextKey(workspaceId: string): readonly ["ctx", string] {
  return ["ctx", workspaceId] as const;
}

/** True when a query key belongs to `workspaceId`'s context. */
export function isContextKey(queryKey: readonly unknown[], workspaceId: string): boolean {
  return queryKey[0] === "ctx" && queryKey[1] === workspaceId;
}

/** Whether a workspace grants nothing right now (suspended organization). */
export function isSuspended(workspace: Pick<Workspace, "organizationStatus">): boolean {
  return (workspace.organizationStatus ?? "").toUpperCase() === "SUSPENDED";
}

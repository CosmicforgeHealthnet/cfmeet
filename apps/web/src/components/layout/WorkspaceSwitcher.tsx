"use client";

import Link from "next/link";
import { Building2, Settings2, User } from "lucide-react";
import { useWorkspace } from "@/context/WorkspaceContext";
import { PERSONAL_WORKSPACE_ID, canAdministerOrganization } from "@/lib/workspace/workspace-core";
import { cn } from "@/lib/utils";

/**
 * Workspace switcher: Personal plus ACTIVE organizations. The choice applies to this tab
 * only; nothing is sent to the server when it changes.
 */
export default function WorkspaceSwitcher({ expanded }: { expanded: boolean }) {
  const { workspaceId, kind, organizationId, capabilities, workspaces, selectable, selectWorkspace, listError, isLoading } = useWorkspace();
  // "Manage organization" only when the server granted an administration capability here.
  const manageHref =
    organizationId && canAdministerOrganization(capabilities) ? `/dashboard/organizations/${encodeURIComponent(organizationId)}` : null;
  const suspended = workspaces.filter((w) => w.kind === "ORGANIZATION" && w.organizationStatus === "SUSPENDED");
  const Icon = kind === "PERSONAL" ? User : Building2;

  if (!expanded) {
    const current = workspaces.find((w) => w.id === workspaceId);
    return (
      <div
        className="flex w-full justify-center text-muted-foreground"
        title={`Workspace: ${current?.displayName ?? "Personal"}`}
        aria-label={`Workspace: ${current?.displayName ?? "Personal"}`}
      >
        <Icon className="h-5 w-5" />
      </div>
    );
  }

  return (
    <div className="grid gap-1">
      <label className="grid gap-1 text-xs font-medium text-muted-foreground">
        Workspace
        <select
          aria-label="Workspace"
          className={cn("h-9 w-full rounded-md border border-input bg-background px-2 text-sm text-foreground")}
          value={workspaceId}
          disabled={isLoading && selectable.length === 0}
          onChange={(event) => selectWorkspace(event.target.value)}
        >
          {selectable.length === 0 && <option value={PERSONAL_WORKSPACE_ID}>Personal</option>}
          {selectable.map((w) => (
            <option key={w.id} value={w.id}>
              {w.kind === "PERSONAL" ? "Personal" : w.displayName}
            </option>
          ))}
          {suspended.map((w) => (
            <option key={w.id} value={w.id} disabled>
              {w.displayName} (suspended)
            </option>
          ))}
        </select>
        {listError ? <span role="status">Organization workspaces are temporarily unavailable.</span> : null}
      </label>
      {manageHref && (
        <Link href={manageHref} className="mt-1 inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline">
          <Settings2 className="h-3.5 w-3.5" /> Manage organization
        </Link>
      )}
    </div>
  );
}

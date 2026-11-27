"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { Check, ChevronsUpDown, Settings2 } from "lucide-react";
import { useWorkspace } from "@/context/WorkspaceContext";
import { useProfile } from "@/hooks";
import { PERSONAL_WORKSPACE_ID, canAdministerOrganization } from "@/lib/workspace/workspace-core";
import { cn } from "@/lib/utils";

/** Personal = your own round avatar (initials); organizations = a square badge. */
function Badge({ name, kind }: { name: string; kind: "PERSONAL" | "ORGANIZATION" }) {
  const { data: user } = useProfile();
  const personal = kind === "PERSONAL";
  const text = personal ? (user?.username ?? "Me").slice(0, 2).toUpperCase() : name.slice(0, 1).toUpperCase();
  return (
    <span
      className={cn(
        "flex size-9 shrink-0 items-center justify-center text-sm font-bold",
        personal ? "rounded-full bg-primary/15 text-primary" : "rounded-lg bg-violet-600 text-white",
      )}
      aria-hidden
    >
      {text}
    </span>
  );
}

/**
 * Workspace switcher: Personal plus ACTIVE organizations. The choice applies to this tab
 * only; nothing is sent to the server when it changes.
 */
export default function WorkspaceSwitcher({ expanded }: { expanded: boolean }) {
  const { workspaceId, kind, organizationId, capabilities, workspaces, selectable, selectWorkspace, listError } = useWorkspace();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  // "Manage organization" only when the server granted an administration capability here.
  const manageHref =
    organizationId && canAdministerOrganization(capabilities) ? `/dashboard/organizations/${encodeURIComponent(organizationId)}` : null;
  const suspended = workspaces.filter((w) => w.kind === "ORGANIZATION" && w.organizationStatus === "SUSPENDED");
  const current = workspaces.find((w) => w.id === workspaceId);
  const name = kind === "PERSONAL" || !current ? "Personal" : current.displayName;
  const options: { id: string; kind: string; displayName: string }[] = selectable.length
    ? selectable
    : [{ id: PERSONAL_WORKSPACE_ID, kind: "PERSONAL", displayName: "Personal" }];

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Workspace: ${name}. Switch workspace`}
        title={expanded ? undefined : `Workspace: ${name}`}
        className={cn(
          "flex w-full items-center gap-3 rounded-xl text-left transition-colors hover:bg-muted",
          expanded ? "p-2" : "justify-center p-1.5",
        )}
      >
        <Badge name={name} kind={kind === "PERSONAL" ? "PERSONAL" : "ORGANIZATION"} />
        {expanded && (
          <>
            <span className="min-w-0 flex-1 leading-tight">
              <span className="block truncate text-sm font-semibold text-foreground">{name}</span>
              <span className="block text-xs text-muted-foreground">{kind === "PERSONAL" ? "Personal workspace" : "Organization"}</span>
            </span>
            <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          </>
        )}
      </button>

      {open && (
        <div
          role="listbox"
          aria-label="Workspaces"
          className={cn(
            "absolute z-50 mt-2 w-64 rounded-xl border border-border bg-popover p-1.5 shadow-xl",
            expanded ? "left-0" : "left-full top-0 ml-2 mt-0",
          )}
        >
          <p className="px-2.5 pb-1 pt-1.5 text-xs font-medium text-muted-foreground">Workspaces</p>
          {options.map((w) => {
            const label = w.kind === "PERSONAL" ? "Personal" : w.displayName;
            const selected = w.id === workspaceId;
            return (
              <button
                key={w.id}
                type="button"
                role="option"
                aria-selected={selected}
                onClick={() => {
                  selectWorkspace(w.id);
                  setOpen(false);
                }}
                className="flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left text-sm hover:bg-muted"
              >
                <Badge name={label} kind={w.kind === "PERSONAL" ? "PERSONAL" : "ORGANIZATION"} />
                <span className="min-w-0 flex-1 truncate font-medium text-foreground">{label}</span>
                {selected && <Check className="size-4 text-primary" aria-hidden />}
              </button>
            );
          })}
          {suspended.map((w) => (
            <div key={w.id} className="flex items-center gap-3 px-2.5 py-2 text-sm text-muted-foreground opacity-70">
              <Badge name={w.displayName} kind="ORGANIZATION" />
              <span className="truncate">{w.displayName} (suspended)</span>
            </div>
          ))}
          {listError ? <p className="px-2.5 py-2 text-xs text-muted-foreground">Organization workspaces are temporarily unavailable.</p> : null}
          {manageHref && (
            <Link
              href={manageHref}
              onClick={() => setOpen(false)}
              className="mt-1 flex items-center gap-2 border-t border-border px-2.5 pb-1.5 pt-2.5 text-sm font-medium text-primary hover:underline"
            >
              <Settings2 className="size-4" /> Manage organization
            </Link>
          )}
        </div>
      )}
    </div>
  );
}

"use client";

/**
 * The Developer Portal console layout: its own sidebar and header (not the chat / meeting
 * dashboard), gated by deployment configuration and the CF Meet sign-in.
 */
import { useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ArrowLeft, Code2, Menu, PlugZap, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { DEVELOPER_API_CONFIG_REASONS } from "@/lib/developer-platform/config";
import { activeSection, PORTAL_BASE, visibleSections } from "@/lib/developer-platform/navigation";
import { DeveloperPortalProvider, useDeveloperPortal } from "./DeveloperPortalProvider";
import { ErrorNotice, Loading, StatusBadge } from "./ui";

const noop = () => () => {};

export default function PortalShell({ children }: { children: React.ReactNode }) {
  const mounted = useSyncExternalStore(noop, () => true, () => false);
  if (!mounted) return null;
  return (
    <DeveloperPortalProvider>
      <Gate>{children}</Gate>
    </DeveloperPortalProvider>
  );
}

function CenteredCard({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-6">
      <div className="w-full max-w-lg space-y-4 rounded-2xl border border-border bg-card p-8 shadow-sm">{children}</div>
    </div>
  );
}

function Gate({ children }: { children: React.ReactNode }) {
  const { config, signedIn } = useDeveloperPortal();

  if (!config.available) {
    return (
      <CenteredCard>
        <div className="flex items-center gap-2 text-lg font-semibold">
          <PlugZap className="h-5 w-5 text-amber-600" /> Developer Platform not available
        </div>
        <p className="text-sm text-muted-foreground">
          The CF Meet Developer Platform (API Gateway, developer workspaces, service accounts and keys) is not enabled in this environment
          yet, so the developer console cannot load. Nothing has been called.
        </p>
        <p className="rounded-lg bg-muted px-3 py-2 font-mono text-xs text-muted-foreground">{DEVELOPER_API_CONFIG_REASONS[config.reason]}</p>
        <p className="text-sm text-muted-foreground">
          Legacy API keys and meeting webhooks in Settings are deprecated and will not be replaced there; they move to this console once it is
          enabled.
        </p>
        <Button asChild variant="outline" size="sm">
          <Link href="/dashboard">
            <ArrowLeft className="h-4 w-4" /> Back to CF Meet
          </Link>
        </Button>
      </CenteredCard>
    );
  }

  if (!signedIn) {
    return (
      <CenteredCard>
        <div className="flex items-center gap-2 text-lg font-semibold">
          <Code2 className="h-5 w-5 text-primary" /> Develop with CF Meet
        </div>
        <p className="text-sm text-muted-foreground">
          Sign in with your CF Meet account to open the developer console. Your sign-in is checked by the Developer Platform; access inside a
          developer workspace depends on your developer role there, not on your CF Meet organization role.
        </p>
        <div className="flex gap-2">
          <Button asChild size="sm">
            <Link href="/login">Sign in</Link>
          </Button>
          <Button asChild size="sm" variant="outline">
            <Link href="/signup">Create an account</Link>
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">After signing in, come back to {PORTAL_BASE}.</p>
      </CenteredCard>
    );
  }

  return <Console>{children}</Console>;
}

function WorkspaceSwitcher() {
  const { workspaces, workspace, selectWorkspace } = useDeveloperPortal();
  if (workspaces.length === 0) return <p className="px-1 text-xs text-muted-foreground">No developer workspace yet</p>;
  return (
    <div className="space-y-1">
      <label htmlFor="dp-workspace" className="px-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Developer workspace
      </label>
      <select
        id="dp-workspace"
        className="w-full rounded-lg border border-border bg-background px-2 py-1.5 text-sm"
        value={workspace?.id ?? ""}
        onChange={(e) => selectWorkspace(e.target.value)}
      >
        {workspaces.map((w) => (
          <option key={w.id} value={w.id}>
            {w.displayName}
            {w.state !== "ACTIVE" ? ` (${w.state.toLowerCase()})` : ""}
          </option>
        ))}
      </select>
      {workspace && (
        <div className="flex items-center gap-2 px-1 text-xs text-muted-foreground">
          Role: <span className="font-medium text-foreground">{workspace.myRole ?? "none"}</span>
          {workspace.state !== "ACTIVE" && <StatusBadge status={workspace.state} />}
        </div>
      )}
    </div>
  );
}

function Sidebar({ onNavigate }: { onNavigate?: () => void }) {
  const pathname = usePathname();
  const { can } = useDeveloperPortal();
  const active = activeSection(pathname);
  return (
    <nav className="flex h-full flex-col gap-5 p-4" aria-label="Developer console">
      <Link href={PORTAL_BASE} className="flex items-center gap-2 px-1 font-semibold" onClick={onNavigate}>
        <Code2 className="h-5 w-5 text-primary" /> CF Meet Developers
      </Link>
      <WorkspaceSwitcher />
      <ul className="flex-1 space-y-0.5">
        {visibleSections(can).map((section) => (
          <li key={section.key}>
            <Link
              href={section.href}
              onClick={onNavigate}
              className={cn(
                "block rounded-lg px-2 py-1.5 text-sm",
                active === section.key ? "bg-primary/10 font-medium text-primary" : "text-muted-foreground hover:bg-muted hover:text-foreground",
              )}
            >
              {section.label}
            </Link>
          </li>
        ))}
      </ul>
      <Link href="/dashboard" className="flex items-center gap-2 px-1 text-sm text-muted-foreground hover:text-foreground">
        <ArrowLeft className="h-4 w-4" /> Back to CF Meet
      </Link>
    </nav>
  );
}

function Console({ children }: { children: React.ReactNode }) {
  const { workspacesLoading, workspacesError, refetchWorkspaces } = useDeveloperPortal();
  const [open, setOpen] = useState(false);
  return (
    <div className="flex h-screen bg-background">
      <aside className="hidden w-64 shrink-0 border-r border-border bg-card md:block">
        <Sidebar />
      </aside>
      {open && (
        <div className="fixed inset-0 z-40 flex md:hidden">
          <aside className="w-72 border-r border-border bg-card">
            <div className="flex justify-end p-2">
              <button type="button" aria-label="Close menu" onClick={() => setOpen(false)}>
                <X className="h-5 w-5" />
              </button>
            </div>
            <Sidebar onNavigate={() => setOpen(false)} />
          </aside>
          <button type="button" aria-label="Close menu" className="flex-1 bg-black/40" onClick={() => setOpen(false)} />
        </div>
      )}
      <main className="flex-1 overflow-y-auto">
        <div className="flex items-center gap-2 border-b border-border px-4 py-3 md:hidden">
          <button type="button" aria-label="Open menu" onClick={() => setOpen(true)}>
            <Menu className="h-5 w-5" />
          </button>
          <span className="font-semibold">CF Meet Developers</span>
        </div>
        <div className="mx-auto max-w-6xl space-y-6 p-4 md:p-8">
          {workspacesLoading ? (
            <Loading label="Loading your developer workspaces…" />
          ) : workspacesError ? (
            <ErrorNotice error={workspacesError} onRetry={refetchWorkspaces} />
          ) : (
            children
          )}
        </div>
      </main>
    </div>
  );
}

/** Renders `children` only when a developer workspace is selected; otherwise points to creation. */
export function RequireWorkspace({ children }: { children: React.ReactNode }) {
  const { workspace } = useDeveloperPortal();
  if (!workspace) {
    return (
      <div className="space-y-3 rounded-xl border border-dashed border-border p-8 text-center">
        <p className="font-medium">You are not a member of any developer workspace yet.</p>
        <p className="text-sm text-muted-foreground">Create one, or accept an invitation from a workspace owner.</p>
        <div className="flex justify-center gap-2">
          <Button asChild size="sm">
            <Link href={`${PORTAL_BASE}/workspace`}>Create a developer workspace</Link>
          </Button>
          <Button asChild size="sm" variant="outline">
            <Link href={`${PORTAL_BASE}/invitations/accept`}>Accept an invitation</Link>
          </Button>
        </div>
      </div>
    );
  }
  return <>{children}</>;
}

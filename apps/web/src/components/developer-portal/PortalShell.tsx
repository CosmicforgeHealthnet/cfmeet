"use client";

/**
 * The Developer Portal console layout: its own sidebar and header (not the chat / meeting
 * dashboard), gated by deployment configuration and the CF Meet sign-in.
 */
import { useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ArrowLeft, BarChart3, BookOpen, Building2, Code2, FolderKanban, KeyRound, LayoutDashboard, Menu, PlugZap, ScrollText, Users, Webhook, X } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { cn } from "@/lib/utils";
import { DEVELOPER_API_CONFIG_REASONS } from "@/lib/developer-platform/config";
import { activeSection, PORTAL_BASE, visibleSections } from "@/lib/developer-platform/navigation";
import { DeveloperPortalProvider, useDeveloperPortal } from "./DeveloperPortalProvider";
import { ErrorNotice, Loading, StatusBadge, Toaster } from "./ui";

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

function CenteredCard({ icon, children }: { icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-muted/40 p-6">
      <div className="w-full max-w-md space-y-5 rounded-3xl border border-border bg-card p-8 shadow-sm">
        <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">{icon}</span>
        {children}
      </div>
    </div>
  );
}

function Gate({ children }: { children: React.ReactNode }) {
  const { config, signedIn } = useDeveloperPortal();

  if (!config.available) {
    return (
      <CenteredCard icon={<PlugZap className="size-6" />}>
        <div>
          <h1 className="text-xl font-bold tracking-tight">The developer console isn&apos;t on yet</h1>
          <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
            It&apos;s where you&apos;ll manage API keys, webhooks and usage for apps built on CF Meet. It hasn&apos;t been switched on for this site, so nothing was loaded.
          </p>
        </div>
        <details className="rounded-xl bg-muted/60 px-3 py-2 text-xs text-muted-foreground">
          <summary className="cursor-pointer font-medium">Technical detail</summary>
          <p className="mt-1.5 font-mono">{DEVELOPER_API_CONFIG_REASONS[config.reason]}</p>
        </details>
        <Link href="/dashboard/developers" className="inline-flex h-10 items-center gap-2 rounded-full px-4 text-sm font-semibold ring-1 ring-border hover:bg-muted">
          <ArrowLeft className="size-4" /> Back to CF Meet
        </Link>
      </CenteredCard>
    );
  }

  if (!signedIn) {
    return (
      <CenteredCard icon={<Code2 className="size-6" />}>
        <div>
          <h1 className="text-xl font-bold tracking-tight">Sign in to the developer console</h1>
          <p className="mt-2 text-sm leading-relaxed text-muted-foreground">Use your CF Meet account. What you can do inside depends on your developer role, not your organization role.</p>
        </div>
        <div className="flex gap-2">
          <Link href="/login" className="inline-flex h-10 items-center rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Sign in</Link>
          <Link href="/signup" className="inline-flex h-10 items-center rounded-full px-5 text-sm font-semibold ring-1 ring-border hover:bg-muted">Create an account</Link>
        </div>
        <p className="text-xs text-muted-foreground">After signing in, come back to {PORTAL_BASE}.</p>
      </CenteredCard>
    );
  }

  return <Console>{children}</Console>;
}

const SECTION_ICONS: Record<string, typeof Code2> = {
  overview: LayoutDashboard,
  workspace: Building2,
  members: Users,
  projects: FolderKanban,
  credentials: KeyRound,
  webhooks: Webhook,
  usage: BarChart3,
  audit: ScrollText,
  docs: BookOpen,
};

function WorkspaceSwitcher() {
  const { workspaces, workspace, selectWorkspace } = useDeveloperPortal();
  if (workspaces.length === 0) return <p className="rounded-xl bg-muted/60 px-3 py-2.5 text-xs text-muted-foreground">No developer workspace yet</p>;
  return (
    <div className="space-y-1.5">
      <NiceSelect
        value={workspace?.id ?? ""}
        onChange={(id) => selectWorkspace(id)}
        options={workspaces.map((w) => ({ value: w.id, label: w.displayName, hint: w.state !== "ACTIVE" ? w.state.toLowerCase() : w.myRole ? `You're ${w.myRole.toLowerCase().replaceAll("_", " ")}` : undefined }))}
      />
      {workspace && workspace.state !== "ACTIVE" && <div className="px-1"><StatusBadge status={workspace.state} /></div>}
    </div>
  );
}

function Sidebar({ onNavigate }: { onNavigate?: () => void }) {
  const pathname = usePathname();
  const { can } = useDeveloperPortal();
  const active = activeSection(pathname);
  return (
    <nav className="flex h-full flex-col gap-5 p-4" aria-label="Developer console">
      <Link href={PORTAL_BASE} className="flex items-center gap-2.5 px-2 pt-1" onClick={onNavigate}>
        <span className="flex size-8 items-center justify-center rounded-lg bg-[#0B1220] text-sky-300"><Code2 className="size-4" /></span>
        <span className="text-[15px] font-bold tracking-tight">Developers</span>
      </Link>
      <WorkspaceSwitcher />
      <ul className="no-scrollbar flex-1 space-y-0.5 overflow-y-auto">
        {visibleSections(can).map((section) => {
          const Icon = SECTION_ICONS[section.key] ?? Code2;
          const on = active === section.key;
          return (
            <li key={section.key}>
              <Link href={section.href} onClick={onNavigate} aria-current={on ? "page" : undefined}
                className={cn("flex items-center gap-3 rounded-xl px-3 py-2.5 text-sm transition-colors", on ? "bg-primary/10 font-semibold text-primary" : "text-foreground hover:bg-muted")}>
                <Icon className="size-[18px] shrink-0" strokeWidth={on ? 2.4 : 2} />
                {section.label}
              </Link>
            </li>
          );
        })}
      </ul>
      <Link href="/dashboard/developers" className="flex items-center gap-2 rounded-xl px-3 py-2.5 text-sm text-muted-foreground hover:bg-muted hover:text-foreground">
        <ArrowLeft className="size-4" /> Back to CF Meet
      </Link>
    </nav>
  );
}

function Console({ children }: { children: React.ReactNode }) {
  const { workspacesLoading, workspacesError, refetchWorkspaces } = useDeveloperPortal();
  const [open, setOpen] = useState(false);
  return (
    <div className="flex h-dvh bg-background">
      <aside className="hidden w-64 shrink-0 border-r border-border bg-muted/30 md:block">
        <Sidebar />
      </aside>
      {open && (
        <div className="fixed inset-0 z-40 flex md:hidden">
          <aside className="w-72 bg-background shadow-2xl">
            <div className="flex justify-end p-2">
              <button type="button" aria-label="Close menu" onClick={() => setOpen(false)} className="flex size-10 items-center justify-center rounded-full hover:bg-muted">
                <X className="size-5" />
              </button>
            </div>
            <Sidebar onNavigate={() => setOpen(false)} />
          </aside>
          <button type="button" aria-label="Close menu" className="flex-1 bg-black/40" onClick={() => setOpen(false)} />
        </div>
      )}
      <Toaster />
      <main className="flex-1 overflow-y-auto">
        <div className="sticky top-0 z-30 flex h-14 items-center gap-2 border-b border-border bg-background/90 px-3 backdrop-blur md:hidden">
          <button type="button" aria-label="Open menu" onClick={() => setOpen(true)} className="flex size-10 items-center justify-center rounded-full hover:bg-muted">
            <Menu className="size-5" />
          </button>
          <span className="flex items-center gap-2 font-bold"><Code2 className="size-4 text-primary" />Developers</span>
        </div>
        <div className="mx-auto max-w-6xl space-y-6 px-4 py-6 md:px-8 md:py-8">
          {workspacesLoading ? (
            <Loading label="Loading your developer workspaces…" />
          ) : workspacesError ? (
            <ErrorNotice page error={workspacesError} onRetry={refetchWorkspaces} />
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
      <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-16 text-center">
        <span className="flex size-16 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Building2 className="size-7" /></span>
        <p className="mt-4 text-lg font-semibold">Create your developer workspace</p>
        <p className="mt-1 max-w-sm text-sm text-muted-foreground">It holds your projects, keys and webhooks. Or join a teammate&apos;s with an invitation.</p>
        <div className="mt-5 flex flex-wrap justify-center gap-2">
          <Link href={`${PORTAL_BASE}/invitations/accept`} className="inline-flex h-10 items-center rounded-full px-5 text-sm font-semibold ring-1 ring-border hover:bg-muted">Use an invitation</Link>
          <Link href={`${PORTAL_BASE}/workspace`} className="inline-flex h-10 items-center rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Create workspace</Link>
        </div>
      </div>
    );
  }
  return <>{children}</>;
}

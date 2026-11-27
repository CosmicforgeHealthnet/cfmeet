"use client";

import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { ArrowRight, BookOpen, Building2, Check, FolderKanban, KeyRound, Lock, ShieldCheck, UserCog, Webhook } from "lucide-react";
import { PORTAL_BASE } from "@/lib/developer-platform/navigation";
import { formatTimestamp, retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import { ROLE_DESCRIPTIONS } from "@/lib/developer-platform/permissions";
import type { Page, Project } from "@/lib/developer-platform/types";
import { cn } from "@/lib/utils";
import { useDeveloperPortal } from "../DeveloperPortalProvider";
import { ErrorNotice, PageHeader, StatusBadge } from "../ui";

const STEPS: { title: string; body: string; href: string; icon: typeof Building2 }[] = [
  { title: "Create a workspace", body: "Your company's home here. You become its owner.", href: `${PORTAL_BASE}/workspace`, icon: Building2 },
  { title: "Create a project", body: "One app or integration, with Sandbox and Test environments.", href: `${PORTAL_BASE}/projects`, icon: FolderKanban },
  { title: "Add a service account", body: "A login for your backend inside an environment.", href: `${PORTAL_BASE}/credentials`, icon: UserCog },
  { title: "Get an API key", body: "Scoped and expiring. Shown once, so keep it on your server.", href: `${PORTAL_BASE}/credentials`, icon: KeyRound },
  { title: "Add a webhook", body: "Signed events, with delivery history and replay.", href: `${PORTAL_BASE}/webhooks`, icon: Webhook },
  { title: "Read the docs", body: "API reference and SDK packages.", href: `${PORTAL_BASE}/docs`, icon: BookOpen },
];

const nice = (v?: string | null) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : "—");

export default function OverviewPage() {
  const { workspace, client } = useDeveloperPortal();
  const projects = useQuery({
    queryKey: ["developer-portal", workspace?.id, "projects-summary"],
    queryFn: () => client.call<Page<Project>>({ operation: "listProjects", path: { organizationId: workspace!.id }, query: { limit: 100 } }),
    enabled: Boolean(workspace),
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const projectCount = projects.data ? projects.data.items.length : 0;
  // Progress through the steps we can actually see: a workspace, then a project.
  const done = [!!workspace, projectCount > 0];

  return (
    <div className="space-y-8">
      <PageHeader title={workspace ? `Welcome back` : "Build on CF Meet"} description="Manage the projects, keys and webhooks your apps use to add meetings, chat and calling." />

      {workspace && (
        <section className="grid gap-3 sm:grid-cols-3">
          <div className="rounded-2xl border border-border bg-card p-5">
            <p className="text-sm text-muted-foreground">Workspace</p>
            <p className="mt-1 truncate text-lg font-semibold">{workspace.displayName}</p>
            <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground"><StatusBadge status={workspace.state} />since {formatTimestamp(workspace.createdAt)}</div>
            {workspace.stateReason && <p className="mt-2 text-xs text-muted-foreground">{workspace.stateReason}</p>}
          </div>
          <div className="rounded-2xl border border-border bg-card p-5">
            <p className="text-sm text-muted-foreground">Your role</p>
            <p className="mt-1 text-lg font-semibold">{nice(workspace.myRole)}</p>
            {workspace.myRole && <p className="mt-1 text-xs text-muted-foreground">{ROLE_DESCRIPTIONS[workspace.myRole]}</p>}
          </div>
          <Link href={`${PORTAL_BASE}/projects`} className="group rounded-2xl border border-border bg-card p-5 transition-colors hover:border-primary/40">
            <p className="text-sm text-muted-foreground">Projects</p>
            <p className="mt-1 text-3xl font-bold tabular-nums">{projects.isLoading ? "…" : `${projectCount}${projects.data?.nextCursor ? "+" : ""}`}</p>
            <p className="mt-1 flex items-center gap-1 text-xs font-semibold text-primary">View projects<ArrowRight className="size-3.5 transition-transform group-hover:translate-x-0.5" /></p>
          </Link>
          {projects.error ? <div className="sm:col-span-3"><ErrorNotice error={projects.error} onRetry={() => void projects.refetch()} /></div> : null}
        </section>
      )}

      <section>
        <h2 className="text-lg font-semibold">Getting started</h2>
        <ol className="mt-4 grid gap-3 md:grid-cols-2 lg:grid-cols-3">
          {STEPS.map(({ title, body, href, icon: Icon }, i) => {
            const complete = done[i] === true;
            return (
              <li key={title}>
                <Link href={href} className="group flex h-full items-start gap-4 rounded-2xl border border-border bg-card p-5 transition-all hover:border-primary/40 hover:shadow-md">
                  <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-xl", complete ? "bg-emerald-500/10 text-emerald-600" : "bg-primary/10 text-primary")}>
                    {complete ? <Check className="size-5" strokeWidth={2.5} /> : <Icon className="size-5" />}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="text-xs font-semibold text-muted-foreground">Step {i + 1}{complete ? " · Done" : ""}</span>
                    <span className="block font-semibold">{title}</span>
                    <span className="mt-0.5 block text-sm text-muted-foreground">{body}</span>
                  </span>
                </Link>
              </li>
            );
          })}
        </ol>
      </section>

      <section className="rounded-2xl bg-muted/50 p-5 sm:p-6">
        <h2 className="flex items-center gap-2 font-semibold"><ShieldCheck className="size-5 text-emerald-600" />How we keep keys safe</h2>
        <ul className="mt-3 grid gap-3 text-sm text-muted-foreground md:grid-cols-3">
          <li className="flex gap-2"><Lock className="mt-0.5 size-4 shrink-0" />This console uses your CF Meet sign-in. It never keeps an API key in your browser.</li>
          <li className="flex gap-2"><KeyRound className="mt-0.5 size-4 shrink-0" />Keys and webhook secrets are shown once. Store them in your server&apos;s secret manager.</li>
          <li className="flex gap-2"><UserCog className="mt-0.5 size-4 shrink-0" />Every action is checked against your role in this workspace.</li>
        </ul>
      </section>
    </div>
  );
}

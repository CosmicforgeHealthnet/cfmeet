"use client";

import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { ArrowRight } from "lucide-react";
import { PORTAL_BASE } from "@/lib/developer-platform/navigation";
import { formatTimestamp, retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import { ROLE_DESCRIPTIONS } from "@/lib/developer-platform/permissions";
import type { Page, Project } from "@/lib/developer-platform/types";
import { useDeveloperPortal } from "../DeveloperPortalProvider";
import { ErrorNotice, PageHeader, Section, StatusBadge } from "../ui";

const STEPS: { title: string; body: string; href: string }[] = [
  { title: "Create a developer workspace", body: "Your company's account on the Developer Platform. You become its owner.", href: `${PORTAL_BASE}/workspace` },
  { title: "Create a project", body: "One integration or application. Each project gets SANDBOX / TEST environments.", href: `${PORTAL_BASE}/projects` },
  { title: "Add a service account", body: "A machine identity inside an environment, for your backend.", href: `${PORTAL_BASE}/credentials` },
  { title: "Issue an API key", body: "Scoped, expiring credentials (cfmk_…). Shown once; keep them on your server only.", href: `${PORTAL_BASE}/credentials` },
  { title: "Subscribe to webhooks", body: "Signed event delivery with delivery history and replay.", href: `${PORTAL_BASE}/webhooks` },
  { title: "Read the API docs and SDKs", body: "The public API reference and SDK packages served by the API Gateway.", href: `${PORTAL_BASE}/docs` },
];

export default function OverviewPage() {
  const { workspace, client } = useDeveloperPortal();
  const projects = useQuery({
    queryKey: ["developer-portal", workspace?.id, "projects-summary"],
    queryFn: () => client.call<Page<Project>>({ operation: "listProjects", path: { organizationId: workspace!.id }, query: { limit: 100 } }),
    enabled: Boolean(workspace),
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });

  return (
    <div className="space-y-6">
      <PageHeader
        title="Develop with CF Meet"
        description="Build on CF Meet meetings, messaging and calling with your own backend. This console manages developer workspaces, projects, environments, service accounts, API keys, webhooks and usage."
      />

      {workspace && (
        <Section title={workspace.displayName} description={`Created ${formatTimestamp(workspace.createdAt)}`} actions={<StatusBadge status={workspace.state} />}>
          <dl className="grid gap-4 text-sm sm:grid-cols-3">
            <div>
              <dt className="text-muted-foreground">Your developer role</dt>
              <dd className="font-medium">{workspace.myRole ?? "—"}</dd>
              {workspace.myRole && <dd className="mt-1 text-xs text-muted-foreground">{ROLE_DESCRIPTIONS[workspace.myRole]}</dd>}
            </div>
            <div>
              <dt className="text-muted-foreground">Projects</dt>
              <dd className="font-medium">
                {projects.data ? `${projects.data.items.length}${projects.data.nextCursor ? "+" : ""}` : projects.isLoading ? "…" : "—"}
              </dd>
            </div>
            <div>
              <dt className="text-muted-foreground">Workspace state</dt>
              <dd className="font-medium">{workspace.state}</dd>
              {workspace.stateReason && <dd className="mt-1 text-xs text-muted-foreground">{workspace.stateReason}</dd>}
            </div>
          </dl>
          {projects.error ? (
            <div className="mt-4">
              <ErrorNotice error={projects.error} onRetry={() => void projects.refetch()} />
            </div>
          ) : null}
        </Section>
      )}

      <Section title="Getting started">
        <ol className="grid gap-3 md:grid-cols-2">
          {STEPS.map((step, i) => (
            <li key={step.title}>
              <Link href={step.href} className="group flex h-full gap-3 rounded-lg border border-border p-4 hover:border-primary">
                <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-primary/10 text-xs font-semibold text-primary">{i + 1}</span>
                <span className="flex-1">
                  <span className="flex items-center gap-1 font-medium">
                    {step.title} <ArrowRight className="h-3.5 w-3.5 opacity-0 transition-opacity group-hover:opacity-100" />
                  </span>
                  <span className="mt-0.5 block text-sm text-muted-foreground">{step.body}</span>
                </span>
              </Link>
            </li>
          ))}
        </ol>
      </Section>

      <Section title="Security model">
        <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">
          <li>This console uses your CF Meet sign-in. It never uses or stores an API key in the browser.</li>
          <li>API keys and webhook signing secrets are shown once, when created or rotated. Store them in your server&apos;s secret manager.</li>
          <li>Every action is checked by the Developer Platform against your developer role in the selected workspace.</li>
        </ul>
      </Section>
    </div>
  );
}

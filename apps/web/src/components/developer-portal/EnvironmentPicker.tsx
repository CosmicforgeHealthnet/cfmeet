"use client";

/**
 * Project + environment selector for environment-scoped screens (keys, webhooks, usage).
 * The selection is kept in the URL (`?project=&env=`): ids only, never a secret.
 * With exactly one project (or one environment) it is chosen automatically.
 */
import { useEffect } from "react";
import Link from "next/link";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { FlaskConical, FolderKanban, Rocket, TestTube2 } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { PORTAL_BASE, portalHref, safeId } from "@/lib/developer-platform/navigation";
import type { Environment, Project, Workspace } from "@/lib/developer-platform/types";
import { ErrorNotice, usePagedList } from "./ui";

const nice = (v: string) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : v);
const ENV_ICON: Record<string, typeof FlaskConical> = { SANDBOX: FlaskConical, TEST: TestTube2, PRODUCTION: Rocket };

export function useEnvironmentSelection() {
  const params = useSearchParams();
  return { projectId: safeId(params.get("project")), environmentId: safeId(params.get("env")) };
}

export function EnvironmentPicker({ workspace, onChange }: { workspace: Workspace; onChange?: () => void }) {
  const router = useRouter();
  const pathname = usePathname();
  const { projectId, environmentId } = useEnvironmentSelection();
  const projects = usePagedList<Project>(["developer-portal", workspace.id, "projects"], { operation: "listProjects", path: { organizationId: workspace.id } });
  const environments = usePagedList<Environment>(["developer-portal", workspace.id, "environments", projectId], { operation: "listEnvironments", path: { projectId: projectId ?? "_" } }, Boolean(projectId));
  const go = (query: Record<string, string | null>) => {
    onChange?.();
    router.replace(portalHref(pathname, query));
  };

  // One choice is no choice: pick it for the user.
  useEffect(() => {
    if (!projectId && !projects.isLoading && projects.items.length === 1) go({ project: projects.items[0].id, env: null });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, projects.isLoading, projects.items.length]);
  useEffect(() => {
    if (projectId && !environmentId && !environments.isLoading && environments.items.length === 1) go({ project: projectId, env: environments.items[0].id });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, environmentId, environments.isLoading, environments.items.length]);

  if (!projects.isLoading && !projects.error && projects.items.length === 0) {
    return (
      <div className="flex flex-wrap items-center gap-3 rounded-2xl bg-muted/60 px-4 py-3 text-sm">
        <FolderKanban className="size-5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">You need a project first.</span>
        <Link href={`${PORTAL_BASE}/projects`} className="font-semibold text-primary hover:underline">Create a project</Link>
      </div>
    );
  }
  const noEnvs = projectId && !environments.isLoading && !environments.error && environments.items.length === 0;
  return (
    <div className="space-y-2">
      <div className="flex flex-col gap-2 rounded-2xl border border-border bg-card p-2 sm:flex-row sm:items-center">
        <span className="hidden pl-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground sm:block">Showing</span>
        <NiceSelect className="sm:w-64" value={projectId ?? ""} placeholder={projects.isLoading ? "Loading…" : "Choose a project"} onChange={(v) => go({ project: v || null, env: null })}
          options={projects.items.map((p) => ({ value: p.id, label: p.displayName, icon: FolderKanban }))} />
        <NiceSelect className="sm:w-64" value={environmentId ?? ""} disabled={!projectId} placeholder={!projectId ? "Pick a project first" : environments.isLoading ? "Loading…" : "Choose an environment"} onChange={(v) => go({ project: projectId, env: v || null })}
          options={environments.items.map((env) => ({ value: env.id, label: env.displayName, icon: ENV_ICON[env.kind] ?? FlaskConical, hint: `${nice(env.kind)}${env.status !== "ACTIVE" ? ` · ${env.status.toLowerCase()}` : ""}` }))} />
      </div>
      {noEnvs && (
        <div className="flex flex-wrap items-center gap-3 rounded-2xl bg-muted/60 px-4 py-3 text-sm">
          <FlaskConical className="size-5 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1">This project has no environments yet.</span>
          <Link href={portalHref(`${PORTAL_BASE}/projects`, { project: projectId })} className="font-semibold text-primary hover:underline">Add a Sandbox</Link>
        </div>
      )}
      <ErrorNotice error={projects.error} onRetry={() => void projects.refetch()} />
      <ErrorNotice error={environments.error} onRetry={() => void environments.refetch()} />
    </div>
  );
}

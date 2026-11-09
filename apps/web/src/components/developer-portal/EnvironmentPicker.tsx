"use client";

/**
 * Project + environment selector for environment-scoped screens (keys, webhooks, usage).
 * The selection is kept in the URL (`?project=&env=`): ids only, never a secret.
 */
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { portalHref, safeId } from "@/lib/developer-platform/navigation";
import type { Environment, Project, Workspace } from "@/lib/developer-platform/types";
import { ErrorNotice, Field, inputClass, usePagedList } from "./ui";

export function useEnvironmentSelection() {
  const params = useSearchParams();
  return { projectId: safeId(params.get("project")), environmentId: safeId(params.get("env")) };
}

export function EnvironmentPicker({ workspace, onChange }: { workspace: Workspace; onChange?: () => void }) {
  const router = useRouter();
  const pathname = usePathname();
  const { projectId, environmentId } = useEnvironmentSelection();
  const projects = usePagedList<Project>(["developer-portal", workspace.id, "projects"], {
    operation: "listProjects",
    path: { organizationId: workspace.id },
  });
  const environments = usePagedList<Environment>(
    ["developer-portal", workspace.id, "environments", projectId],
    { operation: "listEnvironments", path: { projectId: projectId ?? "_" } },
    Boolean(projectId),
  );
  const go = (query: Record<string, string | null>) => {
    onChange?.();
    router.replace(portalHref(pathname, query));
  };
  return (
    <div className="space-y-2">
      <div className="grid gap-3 rounded-xl border border-border bg-card p-4 sm:grid-cols-2">
        <Field label="Project">
          <select className={inputClass} value={projectId ?? ""} onChange={(e) => go({ project: e.target.value || null, env: null })}>
            <option value="">{projects.isLoading ? "Loading…" : "Select a project…"}</option>
            {projects.items.map((p) => (
              <option key={p.id} value={p.id}>
                {p.displayName}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Environment">
          <select
            className={inputClass}
            value={environmentId ?? ""}
            disabled={!projectId}
            onChange={(e) => go({ project: projectId, env: e.target.value || null })}
          >
            <option value="">{environments.isLoading ? "Loading…" : "Select an environment…"}</option>
            {environments.items.map((env) => (
              <option key={env.id} value={env.id}>
                {env.displayName} ({env.kind}
                {env.status !== "ACTIVE" ? `, ${env.status.toLowerCase()}` : ""})
              </option>
            ))}
          </select>
        </Field>
      </div>
      <ErrorNotice error={projects.error} onRetry={() => void projects.refetch()} />
      <ErrorNotice error={environments.error} onRetry={() => void environments.refetch()} />
    </div>
  );
}

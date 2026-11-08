"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import { Button } from "@/components/ui/button";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { PORTAL_BASE, portalHref, safeId } from "@/lib/developer-platform/navigation";
import { formatTimestamp, retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import type {
  Environment,
  Page,
  ProductionAccessRequest,
  ProductionAccessStatus,
  ProductionCapability,
  Project,
  ProjectScopes,
  ScopeRegistry,
  Workspace,
} from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { RequireWorkspace } from "../PortalShell";
import {
  ConfirmButton,
  Empty,
  ErrorNotice,
  Field,
  inputClass,
  LoadMore,
  Loading,
  Mono,
  PageHeader,
  Section,
  StatusBadge,
  Table,
  Td,
  useConsoleMutation,
  usePagedList,
} from "../ui";

const retry = {
  retry: (count: number, error: unknown) => shouldRetryQuery(count, error),
  retryDelay: (attempt: number, error: unknown) => retryDelayMs(attempt, error),
};

export default function ProjectsPage() {
  return (
    <RequireWorkspace>
      <Projects />
    </RequireWorkspace>
  );
}

function Projects() {
  const workspace = useWorkspace();
  const params = useSearchParams();
  const projectId = safeId(params.get("project"));
  return projectId ? <ProjectDetail key={projectId} workspace={workspace} projectId={projectId} /> : <ProjectList key={workspace.id} workspace={workspace} />;
}

function ProjectList({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const router = useRouter();
  const key = ["developer-portal", workspace.id, "projects"];
  const list = usePagedList<Project>(key, { operation: "listProjects", path: { organizationId: workspace.id } });
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const create = useConsoleMutation(
    () =>
      client.call<Project>({
        operation: "createProject",
        path: { organizationId: workspace.id },
        body: { displayName: displayName.trim(), ...(description.trim() ? { description: description.trim() } : {}) },
        idempotencyKey,
      }),
    [key, ["developer-portal", workspace.id, "projects-summary"]],
    (project) => {
      setDisplayName("");
      setDescription("");
      setIdempotencyKey(newIdempotencyKey());
      router.push(portalHref(`${PORTAL_BASE}/projects`, { project: project.id }));
    },
  );
  return (
    <div className="space-y-6">
      <PageHeader title="Projects & environments" description="A project is one integration. Each has SANDBOX and TEST environments; PRODUCTION appears only after production access is approved and activated." />
      {can("ProjectsManage") && (
        <Section title="Create a project">
          <form
            className="grid gap-3 sm:grid-cols-[1fr_2fr_auto] sm:items-end"
            onSubmit={(e) => {
              e.preventDefault();
              if (displayName.trim()) create.mutate(undefined);
            }}
          >
            <Field label="Name">
              <input className={inputClass} required maxLength={120} value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
            </Field>
            <Field label="Description (optional)">
              <input className={inputClass} maxLength={500} value={description} onChange={(e) => setDescription(e.target.value)} />
            </Field>
            <Button type="submit" size="sm" loading={create.isPending} disabled={!displayName.trim()}>
              Create
            </Button>
          </form>
          <div className="mt-3">
            <ErrorNotice error={create.error} />
          </div>
        </Section>
      )}
      <Section title="Projects">
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <Empty>No projects yet.</Empty>
        ) : (
          <>
            <Table head={["Name", "Status", "Created", ""]}>
              {list.items.map((p) => (
                <tr key={p.id}>
                  <Td>
                    <p className="font-medium">{p.displayName}</p>
                    {p.description && <p className="text-xs text-muted-foreground">{p.description}</p>}
                  </Td>
                  <Td>
                    <StatusBadge status={p.status} />
                  </Td>
                  <Td>{formatTimestamp(p.createdAt)}</Td>
                  <Td>
                    <Link className="text-sm font-medium text-primary underline" href={portalHref(`${PORTAL_BASE}/projects`, { project: p.id })}>
                      Open
                    </Link>
                  </Td>
                </tr>
              ))}
            </Table>
            <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
          </>
        )}
      </Section>
    </div>
  );
}

function ProjectDetail({ workspace, projectId }: { workspace: Workspace; projectId: string }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "project", projectId];
  const project = useQuery({ queryKey: key, queryFn: () => client.call<Project>({ operation: "getProject", path: { projectId } }), ...retry });
  const lifecycle = useConsoleMutation(
    (operation: "archiveProject" | "suspendProject" | "resumeProject") => client.call<Project>({ operation, path: { projectId } }),
    [key, ["developer-portal", workspace.id, "projects"]],
  );
  const p = project.data;
  return (
    <div className="space-y-6">
      <Link href={`${PORTAL_BASE}/projects`} className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
        <ArrowLeft className="h-4 w-4" /> All projects
      </Link>
      {project.isLoading ? (
        <Loading />
      ) : project.error ? (
        <ErrorNotice error={project.error} onRetry={() => void project.refetch()} />
      ) : p ? (
        <>
          <PageHeader
            title={p.displayName}
            description={p.description ?? `Project ${p.id}`}
            actions={
              <>
                <StatusBadge status={p.status} />
                {can("ProjectsManage") && p.status === "ACTIVE" && (
                  <ConfirmButton label="Suspend" message="Suspend this project? Its API keys stop working until resumed." pending={lifecycle.isPending} onConfirm={() => lifecycle.mutate("suspendProject")} />
                )}
                {can("ProjectsManage") && p.status === "SUSPENDED" && (
                  <Button size="sm" variant="outline" loading={lifecycle.isPending} onClick={() => lifecycle.mutate("resumeProject")}>
                    Resume
                  </Button>
                )}
                {can("ProjectsManage") && p.status !== "ARCHIVED" && (
                  <ConfirmButton label="Archive" message="Archive this project? This is permanent." destructive pending={lifecycle.isPending} onConfirm={() => lifecycle.mutate("archiveProject")} />
                )}
              </>
            }
          />
          <ErrorNotice error={lifecycle.error} />
          <Environments workspace={workspace} project={p} />
          <Scopes workspace={workspace} project={p} />
          <ProductionAccess workspace={workspace} project={p} />
        </>
      ) : null}
    </div>
  );
}

function Environments({ workspace, project }: { workspace: Workspace; project: Project }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "environments", project.id];
  const list = usePagedList<Environment>(key, { operation: "listEnvironments", path: { projectId: project.id } });
  const [kind, setKind] = useState<"SANDBOX" | "TEST">("SANDBOX");
  const [displayName, setDisplayName] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const create = useConsoleMutation(
    () =>
      client.call<Environment>({
        operation: "createEnvironment",
        path: { projectId: project.id },
        body: { kind, ...(displayName.trim() ? { displayName: displayName.trim() } : {}) },
        idempotencyKey,
      }),
    [key],
    () => {
      setDisplayName("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const lifecycle = useConsoleMutation(
    (v: { env: Environment; operation: "suspendEnvironment" | "resumeEnvironment" }) =>
      client.call<Environment>({ operation: v.operation, path: { environmentId: v.env.id } }),
    [key],
  );
  return (
    <Section title="Environments" description="Service accounts, API keys, webhooks and usage belong to one environment.">
      <div className="space-y-4">
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <Empty>No environments yet.</Empty>
        ) : (
          <Table head={["Environment", "Kind", "Status", "Production access", "Open"]}>
            {list.items.map((env) => (
              <tr key={env.id}>
                <Td>
                  <p className="font-medium">{env.displayName}</p>
                  <Mono>{env.id}</Mono>
                </Td>
                <Td>{env.kind}</Td>
                <Td>
                  <StatusBadge status={env.status} />
                </Td>
                <Td>{env.productionAccessState.replace(/_/g, " ").toLowerCase()}</Td>
                <Td className="space-x-2 whitespace-nowrap">
                  <Link className="text-primary underline" href={portalHref(`${PORTAL_BASE}/credentials`, { project: project.id, env: env.id })}>
                    Keys
                  </Link>
                  <Link className="text-primary underline" href={portalHref(`${PORTAL_BASE}/webhooks`, { project: project.id, env: env.id })}>
                    Webhooks
                  </Link>
                  <Link className="text-primary underline" href={portalHref(`${PORTAL_BASE}/usage`, { project: project.id, env: env.id })}>
                    Usage
                  </Link>
                  {can("EnvironmentsManage") && env.kind !== "PRODUCTION" && env.status === "ACTIVE" && (
                    <ConfirmButton label="Suspend" message="Suspend this environment?" pending={lifecycle.isPending} onConfirm={() => lifecycle.mutate({ env, operation: "suspendEnvironment" })} />
                  )}
                  {can("EnvironmentsManage") && env.kind !== "PRODUCTION" && env.status === "SUSPENDED" && (
                    <Button size="sm" variant="outline" loading={lifecycle.isPending} onClick={() => lifecycle.mutate({ env, operation: "resumeEnvironment" })}>
                      Resume
                    </Button>
                  )}
                </Td>
              </tr>
            ))}
          </Table>
        )}
        <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        <ErrorNotice error={lifecycle.error} />
        {can("EnvironmentsManage") && project.status === "ACTIVE" && (
          <form
            className="grid gap-3 sm:grid-cols-[1fr_2fr_auto] sm:items-end"
            onSubmit={(e) => {
              e.preventDefault();
              create.mutate(undefined);
            }}
          >
            <Field label="Kind">
              <select className={inputClass} value={kind} onChange={(e) => setKind(e.target.value as "SANDBOX" | "TEST")}>
                <option value="SANDBOX">SANDBOX</option>
                <option value="TEST">TEST</option>
              </select>
            </Field>
            <Field label="Display name (optional)">
              <input className={inputClass} maxLength={120} value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
            </Field>
            <Button type="submit" size="sm" loading={create.isPending}>
              Add environment
            </Button>
          </form>
        )}
        <ErrorNotice error={create.error} />
      </div>
    </Section>
  );
}

function scopeName(entry: Record<string, unknown>): string | null {
  const value = entry.scope ?? entry.name;
  return typeof value === "string" ? value : null;
}

function Scopes({ workspace, project }: { workspace: Workspace; project: Project }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "project-scopes", project.id];
  const granted = useQuery({ queryKey: key, queryFn: () => client.call<ProjectScopes>({ operation: "listProjectScopes", path: { projectId: project.id } }), ...retry });
  const registry = useQuery({
    queryKey: ["developer-portal", "scope-registry"],
    queryFn: () => client.call<ScopeRegistry>({ operation: "listScopeRegistry" }),
    staleTime: 5 * 60_000,
    ...retry,
  });
  const [scope, setScope] = useState("");
  const grant = useConsoleMutation(
    () => client.call<unknown>({ operation: "grantProjectScope", path: { projectId: project.id }, body: { scope } }),
    [key],
    () => setScope(""),
  );
  const withdraw = useConsoleMutation(
    (name: string) => client.call<unknown>({ operation: "withdrawProjectScope", path: { projectId: project.id, scope: name } }),
    [key],
  );
  const names = (granted.data?.scopes ?? []).map(scopeName).filter((s): s is string => Boolean(s));
  const grantable = (registry.data?.items ?? []).filter((s) => s.selfServiceGrantable && !names.includes(s.name));
  return (
    <Section title="API scopes" description="Scopes this project may put on its API keys. Regulated capabilities also need approved production access.">
      <div className="space-y-4">
        {granted.isLoading ? (
          <Loading />
        ) : granted.error ? (
          <ErrorNotice error={granted.error} onRetry={() => void granted.refetch()} />
        ) : names.length === 0 ? (
          <Empty>No scopes granted to this project.</Empty>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {names.map((name) => (
              <li key={name} className="flex items-center gap-1 rounded-full border border-border px-3 py-1 text-xs">
                <Mono>{name}</Mono>
                {can("ProjectsManage") && (
                  <button
                    type="button"
                    aria-label={`Withdraw ${name}`}
                    className="ml-1 text-muted-foreground hover:text-red-600"
                    disabled={withdraw.isPending}
                    onClick={() => withdraw.mutate(name)}
                  >
                    ×
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
        <ErrorNotice error={withdraw.error} />
        {can("ProjectsManage") && (
          <form
            className="flex flex-col gap-3 sm:flex-row sm:items-end"
            onSubmit={(e) => {
              e.preventDefault();
              if (scope) grant.mutate(undefined);
            }}
          >
            <div className="flex-1">
              <Field label="Grant a self-service scope">
                <select className={inputClass} value={scope} onChange={(e) => setScope(e.target.value)}>
                  <option value="">Select a scope…</option>
                  {grantable.map((s) => (
                    <option key={s.name} value={s.name}>
                      {s.name}
                      {s.regulated ? " (regulated)" : ""}
                      {s.description ? ` — ${s.description}` : ""}
                    </option>
                  ))}
                </select>
              </Field>
            </div>
            <Button type="submit" size="sm" loading={grant.isPending} disabled={!scope}>
              Grant
            </Button>
          </form>
        )}
        <ErrorNotice error={registry.error} />
        <ErrorNotice error={grant.error} />
      </div>
    </Section>
  );
}

function ProductionAccess({ workspace, project }: { workspace: Workspace; project: Project }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "production-access", project.id];
  const status = useQuery({
    queryKey: key,
    queryFn: () => client.call<ProductionAccessStatus>({ operation: "getProductionAccess", path: { projectId: project.id } }),
    ...retry,
  });
  const requestsKey = ["developer-portal", workspace.id, "production-access-requests", project.id];
  const requests = useQuery({
    queryKey: requestsKey,
    queryFn: () => client.call<Page<ProductionAccessRequest>>({ operation: "listProductionAccessRequests", path: { projectId: project.id }, query: { limit: 20 } }),
    ...retry,
  });
  const [capabilities, setCapabilities] = useState<ProductionCapability[]>([]);
  const [summary, setSummary] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const invalidate = [key, requestsKey, ["developer-portal", workspace.id, "environments", project.id]];
  const submit = useConsoleMutation(
    () =>
      client.call<ProductionAccessRequest>({
        operation: "submitProductionAccessRequest",
        path: { projectId: project.id },
        body: { capabilities, ...(summary.trim() ? { useCaseSummary: summary.trim() } : {}) },
        idempotencyKey,
      }),
    invalidate,
    () => {
      setCapabilities([]);
      setSummary("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const decide = useConsoleMutation(
    (v: { requestId: string; operation: "cancelProductionAccessRequest" | "activateProductionAccess" }) =>
      client.call<ProductionAccessRequest>({ operation: v.operation, path: { requestId: v.requestId } }),
    invalidate,
  );
  const s = status.data;
  const current = s?.currentRequest ?? null;
  const open = current && ["PRODUCTION_REQUESTED", "UNDER_REVIEW", "APPROVED"].includes(current.state);
  return (
    <Section title="Production access" description="CF Meet reviews production requests. Approval decisions are made by CF Meet operators, not from this console.">
      {status.isLoading ? (
        <Loading />
      ) : status.error ? (
        <ErrorNotice error={status.error} onRetry={() => void status.refetch()} />
      ) : s ? (
        <div className="space-y-4 text-sm">
          <div className="flex flex-wrap items-center gap-3">
            <StatusBadge status={s.state} />
            <span className="text-muted-foreground">Enabled: {s.enabledCapabilities.length ? s.enabledCapabilities.join(", ") : "none"}</span>
          </div>
          {current && (
            <div className="rounded-lg border border-border p-3">
              <p>
                Current request <StatusBadge status={current.state} /> · requested {current.requestedCapabilities.join(", ")} · {formatTimestamp(current.createdAt)}
              </p>
              {current.approvalExpiresAt && <p className="text-xs text-muted-foreground">Approval expires {formatTimestamp(current.approvalExpiresAt)}</p>}
              {can("ProductionAccessRequest") && (
                <div className="mt-2 flex gap-2">
                  {current.state === "APPROVED" && (
                    <ConfirmButton
                      label="Activate production"
                      message="Create the PRODUCTION environment now?"
                      pending={decide.isPending}
                      onConfirm={() => decide.mutate({ requestId: current.id, operation: "activateProductionAccess" })}
                    />
                  )}
                  {open && (
                    <ConfirmButton
                      label="Cancel request"
                      message="Cancel this production request?"
                      destructive
                      pending={decide.isPending}
                      onConfirm={() => decide.mutate({ requestId: current.id, operation: "cancelProductionAccessRequest" })}
                    />
                  )}
                </div>
              )}
            </div>
          )}
          <ErrorNotice error={decide.error} />
          {can("ProductionAccessRequest") && !open && (
            <form
              className="space-y-3"
              onSubmit={(e) => {
                e.preventDefault();
                if (capabilities.length) submit.mutate(undefined);
              }}
            >
              <p className="font-medium">Request production access</p>
              <div className="flex flex-wrap gap-3">
                {s.capabilityCatalogue.map((c) => (
                  <label key={c.name} className="flex items-center gap-1.5">
                    <input
                      type="checkbox"
                      checked={capabilities.includes(c.name)}
                      onChange={(e) => setCapabilities((prev) => (e.target.checked ? [...prev, c.name] : prev.filter((x) => x !== c.name)))}
                    />
                    {c.name.replace(/_/g, " ").toLowerCase()}
                    {c.regulated && <span className="text-xs text-amber-600">regulated</span>}
                  </label>
                ))}
              </div>
              <Field label="Use case (optional)">
                <textarea className={inputClass} rows={3} maxLength={2000} value={summary} onChange={(e) => setSummary(e.target.value)} />
              </Field>
              <Button type="submit" size="sm" loading={submit.isPending} disabled={!capabilities.length}>
                Submit request
              </Button>
            </form>
          )}
          <ErrorNotice error={submit.error} />
          {requests.data && requests.data.items.length > 0 && (
            <Table head={["Request", "State", "Capabilities", "Updated"]}>
              {requests.data.items.map((r) => (
                <tr key={r.id}>
                  <Td>
                    <Mono>{r.id}</Mono>
                  </Td>
                  <Td>
                    <StatusBadge status={r.state} />
                  </Td>
                  <Td>{(r.approvedCapabilities.length ? r.approvedCapabilities : r.requestedCapabilities).join(", ")}</Td>
                  <Td>{formatTimestamp(r.updatedAt)}</Td>
                </tr>
              ))}
            </Table>
          )}
          <ErrorNotice error={requests.error} />
        </div>
      ) : null}
    </Section>
  );
}

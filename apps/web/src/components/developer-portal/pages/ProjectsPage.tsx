"use client";

import { NiceSelect } from "@/components/ui/nice-select";

import { useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, BarChart3, Check, ChevronRight, FlaskConical, FolderKanban, KeyRound, Plus, Rocket, ShieldCheck, TestTube2, Webhook, X } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
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

const nice = (v: string) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : v);
const CAPABILITY: Record<string, { label: string; hint: string }> = {
  PLATFORM_CONTROL: { label: "Platform control", hint: "Manage your workspace from code" },
  MEETINGS: { label: "Meetings", hint: "Create and run video meetings" },
  MESSAGING: { label: "Messaging", hint: "Chat and conversations" },
  APP_CALLING: { label: "In-app calling", hint: "Voice and video calls inside your app" },
  PUBLIC_NETWORK_CALLING: { label: "Phone calls", hint: "Call regular phone numbers" },
  PRIVACY_RELAY: { label: "Privacy relay", hint: "Masked numbers between two people" },
  NUMBERS: { label: "Phone numbers", hint: "Rent and manage numbers" },
  SMS: { label: "SMS", hint: "Send and receive text messages" },
};
const capLabel = (name: string) => CAPABILITY[name]?.label ?? nice(name);
const ENV_ICON: Record<string, typeof FlaskConical> = { SANDBOX: FlaskConical, TEST: TestTube2, PRODUCTION: Rocket };
const PROD_STATE: Record<string, string> = {
  NOT_READY: "Your project is in Sandbox and Test only. Request production when you're ready to go live.",
  PRODUCTION_REQUESTED: "Request sent. CF Meet will review it.",
  UNDER_REVIEW: "CF Meet is reviewing your request.",
  APPROVED: "Approved. Activate production to create your live environment.",
  ACTIVE: "Production is live.",
  REJECTED: "The last request wasn't approved. You can send a new one.",
};

function Tabs<T extends string>({ value, onChange, items }: { value: T; onChange: (v: T) => void; items: { id: T; label: string; icon: typeof FolderKanban }[] }) {
  return (
    <nav className="no-scrollbar flex gap-1 overflow-x-auto border-b border-border" role="tablist">
      {items.map(({ id, label, icon: Icon }) => (
        <button key={id} type="button" role="tab" aria-selected={value === id} onClick={() => onChange(id)}
          className={cn("-mb-px inline-flex h-11 shrink-0 items-center gap-2 border-b-2 px-3 text-sm font-medium transition-colors", value === id ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>
          <Icon className="size-4" />{label}
        </button>
      ))}
    </nav>
  );
}

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
  const [creating, setCreating] = useState(false);
  return (
    <div className="space-y-6">
      <PageHeader
        title="Projects"
        description="One project per app or integration. Each gets Sandbox and Test environments; a live one comes after CF Meet approves production."
        actions={can("ProjectsManage") ? <button type="button" onClick={() => setCreating(true)} className="inline-flex h-10 items-center gap-2 rounded-full bg-primary pl-3.5 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New project</button> : null}
      />
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : list.items.length === 0 ? (
        <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-16 text-center">
          <span className="flex size-16 items-center justify-center rounded-2xl bg-primary/10 text-primary"><FolderKanban className="size-7" /></span>
          <p className="mt-4 text-lg font-semibold">No projects yet</p>
          <p className="mt-1 max-w-sm text-sm text-muted-foreground">Create one for the app you&apos;re building. You&apos;ll add environments, keys and webhooks inside it.</p>
          {can("ProjectsManage") && <button type="button" onClick={() => setCreating(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New project</button>}
        </div>
      ) : (
        <>
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {list.items.map((p) => (
              <li key={p.id}>
                <Link href={portalHref(`${PORTAL_BASE}/projects`, { project: p.id })} className="group flex h-full flex-col rounded-2xl border border-border bg-card p-5 transition-all hover:border-primary/40 hover:shadow-md">
                  <div className="flex items-start gap-3">
                    <span className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><FolderKanban className="size-5" /></span>
                    <div className="min-w-0 flex-1">
                      <p className="truncate font-semibold">{p.displayName}</p>
                      <p className="mt-0.5 line-clamp-2 text-sm text-muted-foreground">{p.description || "No description"}</p>
                    </div>
                    <ChevronRight className="size-5 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
                  </div>
                  <div className="mt-4 flex items-center justify-between gap-2 border-t border-border pt-3 text-xs text-muted-foreground">
                    <StatusBadge status={p.status} />
                    <span>Created {formatTimestamp(p.createdAt)}</span>
                  </div>
                </Link>
              </li>
            ))}
          </ul>
          <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        </>
      )}
      <Dialog open={creating} onOpenChange={setCreating}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader className="text-left">
            <DialogTitle>New project</DialogTitle>
            <DialogDescription>Name it after the app or integration you&apos;re building.</DialogDescription>
          </DialogHeader>
          <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); if (displayName.trim()) create.mutate(undefined); }}>
            <Field label="Name">
              <input autoFocus className={inputClass} required maxLength={120} placeholder="e.g. Clinic booking app" value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
            </Field>
            <Field label="Description (optional)">
              <input className={inputClass} maxLength={500} placeholder="What it does, in a line" value={description} onChange={(e) => setDescription(e.target.value)} />
            </Field>
            <ErrorNotice error={create.error} />
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => setCreating(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
              <Button type="submit" className="rounded-full" loading={create.isPending} disabled={!displayName.trim()}>Create project</Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
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
  const [tab, setTab] = useState<"environments" | "permissions" | "live">("environments");
  return (
    <div className="space-y-6">
      <Link href={`${PORTAL_BASE}/projects`} className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
        <ArrowLeft className="size-4" /> Projects
      </Link>
      {project.isLoading ? (
        <Loading />
      ) : project.error ? (
        <ErrorNotice error={project.error} onRetry={() => void project.refetch()} />
      ) : p ? (
        <>
          <PageHeader
            title={p.displayName}
            description={p.description || undefined}
            actions={
              <>
                <span className="self-center"><StatusBadge status={p.status} /></span>
                {can("ProjectsManage") && p.status === "ACTIVE" && (
                  <ConfirmButton label="Suspend" message="Suspend this project? Its API keys stop working until resumed." pending={lifecycle.isPending} onConfirm={() => lifecycle.mutate("suspendProject")} />
                )}
                {can("ProjectsManage") && p.status === "SUSPENDED" && (
                  <Button size="sm" variant="outline" className="rounded-full" loading={lifecycle.isPending} onClick={() => lifecycle.mutate("resumeProject")}>
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
          <Tabs value={tab} onChange={setTab} items={[
            { id: "environments", label: "Environments", icon: FlaskConical },
            { id: "permissions", label: "API permissions", icon: ShieldCheck },
            { id: "live", label: "Go live", icon: Rocket },
          ]} />
          {tab === "environments" && <Environments workspace={workspace} project={p} />}
          {tab === "permissions" && <Scopes workspace={workspace} project={p} />}
          {tab === "live" && <ProductionAccess workspace={workspace} project={p} />}
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
  const [adding, setAdding] = useState(false);
  const canAdd = can("EnvironmentsManage") && project.status === "ACTIVE";
  const quick = "inline-flex h-8 items-center gap-1.5 rounded-full px-3 text-xs font-semibold ring-1 ring-border hover:bg-muted";
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">Keys, webhooks and usage each belong to one environment.</p>
        {canAdd && <button type="button" onClick={() => setAdding(true)} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-primary pl-3 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Add environment</button>}
      </div>
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : list.items.length === 0 ? (
        <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-14 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-amber-500/10 text-amber-600"><FlaskConical className="size-6" /></span>
          <p className="mt-4 font-semibold">Add your first environment</p>
          <p className="mt-1 max-w-sm text-sm text-muted-foreground">Start with a Sandbox. It&apos;s a safe place to try the API before anything real.</p>
          {canAdd && <button type="button" onClick={() => setAdding(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Add environment</button>}
        </div>
      ) : (
        <ul className="grid gap-3 md:grid-cols-2">
          {list.items.map((env) => {
            const Icon = ENV_ICON[env.kind] ?? FlaskConical;
            return (
              <li key={env.id} className="rounded-2xl border border-border bg-card p-5">
                <div className="flex items-start gap-3">
                  <span className={cn("flex size-11 shrink-0 items-center justify-center rounded-xl", env.kind === "PRODUCTION" ? "bg-emerald-500/10 text-emerald-600" : env.kind === "TEST" ? "bg-violet-500/10 text-violet-600" : "bg-amber-500/10 text-amber-600")}><Icon className="size-5" /></span>
                  <div className="min-w-0 flex-1">
                    <p className="truncate font-semibold">{env.displayName}</p>
                    <p className="text-xs text-muted-foreground">{nice(env.kind)}{env.kind !== "PRODUCTION" ? " · not live" : " · live"}</p>
                  </div>
                  <StatusBadge status={env.status} />
                </div>
                <div className="mt-4 flex flex-wrap items-center gap-2">
                  <Link className={quick} href={portalHref(`${PORTAL_BASE}/credentials`, { project: project.id, env: env.id })}><KeyRound className="size-3.5" />Keys</Link>
                  <Link className={quick} href={portalHref(`${PORTAL_BASE}/webhooks`, { project: project.id, env: env.id })}><Webhook className="size-3.5" />Webhooks</Link>
                  <Link className={quick} href={portalHref(`${PORTAL_BASE}/usage`, { project: project.id, env: env.id })}><BarChart3 className="size-3.5" />Usage</Link>
                  <span className="ml-auto">
                    {can("EnvironmentsManage") && env.kind !== "PRODUCTION" && env.status === "ACTIVE" && (
                      <ConfirmButton label="Suspend" message="Suspend this environment? Its keys stop working until you resume it." pending={lifecycle.isPending} onConfirm={() => lifecycle.mutate({ env, operation: "suspendEnvironment" })} />
                    )}
                    {can("EnvironmentsManage") && env.kind !== "PRODUCTION" && env.status === "SUSPENDED" && (
                      <Button size="sm" variant="outline" className="rounded-full" loading={lifecycle.isPending} onClick={() => lifecycle.mutate({ env, operation: "resumeEnvironment" })}>Resume</Button>
                    )}
                  </span>
                </div>
              </li>
            );
          })}
        </ul>
      )}
      <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
      <ErrorNotice error={lifecycle.error} />
      <Dialog open={adding} onOpenChange={setAdding}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader className="text-left">
            <DialogTitle>Add environment</DialogTitle>
            <DialogDescription>A separate space with its own keys and webhooks.</DialogDescription>
          </DialogHeader>
          <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); create.mutate(undefined, { onSuccess: () => setAdding(false) }); }}>
            <div className="grid grid-cols-2 gap-2">
              {([["SANDBOX", "Sandbox", "Try things safely", FlaskConical], ["TEST", "Test", "Automated tests", TestTube2]] as const).map(([value, label, hint, Icon]) => (
                <button key={value} type="button" aria-pressed={kind === value} onClick={() => setKind(value)}
                  className={cn("flex flex-col items-start gap-2 rounded-xl p-3 text-left ring-1 transition-colors", kind === value ? "bg-primary/5 ring-2 ring-primary" : "ring-border hover:bg-muted/50")}>
                  <Icon className={cn("size-5", value === "TEST" ? "text-violet-600" : "text-amber-600")} />
                  <span><span className="block text-sm font-semibold">{label}</span><span className="block text-xs text-muted-foreground">{hint}</span></span>
                </button>
              ))}
            </div>
            <Field label="Name (optional)">
              <input className={inputClass} maxLength={120} placeholder={kind === "SANDBOX" ? "e.g. My sandbox" : "e.g. CI tests"} value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
            </Field>
            <ErrorNotice error={create.error} />
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => setAdding(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
              <Button type="submit" className="rounded-full" loading={create.isPending}>Add environment</Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
    </div>
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
  const describe = (name: string) => registry.data?.items.find((x) => x.name === name);
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">What this project&apos;s API keys are allowed to do. Each key can use any of these. Regulated ones also need production approval.</p>
      {granted.isLoading ? (
        <Loading />
      ) : granted.error ? (
        <ErrorNotice error={granted.error} onRetry={() => void granted.refetch()} />
      ) : names.length === 0 ? (
        <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-12 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><ShieldCheck className="size-6" /></span>
          <p className="mt-4 font-semibold">No permissions yet</p>
          <p className="mt-1 max-w-sm text-sm text-muted-foreground">Add what your app needs, like reading meetings, before you create keys.</p>
        </div>
      ) : (
        <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
          {names.map((name) => {
            const info = describe(name);
            return (
              <li key={name} className="flex items-center gap-3 px-4 py-3">
                <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-600"><Check className="size-4" /></span>
                <div className="min-w-0 flex-1">
                  <p className="flex flex-wrap items-center gap-2"><Mono>{name}</Mono>{info?.regulated && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-[11px] font-semibold text-amber-700 dark:text-amber-300">Needs review</span>}</p>
                  {info?.description && <p className="mt-0.5 text-sm text-muted-foreground">{info.description}</p>}
                </div>
                {can("ProjectsManage") && (
                  <button type="button" aria-label={`Remove ${name}`} title="Remove" disabled={withdraw.isPending} onClick={() => withdraw.mutate(name)} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-red-500/10 hover:text-red-600"><X className="size-4" /></button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <ErrorNotice error={withdraw.error} />
      {can("ProjectsManage") && (
        <form className="flex flex-col gap-2 rounded-2xl bg-muted/50 p-4 sm:flex-row sm:items-end" onSubmit={(e) => { e.preventDefault(); if (scope) grant.mutate(undefined); }}>
          <div className="min-w-0 flex-1">
            <Field label="Add a permission">
              <NiceSelect value={scope} placeholder={grantable.length ? "Choose a permission" : "Nothing else to add"} disabled={!grantable.length} onChange={setScope} options={grantable.map((x) => ({ value: x.name, label: x.name, hint: [x.regulated ? "Needs review" : "", x.description ?? ""].filter(Boolean).join(" · ") || undefined }))} />
            </Field>
          </div>
          <Button type="submit" className="h-11 rounded-full px-5" loading={grant.isPending} disabled={!scope}>Add</Button>
        </form>
      )}
      <ErrorNotice error={registry.error} />
      <ErrorNotice error={grant.error} />
    </div>
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
    <div className="space-y-5">
      {status.isLoading ? (
        <Loading />
      ) : status.error ? (
        <ErrorNotice error={status.error} onRetry={() => void status.refetch()} />
      ) : s ? (
        <>
          <div className={cn("flex flex-wrap items-center gap-4 rounded-2xl p-5", s.state === "ACTIVE" ? "bg-emerald-500/10" : "bg-muted/50")}>
            <span className={cn("flex size-12 shrink-0 items-center justify-center rounded-xl", s.state === "ACTIVE" ? "bg-emerald-500 text-white" : "bg-background text-primary ring-1 ring-border")}><Rocket className="size-6" /></span>
            <div className="min-w-0 flex-1">
              <p className="flex flex-wrap items-center gap-2 font-semibold">Production <StatusBadge status={s.state} /></p>
              <p className="mt-0.5 text-sm text-muted-foreground">{PROD_STATE[s.state] ?? "CF Meet reviews every production request."}</p>
              {s.enabledCapabilities.length > 0 && <p className="mt-2 flex flex-wrap gap-1.5">{s.enabledCapabilities.map((c) => <span key={c} className="rounded-full bg-background px-2.5 py-0.5 text-xs font-medium ring-1 ring-border">{capLabel(c)}</span>)}</p>}
            </div>
          </div>

          {current && (
            <div className="rounded-2xl border border-border bg-card p-5">
              <div className="flex flex-wrap items-center gap-2">
                <p className="font-semibold">Your request</p>
                <StatusBadge status={current.state} />
                <span className="text-xs text-muted-foreground">sent {formatTimestamp(current.createdAt)}</span>
              </div>
              <p className="mt-2 flex flex-wrap gap-1.5">{current.requestedCapabilities.map((c) => <span key={c} className="rounded-full bg-muted px-2.5 py-0.5 text-xs font-medium">{capLabel(c)}</span>)}</p>
              {current.approvalExpiresAt && <p className="mt-2 text-xs text-muted-foreground">Activate before {formatTimestamp(current.approvalExpiresAt)}, or the approval expires.</p>}
              {can("ProductionAccessRequest") && (current.state === "APPROVED" || open) && (
                <div className="mt-4 flex flex-wrap gap-2">
                  {current.state === "APPROVED" && (
                    <ConfirmButton label="Activate production" message="Create your live environment now? Your app can then serve real users." pending={decide.isPending} onConfirm={() => decide.mutate({ requestId: current.id, operation: "activateProductionAccess" })} />
                  )}
                  {open && (
                    <ConfirmButton label="Cancel request" message="Cancel this production request? You can send a new one later." destructive pending={decide.isPending} onConfirm={() => decide.mutate({ requestId: current.id, operation: "cancelProductionAccessRequest" })} />
                  )}
                </div>
              )}
            </div>
          )}
          <ErrorNotice error={decide.error} />

          {can("ProductionAccessRequest") && !open && (
            <form className="space-y-4 rounded-2xl border border-border bg-card p-5" onSubmit={(e) => { e.preventDefault(); if (capabilities.length) submit.mutate(undefined); }}>
              <div>
                <p className="font-semibold">Request production</p>
                <p className="mt-0.5 text-sm text-muted-foreground">Pick what your live app needs. CF Meet reviews every request; ones marked &quot;Needs review&quot; take longer.</p>
              </div>
              <div className="grid gap-2 sm:grid-cols-2">
                {s.capabilityCatalogue.map((c) => {
                  const on = capabilities.includes(c.name);
                  return (
                    <label key={c.name} className={cn("flex cursor-pointer items-start gap-3 rounded-xl p-3 ring-1 transition-colors", on ? "bg-primary/5 ring-2 ring-primary" : "ring-border hover:bg-muted/50")}>
                      <input type="checkbox" className="sr-only" checked={on} onChange={(e) => setCapabilities((prev) => (e.target.checked ? [...prev, c.name] : prev.filter((x) => x !== c.name)))} />
                      <span className={cn("mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-md border", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                      <span className="min-w-0">
                        <span className="flex flex-wrap items-center gap-1.5 text-sm font-semibold">{capLabel(c.name)}{c.regulated && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-[11px] font-semibold text-amber-700 dark:text-amber-300">Needs review</span>}</span>
                        {CAPABILITY[c.name]?.hint && <span className="block text-xs text-muted-foreground">{CAPABILITY[c.name].hint}</span>}
                      </span>
                    </label>
                  );
                })}
              </div>
              <Field label="What will you use it for? (optional)" hint="A sentence or two helps the review go faster.">
                <textarea className={cn(inputClass, "h-auto min-h-24 py-2.5")} rows={3} maxLength={2000} value={summary} onChange={(e) => setSummary(e.target.value)} />
              </Field>
              <ErrorNotice error={submit.error} />
              <div className="flex justify-end">
                <Button type="submit" className="rounded-full" loading={submit.isPending} disabled={!capabilities.length}>{capabilities.length ? `Request ${capabilities.length} ${capabilities.length === 1 ? "capability" : "capabilities"}` : "Request production"}</Button>
              </div>
            </form>
          )}

          {requests.data && requests.data.items.length > 1 && (
            <div>
              <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Past requests</p>
              <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
                {requests.data.items.map((req) => (
                  <li key={req.id} className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
                    <StatusBadge status={req.state} />
                    <span className="min-w-0 flex-1 truncate">{(req.approvedCapabilities.length ? req.approvedCapabilities : req.requestedCapabilities).map(capLabel).join(", ")}</span>
                    <span className="text-xs text-muted-foreground">{formatTimestamp(req.updatedAt)}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <ErrorNotice error={requests.error} />
        </>
      ) : null}
    </div>
  );
}

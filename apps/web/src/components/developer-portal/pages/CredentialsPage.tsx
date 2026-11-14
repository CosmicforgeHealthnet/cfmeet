"use client";

import { useState } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { NiceSelect } from "@/components/ui/nice-select";
import { Ban, Check, ChevronRight, KeyRound, MoreHorizontal, Pause, Play, Plus, RefreshCw, Server } from "lucide-react";
import { Popover } from "radix-ui";
import { cn } from "@/lib/utils";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { portalHref, safeId } from "@/lib/developer-platform/navigation";
import {
  credentialActions,
  credentialBadges,
  credentialSecret,
  expiryFromLocalInput,
  formatTimestamp,
  retryDelayMs,
  shouldRetryQuery,
  type OneTimeSecret,
} from "@/lib/developer-platform/presentation";
import type { Credential, IssuedCredential, ProjectScopes, ServiceAccount, Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { EnvironmentPicker, useEnvironmentSelection } from "../EnvironmentPicker";
import { RequireWorkspace } from "../PortalShell";
import {
  Badge,
  ConfirmButton,
  Empty,
  ErrorNotice,
  Field,
  inputClass,
  LoadMore,
  Loading,
  Mono,
  NotPermitted,
  PageHeader,
  SecretOnce,
  Section,
  StatusBadge,
  Table,
  Td,
  friendlyTime,
  shortKey,
  useConsoleMutation,
  usePagedList,
} from "../ui";

/** Backend rule for service account names (Platform Access key). */
const SA_NAME = /^[a-z0-9][a-z0-9-]{0,62}$/;

export default function CredentialsPage() {
  return (
    <RequireWorkspace>
      <Credentials />
    </RequireWorkspace>
  );
}

function Credentials() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  const { projectId, environmentId } = useEnvironmentSelection();
  return (
    <div className="space-y-6">
      <PageHeader
        title="API keys"
        description="Keys let your server talk to CF Meet. Each key belongs to a service account: a login for one of your servers or apps. Keep keys on servers only, never in a browser, mobile app or code repository."
      />
      {!can("ServiceAccountsRead") ? (
        <NotPermitted what="service accounts and API keys" />
      ) : (
        <>
          <EnvironmentPicker workspace={workspace} />
          {environmentId ? (
            <ServiceAccounts key={environmentId} workspace={workspace} projectId={projectId} environmentId={environmentId} />
          ) : (
            <Empty>Choose a project and environment above to see its keys.</Empty>
          )}
        </>
      )}
    </div>
  );
}

function ServiceAccounts({ workspace, projectId, environmentId }: { workspace: Workspace; projectId: string | null; environmentId: string }) {
  const { client, can } = useDeveloperPortal();
  const router = useRouter();
  const pathname = usePathname();
  const params = useSearchParams();
  const selected = safeId(params.get("sa"));
  const key = ["developer-portal", workspace.id, "service-accounts", environmentId];
  const list = usePagedList<ServiceAccount>(key, { operation: "listServiceAccounts", path: { environmentId } });
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const create = useConsoleMutation(
    () =>
      client.call<ServiceAccount>({
        operation: "createServiceAccount",
        path: { environmentId },
        body: { name: name.trim().replace(/-+$/, ""), ...(description.trim() ? { description: description.trim() } : {}) },
        idempotencyKey,
      }),
    [key],
    (sa) => {
      setName("");
      setDescription("");
      setIdempotencyKey(newIdempotencyKey());
      router.replace(portalHref(pathname, { project: projectId, env: environmentId, sa: sa.id }));
    },
  );
  const lifecycle = useConsoleMutation(
    (v: { sa: ServiceAccount; operation: "suspendServiceAccount" | "resumeServiceAccount" }) =>
      client.call<ServiceAccount>({ operation: v.operation, path: { environmentId, serviceAccountId: v.sa.id } }),
    [key],
  );
  const current = list.items.find((sa) => sa.id === selected) ?? null;

  const [creating, setCreating] = useState(false);
  const select = (id: string) => router.replace(portalHref(pathname, { project: projectId, env: environmentId, sa: id }));
  return (
    <div className="space-y-6">
      <section className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h2 className="text-lg font-semibold">Service accounts</h2>
          {can("ServiceAccountsManage") && <button type="button" onClick={() => setCreating(true)} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-primary pl-3 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New service account</button>}
        </div>
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-14 text-center">
            <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Server className="size-6" /></span>
            <p className="mt-4 font-semibold">Add a service account first</p>
            <p className="mt-1 max-w-sm text-sm text-muted-foreground">Make one for each server or app that calls CF Meet, like &quot;Booking backend&quot;. Then create keys for it.</p>
            {can("ServiceAccountsManage") && <button type="button" onClick={() => setCreating(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New service account</button>}
          </div>
        ) : (
          <ul className="grid gap-3 md:grid-cols-2">
            {list.items.map((sa) => {
              const on = sa.id === selected;
              return (
                <li key={sa.id}>
                  <div className={cn("flex h-full items-center gap-3 rounded-2xl border bg-card p-4 transition-colors", on ? "border-primary/50 ring-1 ring-primary/30" : "border-border")}>
                    <button type="button" onClick={() => select(sa.id)} className="flex min-w-0 flex-1 items-center gap-3 text-left">
                      <span className={cn("flex size-11 shrink-0 items-center justify-center rounded-xl", on ? "bg-primary text-primary-foreground" : "bg-muted text-foreground")}><Server className="size-5" /></span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate font-semibold">{sa.name}</span>
                        <span className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground"><StatusBadge status={sa.status} /><span className="truncate">{sa.description || `Added ${friendlyTime(sa.createdAt)}`}</span></span>
                      </span>
                      <ChevronRight className={cn("size-5 shrink-0 text-muted-foreground", on && "text-primary")} />
                    </button>
                    {((can("ServiceAccountsSuspend") && sa.status === "ACTIVE") || (can("ServiceAccountsManage") && sa.status === "SUSPENDED")) && (
                      <RowMenu items={sa.status === "ACTIVE"
                        ? [{ label: "Suspend", icon: Pause, confirm: "Suspend this service account? All its keys stop working until you resume it.", run: () => lifecycle.mutate({ sa, operation: "suspendServiceAccount" }) }]
                        : [{ label: "Resume", icon: Play, run: () => lifecycle.mutate({ sa, operation: "resumeServiceAccount" }) }]} />
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
        <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        <ErrorNotice error={lifecycle.error} />
        {list.items.length > 0 && !current && <p className="text-sm text-muted-foreground">Pick a service account to see and create its keys.</p>}
      </section>

      {current && <CredentialList key={current.id} workspace={workspace} projectId={projectId} environmentId={environmentId} serviceAccount={current} />}

      <Dialog open={creating} onOpenChange={setCreating}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader className="text-left">
            <DialogTitle>New service account</DialogTitle>
            <DialogDescription>Name it after the server or app that will use its keys.</DialogDescription>
          </DialogHeader>
          <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); if (SA_NAME.test(name.replace(/-+$/, ""))) { setName(name.replace(/-+$/, "")); create.mutate(undefined, { onSuccess: () => setCreating(false) }); } }}>
            <Field label="Name" hint="Lowercase letters, numbers and hyphens. Spaces turn into hyphens as you type.">
              <input autoFocus className={cn(inputClass, "font-mono")} required maxLength={63} placeholder="e.g. booking-backend" value={name}
                onChange={(e) => setName(e.target.value.toLowerCase().replace(/[\s_]+/g, "-").replace(/[^a-z0-9-]/g, "").replace(/^-+/, "").slice(0, 63))} />
            </Field>
            <Field label="Description (optional)">
              <input className={inputClass} maxLength={500} placeholder="What uses it" value={description} onChange={(e) => setDescription(e.target.value)} />
            </Field>
            <ErrorNotice error={create.error} />
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => setCreating(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
              <Button type="submit" className="rounded-full" loading={create.isPending} disabled={!SA_NAME.test(name.replace(/-+$/, ""))}>Create</Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function scopeNames(data: ProjectScopes | undefined): string[] {
  return (data?.scopes ?? [])
    .map((entry) => (typeof entry.scope === "string" ? entry.scope : typeof entry.name === "string" ? entry.name : null))
    .filter((s): s is string => Boolean(s));
}

function CredentialList({
  workspace,
  projectId,
  environmentId,
  serviceAccount,
}: {
  workspace: Workspace;
  projectId: string | null;
  environmentId: string;
  serviceAccount: ServiceAccount;
}) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "credentials", environmentId, serviceAccount.id];
  const list = usePagedList<Credential>(key, {
    operation: "listCredentials",
    path: { environmentId, serviceAccountId: serviceAccount.id },
  });
  const scopes = useQuery({
    queryKey: ["developer-portal", workspace.id, "project-scopes", projectId],
    queryFn: () => client.call<ProjectScopes>({ operation: "listProjectScopes", path: { projectId: projectId! } }),
    enabled: Boolean(projectId),
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const available = scopeNames(scopes.data);

  // The one-time secret lives only here, in component state, until "Done".
  const [secret, setSecret] = useState<{ title: string; value: OneTimeSecret } | null>(null);
  const [label, setLabel] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [expiry, setExpiry] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const [rotating, setRotating] = useState<{ credential: Credential; overlap: string; key: string } | null>(null);

  const create = useConsoleMutation(
    (expiresAt: string | null) =>
      client.call<IssuedCredential>({
        operation: "createCredential",
        path: { environmentId, serviceAccountId: serviceAccount.id },
        body: { ...(chosen.length ? { scopes: chosen } : {}), ...(label.trim() ? { label: label.trim() } : {}), ...(expiresAt ? { expiresAt } : {}) },
        idempotencyKey,
      }),
    [key],
    (issued) => {
      setSecret({ title: "New API key", value: credentialSecret(issued) });
      setLabel("");
      setChosen([]);
      setExpiry("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const rotate = useConsoleMutation(
    (v: { credential: Credential; overlapMinutes: number | null; key: string }) =>
      client.call<IssuedCredential>({
        operation: "rotateCredential",
        path: { environmentId, credentialId: v.credential.id },
        body: v.overlapMinutes != null ? { overlapMinutes: v.overlapMinutes } : {},
        idempotencyKey: v.key,
      }),
    [key],
    (issued) => {
      setRotating(null);
      setSecret({ title: "Rotated API key", value: credentialSecret(issued) });
    },
  );
  const action = useConsoleMutation(
    (v: { credential: Credential; operation: "revokeCredential" | "suspendCredential" | "resumeCredential" }) =>
      client.call<Credential>({
        operation: v.operation,
        path: { environmentId, credentialId: v.credential.id },
        ...(v.operation === "suspendCredential" ? { body: {} } : {}),
      }),
    [key],
  );

  const [creatingKey, setCreatingKey] = useState(false);
  const canCreate = can("CredentialsManage") && serviceAccount.status === "ACTIVE";
  const OVERLAP = [
    { value: "", label: "Workspace default", hint: "The usual handover time" },
    { value: "0", label: "Stop the old key now", hint: "Only if it may have leaked" },
    { value: "15", label: "15 minutes" },
    { value: "60", label: "1 hour" },
    { value: "1440", label: "24 hours", hint: "Time to update every server" },
  ];
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">Keys for {serviceAccount.name}</h2>
          <p className="text-sm text-muted-foreground">After creating a key, only its masked form is ever shown again.</p>
        </div>
        {canCreate && <button type="button" onClick={() => setCreatingKey(true)} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-primary pl-3 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Create key</button>}
      </div>

      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : list.items.length === 0 ? (
        <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-12 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-amber-500/10 text-amber-600"><KeyRound className="size-6" /></span>
          <p className="mt-4 font-semibold">No keys yet</p>
          <p className="mt-1 max-w-sm text-sm text-muted-foreground">Create one and put it in your server&apos;s secret settings.</p>
          {canCreate && <button type="button" onClick={() => setCreatingKey(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Create key</button>}
        </div>
      ) : (
        <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
          {list.items.map((c) => {
            const allowed = credentialActions(c);
            return (
              <li key={c.id} className="flex flex-col gap-3 p-4 sm:flex-row sm:items-center">
                <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-amber-500/10 text-amber-600"><KeyRound className="size-5" /></span>
                <div className="min-w-0 flex-1">
                  <p className="flex flex-wrap items-center gap-2 font-semibold">{c.label ?? "Unnamed key"}{credentialBadges(c).map((b) => <Badge key={b.label} {...b} />)}</p>
                  <p className="mt-1" title={c.masked ?? undefined}><Mono>{shortKey(c.masked ?? c.publicId ?? c.id)}</Mono></p>
                  <p className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                    <span>{c.expiresAt ? `Expires ${friendlyTime(c.expiresAt)}` : "No expiry date"}</span>
                    {c.scopes?.length ? <span>Can: {c.scopes.join(", ")}</span> : <span>All project permissions</span>}
                    {c.supersessionValidUntil && <span className="text-amber-700 dark:text-amber-300">Old key works until {friendlyTime(c.supersessionValidUntil)}</span>}
                  </p>
                </div>
                <div className="flex items-center gap-1">
                  {can("CredentialsManage") && allowed.rotate && (
                    <Button size="sm" variant="outline" className="rounded-full" onClick={() => setRotating({ credential: c, overlap: "", key: newIdempotencyKey() })}><RefreshCw className="size-3.5" />Rotate</Button>
                  )}
                  <RowMenu items={[
                    ...(can("CredentialsRevoke") && allowed.suspend ? [{ label: "Suspend", icon: Pause, confirm: "Suspend this key? It stops working until you resume it.", run: () => action.mutate({ credential: c, operation: "suspendCredential" }) }] : []),
                    ...(can("CredentialsManage") && allowed.resume ? [{ label: "Resume", icon: Play, run: () => action.mutate({ credential: c, operation: "resumeCredential" }) }] : []),
                    ...(can("CredentialsRevoke") && allowed.revoke ? [{ label: "Revoke key", icon: Ban, danger: true, confirm: "Revoke this key for good? Anything using it stops working straight away.", run: () => action.mutate({ credential: c, operation: "revokeCredential" }) }] : []),
                  ]} />
                </div>
              </li>
            );
          })}
        </ul>
      )}
      <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
      <ErrorNotice error={action.error} />

      {/* Create key */}
      <Dialog open={creatingKey} onOpenChange={(v) => { setCreatingKey(v); if (!v) setFormError(null); }}>
        <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
          <DialogHeader className="text-left">
            <DialogTitle>Create a key</DialogTitle>
            <DialogDescription>For {serviceAccount.name}. You&apos;ll see the secret once, right after this.</DialogDescription>
          </DialogHeader>
          <form className="space-y-4" onSubmit={(e) => {
            e.preventDefault();
            const parsed = expiryFromLocalInput(expiry);
            if (!parsed.ok) { setFormError(parsed.reason); return; }
            setFormError(null);
            create.mutate(parsed.iso, { onSuccess: () => setCreatingKey(false) });
          }}>
            <Field label="Name (optional)">
              <input autoFocus className={inputClass} maxLength={120} placeholder="e.g. Production server" value={label} onChange={(e) => setLabel(e.target.value)} />
            </Field>
            <Field label="Expires (optional)" hint="Leave empty to use the longest time your workspace allows.">
              <input className={inputClass} type="datetime-local" value={expiry} onChange={(e) => setExpiry(e.target.value)} />
            </Field>
            <div className="space-y-2 text-sm">
              <p className="font-medium">What can this key do?</p>
              {available.length === 0 ? (
                <p className="rounded-xl bg-muted/60 px-3 py-2.5 text-xs text-muted-foreground">{projectId ? "This project has no permissions yet. Add them on the project's API permissions tab." : "Choose the project first."}</p>
              ) : (
                <>
                  <div className="flex flex-wrap gap-2">
                    {available.map((sc) => {
                      const on = chosen.includes(sc);
                      return (
                        <button key={sc} type="button" aria-pressed={on} onClick={() => setChosen((prev) => (on ? prev.filter((x) => x !== sc) : [...prev, sc]))}
                          className={cn("inline-flex h-8 items-center gap-1.5 rounded-full px-3 font-mono text-xs ring-1 transition-colors", on ? "bg-primary/10 text-primary ring-primary/40" : "ring-border hover:bg-muted")}>
                          {on && <Check className="size-3.5" />}{sc}
                        </button>
                      );
                    })}
                  </div>
                  <p className="text-xs text-muted-foreground">{chosen.length ? `Limited to ${chosen.length} permission${chosen.length === 1 ? "" : "s"}.` : "None picked: the key gets all of the project's permissions."}</p>
                </>
              )}
            </div>
            {formError && <p className="rounded-xl bg-red-500/10 px-3 py-2 text-sm text-red-600">{formError}</p>}
            <ErrorNotice error={create.error} />
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => setCreatingKey(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
              <Button type="submit" className="rounded-full" loading={create.isPending}>Create key</Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>

      {/* Rotate key */}
      <Dialog open={!!rotating} onOpenChange={(v) => { if (!v) setRotating(null); }}>
        <DialogContent className="sm:max-w-md">
          {rotating && (
            <form className="space-y-4" onSubmit={(e) => {
              e.preventDefault();
              const overlap = rotating.overlap.trim() === "" ? null : Number(rotating.overlap);
              rotate.mutate({ credential: rotating.credential, overlapMinutes: overlap, key: rotating.key });
            }}>
              <DialogHeader className="text-left">
                <DialogTitle>Rotate {rotating.credential.label ?? "key"}?</DialogTitle>
                <DialogDescription>You&apos;ll get a new secret. The old one keeps working for a while so you can swap it on your servers without downtime.</DialogDescription>
              </DialogHeader>
              <NiceSelect label="Keep the old key working for" value={rotating.overlap} onChange={(v) => setRotating({ ...rotating, overlap: v })} options={OVERLAP} />
              <ErrorNotice error={rotate.error} />
              <div className="flex justify-end gap-2 pt-1">
                <button type="button" onClick={() => setRotating(null)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
                <Button type="submit" className="rounded-full" loading={rotate.isPending}>Rotate key</Button>
              </div>
            </form>
          )}
        </DialogContent>
      </Dialog>

      {/* The one-time secret: a modal you can't close until you've saved it. */}
      <Dialog open={!!secret} onOpenChange={() => { /* closed only via Done */ }}>
        <DialogContent className="sm:max-w-lg [&>button]:hidden">
          <DialogHeader className="sr-only"><DialogTitle>{secret?.title ?? "API key"}</DialogTitle><DialogDescription>Copy and store this secret now.</DialogDescription></DialogHeader>
          {secret && (
            <SecretOnce
              title={secret.title}
              secret={secret.value}
              onDone={() => setSecret(null)}
              redactedHelp="This request was already completed earlier, so the secret isn't shown again. If you didn't save it, rotate the key to get a new one."
            />
          )}
        </DialogContent>
      </Dialog>
    </section>
  );
}

type MenuItem = { label: string; icon: typeof Pause; run: () => void; confirm?: string; danger?: boolean };

/** ⋯ menu for a row; items with `confirm` ask first in a small dialog. */
function RowMenu({ items }: { items: MenuItem[] }) {
  const [asking, setAsking] = useState<MenuItem | null>(null);
  if (!items.length) return null;
  const [question, ...rest] = (asking?.confirm ?? "").split(/(?<=\?)\s+/);
  return (
    <>
      <Popover.Root>
        <Popover.Trigger asChild>
          <button type="button" aria-label="More actions" className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreHorizontal className="size-5" /></button>
        </Popover.Trigger>
        <Popover.Portal>
          <Popover.Content align="end" sideOffset={4} className="z-50 w-48 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
            {items.map((it) => (
              <Popover.Close asChild key={it.label}>
                <button type="button" onClick={() => (it.confirm ? setAsking(it) : it.run())} className={cn("flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted", it.danger && "text-red-600")}>
                  <it.icon className="size-4" />{it.label}
                </button>
              </Popover.Close>
            ))}
          </Popover.Content>
        </Popover.Portal>
      </Popover.Root>
      <Dialog open={!!asking} onOpenChange={(v) => !v && setAsking(null)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader className="text-left">
            <DialogTitle className="text-lg">{question}</DialogTitle>
            <DialogDescription>{rest.join(" ")}</DialogDescription>
          </DialogHeader>
          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={() => setAsking(null)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" onClick={() => { asking?.run(); setAsking(null); }} className={cn("h-10 rounded-full px-5 text-sm font-semibold text-white", asking?.danger ? "bg-red-600 hover:bg-red-700" : "bg-primary hover:bg-primary/90")}>{asking?.label}</button>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}

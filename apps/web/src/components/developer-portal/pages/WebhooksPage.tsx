"use client";

import { useState } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { AlertTriangle, Check, CheckCircle2, ChevronDown, ChevronRight, MoreHorizontal, Pause, Play, Plus, RefreshCw, RotateCw, Search, Send, Trash2, Webhook as WebhookIcon, XCircle } from "lucide-react";
import { Popover } from "radix-ui";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { NiceSelect } from "@/components/ui/nice-select";
import { cn } from "@/lib/utils";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { portalHref, safeId } from "@/lib/developer-platform/navigation";
import { canReplayDelivery, DELIVERY_STATUS_HELP, validateWebhookUrl, webhookSecret, type OneTimeSecret } from "@/lib/developer-platform/presentation";
import { DELIVERY_STATUSES, type ConsoleWebhook, type ConsoleWebhookWithSecret, type WebhookAttempt, type WebhookDelivery, type Workspace } from "@/lib/developer-platform/types";
import { groupEventTypes } from "@/lib/developer-platform/webhook-events";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { EnvironmentPicker, useEnvironmentSelection } from "../EnvironmentPicker";
import { RequireWorkspace } from "../PortalShell";
import { Empty, ErrorNotice, Field, friendlyTime, inputClass, LoadMore, Loading, Mono, notify, NotPermitted, PageHeader, SecretOnce, StatusBadge, useConsoleMutation, usePagedList } from "../ui";

const nice = (v: string) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : v);

export default function WebhooksPage() {
  return (
    <RequireWorkspace>
      <Webhooks />
    </RequireWorkspace>
  );
}

function Webhooks() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  const { environmentId } = useEnvironmentSelection();
  return (
    <div className="space-y-6">
      <PageHeader title="Webhooks" description="CF Meet tells your server when things happen, like a meeting starting or a message arriving, by sending a signed request to your URL." />
      {!can("WebhooksRead") ? (
        <NotPermitted what="webhooks" />
      ) : (
        <>
          <EnvironmentPicker workspace={workspace} />
          {environmentId ? <Subscriptions key={environmentId} workspace={workspace} environmentId={environmentId} /> : <Empty>Choose a project and environment above to see its webhooks.</Empty>}
        </>
      )}
    </div>
  );
}

type Asking = { title: string; body: string; label: string; danger?: boolean; run: () => void };

function ConfirmDialog({ asking, onClose }: { asking: Asking | null; onClose: () => void }) {
  return (
    <Dialog open={!!asking} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader className="text-left"><DialogTitle className="text-lg">{asking?.title}</DialogTitle><DialogDescription>{asking?.body}</DialogDescription></DialogHeader>
        <div className="flex justify-end gap-2 pt-2">
          <button type="button" onClick={onClose} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
          <button type="button" onClick={() => { asking?.run(); onClose(); }} className={cn("h-10 rounded-full px-5 text-sm font-semibold text-white", asking?.danger ? "bg-red-600 hover:bg-red-700" : "bg-primary hover:bg-primary/90")}>{asking?.label}</button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function Subscriptions({ workspace, environmentId }: { workspace: Workspace; environmentId: string }) {
  const { client, can } = useDeveloperPortal();
  const router = useRouter();
  const pathname = usePathname();
  const params = useSearchParams();
  const selected = safeId(params.get("webhook"));
  const key = ["developer-portal", workspace.id, "webhooks", environmentId];
  const list = usePagedList<ConsoleWebhook>(key, { operation: "listEnvironmentWebhooks", path: { environmentId } });
  const [secret, setSecret] = useState<{ title: string; value: OneTimeSecret } | null>(null);
  const [creating, setCreating] = useState(false);
  const [asking, setAsking] = useState<Asking | null>(null);

  const update = useConsoleMutation(
    (v: { webhook: ConsoleWebhook; status: "ACTIVE" | "DISABLED" }) =>
      client.call<ConsoleWebhook>({ operation: "updateEnvironmentWebhook", path: { environmentId, webhookId: v.webhook.webhookId }, body: { expectedVersion: v.webhook.version, status: v.status } }),
    [key],
  );
  const remove = useConsoleMutation(
    (webhook: ConsoleWebhook) => client.call<void>({ operation: "deleteEnvironmentWebhook", path: { environmentId, webhookId: webhook.webhookId }, query: { expectedVersion: webhook.version } }),
    [key],
    undefined,
    "Webhook deleted.",
  );
  const rotate = useConsoleMutation(
    (webhook: ConsoleWebhook) => client.call<ConsoleWebhookWithSecret>({ operation: "rotateEnvironmentWebhookSecret", path: { environmentId, webhookId: webhook.webhookId }, body: { expectedVersion: webhook.version } }),
    [key],
    (result) => setSecret({ title: "New signing secret", value: webhookSecret(result) }),
    "",
  );
  // DF1: a test event, signed and delivered like a real one ("test": true in the payload).
  const sendTest = useConsoleMutation(
    (webhook: ConsoleWebhook) => client.call<WebhookDelivery>({ operation: "sendEnvironmentWebhookTest", path: { environmentId, webhookId: webhook.webhookId } }),
    [["developer-portal", workspace.id, "deliveries", environmentId]],
    undefined,
    "Test event sent. It'll show up in Deliveries in a moment.",
  );
  const current = list.items.find((w) => w.webhookId === selected) ?? null;
  const manage = can("WebhooksManage");
  const menu = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted";

  return (
    <div className="space-y-6">
      <section className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h2 className="text-lg font-semibold">Endpoints</h2>
          {manage && <button type="button" onClick={() => setCreating(true)} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-primary pl-3 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Add endpoint</button>}
        </div>
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-14 text-center">
            <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><WebhookIcon className="size-6" /></span>
            <p className="mt-4 font-semibold">No endpoints yet</p>
            <p className="mt-1 max-w-sm text-sm text-muted-foreground">Add your server&apos;s URL and pick which events to receive.</p>
            {manage && <button type="button" onClick={() => setCreating(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Add endpoint</button>}
          </div>
        ) : (
          <ul className="space-y-3">
            {list.items.map((w) => {
              const on = w.webhookId === selected;
              const failing = w.health.consecutiveFailures > 0 || !!w.health.disabledReason;
              return (
                <li key={w.webhookId} className={cn("rounded-2xl border bg-card p-4", on ? "border-primary/50 ring-1 ring-primary/30" : "border-border")}>
                  <div className="flex flex-wrap items-start gap-3">
                    <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-xl", failing ? "bg-red-500/10 text-red-600" : w.status === "ACTIVE" ? "bg-emerald-500/10 text-emerald-600" : "bg-muted text-muted-foreground")}><WebhookIcon className="size-5" /></span>
                    <div className="min-w-0 flex-1">
                      <p className="flex flex-wrap items-center gap-2"><span className="break-all font-mono text-sm font-semibold">{w.url}</span><StatusBadge status={w.status} /></p>
                      <p className="mt-0.5 text-sm text-muted-foreground">{w.description || `${w.eventTypes.length} event type${w.eventTypes.length === 1 ? "" : "s"}`}{w.origin === "MIGRATED_FROM_CALLING" ? " · moved from Calling" : ""}</p>
                      <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs">
                        <span className="inline-flex items-center gap-1 text-muted-foreground"><CheckCircle2 className="size-3.5 text-emerald-500" />Last success {friendlyTime(w.health.lastSuccessAt)}</span>
                        {w.health.lastFailureAt && <span className="inline-flex items-center gap-1 text-muted-foreground"><XCircle className="size-3.5 text-red-500" />Last failure {friendlyTime(w.health.lastFailureAt)}</span>}
                        {w.health.consecutiveFailures > 0 && <span className="font-semibold text-amber-700 dark:text-amber-300">{w.health.consecutiveFailures} failures in a row</span>}
                        {w.health.disabledReason && <span className="font-semibold text-red-600">Turned off after repeated failures</span>}
                      </p>
                    </div>
                    <div className="flex items-center gap-1">
                      <Button size="sm" variant={on ? "default" : "outline"} className="rounded-full" onClick={() => router.replace(portalHref(pathname, { project: params.get("project"), env: environmentId, webhook: on ? null : w.webhookId }))}>
                        Deliveries<ChevronRight className={cn("size-4 transition-transform", on && "rotate-90")} />
                      </Button>
                      {manage && w.status !== "DELETED" && (
                        <Popover.Root>
                          <Popover.Trigger asChild><button type="button" aria-label="More" className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreHorizontal className="size-5" /></button></Popover.Trigger>
                          <Popover.Portal>
                            <Popover.Content align="end" sideOffset={4} className="z-50 w-56 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                              {w.status === "ACTIVE" && <Popover.Close asChild><button type="button" className={menu} onClick={() => update.mutate({ webhook: w, status: "DISABLED" })}><Pause className="size-4 text-muted-foreground" />Pause deliveries</button></Popover.Close>}
                              {w.status === "DISABLED" && <Popover.Close asChild><button type="button" className={menu} onClick={() => update.mutate({ webhook: w, status: "ACTIVE" })}><Play className="size-4 text-muted-foreground" />Resume deliveries</button></Popover.Close>}
                              {w.status === "ACTIVE" && <Popover.Close asChild><button type="button" className={menu} disabled={sendTest.isPending} onClick={() => sendTest.mutate(w)}><Send className="size-4 text-muted-foreground" />Send test event</button></Popover.Close>}
                              <Popover.Close asChild><button type="button" className={menu} onClick={() => setAsking({ title: "Get a new signing secret?", body: "Your server must switch to the new secret to keep verifying requests.", label: "Rotate secret", run: () => rotate.mutate(w) })}><RefreshCw className="size-4 text-muted-foreground" />Rotate signing secret</button></Popover.Close>
                              <Popover.Close asChild><button type="button" className={cn(menu, "text-red-600")} onClick={() => setAsking({ title: "Delete this endpoint?", body: "CF Meet stops sending events to it. This can't be undone.", label: "Delete", danger: true, run: () => remove.mutate(w) })}><Trash2 className="size-4" />Delete</button></Popover.Close>
                            </Popover.Content>
                          </Popover.Portal>
                        </Popover.Root>
                      )}
                    </div>
                  </div>
                  {on && <Deliveries key={w.webhookId} workspace={workspace} environmentId={environmentId} webhook={w} />}
                </li>
              );
            })}
          </ul>
        )}
        <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        <ErrorNotice error={update.error} />
        <ErrorNotice error={rotate.error} />
        <ErrorNotice error={remove.error} />
      </section>
      {current === null && selected && !list.isLoading ? <Empty>That endpoint isn&apos;t in this environment.</Empty> : null}

      {manage && <CreateWebhook open={creating} onOpenChange={setCreating} environmentId={environmentId} invalidate={key} onSecret={(value) => setSecret({ title: "Signing secret", value })} />}
      <ConfirmDialog asking={asking} onClose={() => setAsking(null)} />
      <Dialog open={!!secret} onOpenChange={() => { /* closed only via Done */ }}>
        <DialogContent className="sm:max-w-lg [&>button]:hidden">
          <DialogHeader className="sr-only"><DialogTitle>{secret?.title ?? "Signing secret"}</DialogTitle><DialogDescription>Copy and store this secret now.</DialogDescription></DialogHeader>
          {secret && <SecretOnce title={secret.title} secret={secret.value} onDone={() => setSecret(null)} redactedHelp="This was already done earlier, so the secret isn't shown again. Rotate it if you didn't save it." />}
        </DialogContent>
      </Dialog>
    </div>
  );
}

function CreateWebhook({ open, onOpenChange, environmentId, invalidate, onSecret }: { open: boolean; onOpenChange: (v: boolean) => void; environmentId: string; invalidate: unknown[]; onSecret: (s: OneTimeSecret) => void }) {
  const { client } = useDeveloperPortal();
  const [url, setUrl] = useState("");
  const [description, setDescription] = useState("");
  const [events, setEvents] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const groups = groupEventTypes();
  const create = useConsoleMutation(
    () => client.call<ConsoleWebhookWithSecret>({ operation: "createEnvironmentWebhook", path: { environmentId }, body: { url: url.trim(), eventTypes: events, ...(description.trim() ? { description: description.trim() } : {}) }, idempotencyKey }),
    [invalidate],
    (result) => {
      onOpenChange(false);
      onSecret(webhookSecret(result));
      setUrl(""); setDescription(""); setEvents([]); setQuery("");
      setIdempotencyKey(newIdempotencyKey());
    },
    "",
  );
  const q = query.trim().toLowerCase();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] gap-0 overflow-hidden p-0 sm:max-w-2xl">
        <DialogHeader className="border-b border-border px-6 pb-4 pt-6 text-left">
          <DialogTitle>Add endpoint</DialogTitle>
          <DialogDescription>Where should CF Meet send events, and which ones?</DialogDescription>
        </DialogHeader>
        <form className="flex max-h-[calc(92dvh-6rem)] flex-col" onSubmit={(e) => {
          e.preventDefault();
          const urlError = validateWebhookUrl(url);
          if (urlError) return setFormError(urlError);
          if (events.length === 0) return setFormError("Pick at least one event.");
          if (events.length > 64) return setFormError("Pick at most 64 events.");
          setFormError(null);
          create.mutate(undefined);
        }}>
          <div className="no-scrollbar min-h-0 flex-1 space-y-5 overflow-y-auto px-6 py-5">
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Your URL" hint="Must be https:// and reachable from the internet.">
                <input autoFocus className={cn(inputClass, "font-mono")} type="url" required placeholder="https://api.yourapp.com/cfmeet" value={url} onChange={(e) => setUrl(e.target.value)} />
              </Field>
              <Field label="Description (optional)">
                <input className={inputClass} maxLength={500} placeholder="e.g. Booking updates" value={description} onChange={(e) => setDescription(e.target.value)} />
              </Field>
            </div>
            <div className="space-y-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-sm font-medium">Events <span className="text-muted-foreground">· {events.length} picked</span></p>
                <label className="flex h-9 w-full items-center gap-2 rounded-full bg-muted px-3 text-muted-foreground sm:w-56">
                  <Search className="size-4 shrink-0" />
                  <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search events" className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
                </label>
              </div>
              <div className="space-y-3">
                {Object.entries(groups).map(([family, types]) => {
                  const shown = types.filter((t) => !q || t.toLowerCase().includes(q));
                  if (!shown.length) return null;
                  const all = types.every((t) => events.includes(t));
                  return (
                    <div key={family} className="rounded-xl border border-border p-3">
                      <div className="mb-2 flex items-center justify-between">
                        <p className="text-sm font-semibold">{nice(family)}</p>
                        <button type="button" onClick={() => setEvents((prev) => (all ? prev.filter((t) => !types.includes(t)) : Array.from(new Set([...prev, ...types]))))} className="text-xs font-semibold text-primary hover:underline">{all ? "Clear" : "Select all"}</button>
                      </div>
                      <div className="flex flex-wrap gap-1.5">
                        {shown.map((t) => {
                          const on = events.includes(t);
                          return (
                            <button key={t} type="button" aria-pressed={on} onClick={() => setEvents((prev) => (on ? prev.filter((x) => x !== t) : [...prev, t]))}
                              className={cn("inline-flex h-8 items-center gap-1.5 rounded-full px-3 font-mono text-xs ring-1 transition-colors", on ? "bg-primary/10 text-primary ring-primary/40" : "ring-border hover:bg-muted")}>
                              {on && <Check className="size-3.5" />}{t}
                            </button>
                          );
                        })}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
            {formError && <p className="rounded-xl bg-red-500/10 px-3 py-2 text-sm text-red-600">{formError}</p>}
            <ErrorNotice error={create.error} />
          </div>
          <div className="flex justify-end gap-2 border-t border-border px-6 py-4">
            <button type="button" onClick={() => onOpenChange(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <Button type="submit" className="rounded-full" loading={create.isPending}>Add endpoint</Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function Deliveries({ workspace, environmentId, webhook }: { workspace: Workspace; environmentId: string; webhook: ConsoleWebhook }) {
  const { client, can } = useDeveloperPortal();
  const [status, setStatus] = useState("");
  const [openAttempts, setOpenAttempts] = useState<string | null>(null);
  const key = ["developer-portal", workspace.id, "deliveries", environmentId, webhook.webhookId, status];
  const list = usePagedList<WebhookDelivery>(key, { operation: "listEnvironmentWebhookDeliveries", path: { environmentId, webhookId: webhook.webhookId }, query: { status: status || undefined } });
  const replay = useConsoleMutation(
    (delivery: WebhookDelivery) => client.call<WebhookDelivery>({ operation: "replayEnvironmentWebhookDelivery", path: { environmentId, deliveryId: delivery.deliveryId } }),
    [["developer-portal", workspace.id, "deliveries", environmentId, webhook.webhookId]],
    () => notify("Sent again. It'll show up in the list in a moment."),
    "",
  );
  return (
    <div className="mt-4 space-y-3 border-t border-border pt-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm font-semibold">Recent deliveries</p>
        <NiceSelect className="w-44" value={status} onChange={setStatus} options={[{ value: "", label: "All results" }, ...DELIVERY_STATUSES.map((s) => ({ value: s, label: nice(s) }))]} />
      </div>
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : list.items.length === 0 ? (
        <p className="rounded-xl bg-muted/50 px-4 py-6 text-center text-sm text-muted-foreground">{status ? `Nothing ${nice(status).toLowerCase()} yet.` : "No events sent yet. They'll appear here as things happen."}</p>
      ) : (
        <ul className="divide-y divide-border overflow-hidden rounded-xl border border-border">
          {list.items.map((d) => {
            const open = openAttempts === d.deliveryId;
            const ok = d.status === "SUCCEEDED";
            const bad = d.status === "FAILED";
            const replayable = can("WebhooksManage") && canReplayDelivery(webhook.status, d);
            return (
              <li key={d.deliveryId}>
                <div className="flex flex-wrap items-center gap-3 px-3 py-2.5">
                  <button type="button" onClick={() => setOpenAttempts(open ? null : d.deliveryId)} aria-expanded={open} className="flex min-w-0 flex-1 items-center gap-3 text-left">
                    {ok ? <CheckCircle2 className="size-5 shrink-0 text-emerald-500" /> : bad ? <XCircle className="size-5 shrink-0 text-red-500" /> : <AlertTriangle className="size-5 shrink-0 text-amber-500" />}
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-mono text-sm">{d.eventType}</span>
                      <span className="block truncate text-xs text-muted-foreground" title={DELIVERY_STATUS_HELP[d.status]}>
                        {nice(d.status)} · {d.attemptCount} {d.attemptCount === 1 ? "try" : "tries"}{d.lastHttpStatus != null ? ` · HTTP ${d.lastHttpStatus}` : ""} · {friendlyTime(d.createdAt)}{d.replayOf ? " · sent again" : ""}
                      </span>
                    </span>
                    <ChevronDown className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} />
                  </button>
                  {replayable && (
                    <button type="button" disabled={replay.isPending} onClick={() => replay.mutate(d)} className="inline-flex h-8 items-center gap-1.5 rounded-full px-3 text-xs font-semibold text-primary hover:bg-primary/10 disabled:opacity-50"><RotateCw className="size-3.5" />Send again</button>
                  )}
                </div>
                {open && (
                  <div className="bg-muted/40 px-3 py-3 text-xs">
                    <p className="mb-2 text-muted-foreground">{DELIVERY_STATUS_HELP[d.status]}{d.failureCategory ? ` (${nice(d.failureCategory)})` : ""}</p>
                    <p className="mb-2">Event <Mono>{d.eventId}</Mono></p>
                    <Attempts environmentId={environmentId} deliveryId={d.deliveryId} />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
      <ErrorNotice error={replay.error} />
    </div>
  );
}

function Attempts({ environmentId, deliveryId }: { environmentId: string; deliveryId: string }) {
  const { workspace } = useDeveloperPortal();
  const list = usePagedList<WebhookAttempt>(["developer-portal", workspace?.id, "attempts", environmentId, deliveryId], { operation: "listWebhookDeliveryAttempts", path: { environmentId, deliveryId } });
  if (list.isLoading) return <p className="text-muted-foreground">Loading tries…</p>;
  if (list.error) return <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />;
  if (list.items.length === 0) return <p className="text-muted-foreground">Not tried yet.</p>;
  return (
    <ol className="space-y-1.5">
      {list.items.map((a) => (
        <li key={a.attemptNumber} className="flex flex-wrap items-center gap-2">
          <span className="flex size-5 items-center justify-center rounded-full bg-background text-[10px] font-semibold ring-1 ring-border">{a.attemptNumber}</span>
          <StatusBadge status={a.outcome === "RETRY" ? "RETRY_SCHEDULED" : a.outcome} />
          <span>{a.httpStatus != null ? `HTTP ${a.httpStatus}` : "no answer"}</span>
          {a.errorClass && <span className="text-muted-foreground">{nice(a.errorClass)}</span>}
          <span className="text-muted-foreground">{friendlyTime(a.startedAt)}</span>
        </li>
      ))}
    </ol>
  );
}

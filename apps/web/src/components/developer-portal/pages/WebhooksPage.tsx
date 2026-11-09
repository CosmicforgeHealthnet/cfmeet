"use client";

import { useState } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { Button } from "@/components/ui/button";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { portalHref, safeId } from "@/lib/developer-platform/navigation";
import {
  canReplayDelivery,
  DELIVERY_STATUS_HELP,
  formatTimestamp,
  validateWebhookUrl,
  webhookSecret,
  type OneTimeSecret,
} from "@/lib/developer-platform/presentation";
import { DELIVERY_STATUSES, type ConsoleWebhook, type ConsoleWebhookWithSecret, type WebhookAttempt, type WebhookDelivery, type Workspace } from "@/lib/developer-platform/types";
import { groupEventTypes } from "@/lib/developer-platform/webhook-events";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { EnvironmentPicker, useEnvironmentSelection } from "../EnvironmentPicker";
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
  NotPermitted,
  PageHeader,
  SecretOnce,
  Section,
  StatusBadge,
  Table,
  Td,
  useConsoleMutation,
  usePagedList,
} from "../ui";

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
      <PageHeader
        title="Webhooks"
        description="Signed event delivery to your HTTPS endpoint, with delivery history, attempts and replay. Verify every request with the subscription's signing secret."
      />
      {!can("WebhooksRead") ? (
        <NotPermitted what="webhooks" />
      ) : (
        <>
          <EnvironmentPicker workspace={workspace} />
          {environmentId ? <Subscriptions key={environmentId} workspace={workspace} environmentId={environmentId} /> : <Empty>Select a project and environment.</Empty>}
        </>
      )}
    </div>
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

  const update = useConsoleMutation(
    (v: { webhook: ConsoleWebhook; status: "ACTIVE" | "DISABLED" }) =>
      client.call<ConsoleWebhook>({
        operation: "updateEnvironmentWebhook",
        path: { environmentId, webhookId: v.webhook.webhookId },
        body: { expectedVersion: v.webhook.version, status: v.status },
      }),
    [key],
  );
  const remove = useConsoleMutation(
    (webhook: ConsoleWebhook) =>
      client.call<void>({
        operation: "deleteEnvironmentWebhook",
        path: { environmentId, webhookId: webhook.webhookId },
        query: { expectedVersion: webhook.version },
      }),
    [key],
  );
  const rotate = useConsoleMutation(
    (webhook: ConsoleWebhook) =>
      client.call<ConsoleWebhookWithSecret>({
        operation: "rotateEnvironmentWebhookSecret",
        path: { environmentId, webhookId: webhook.webhookId },
        body: { expectedVersion: webhook.version },
      }),
    [key],
    (result) => setSecret({ title: "New signing secret", value: webhookSecret(result) }),
  );
  const current = list.items.find((w) => w.webhookId === selected) ?? null;
  const manage = can("WebhooksManage");

  return (
    <>
      <Section title="Subscriptions">
        <div className="space-y-4">
          {secret && (
            <SecretOnce
              title={secret.title}
              secret={secret.value}
              onDone={() => setSecret(null)}
              redactedHelp="This request was already completed earlier, so the signing secret is not returned again. Rotate the secret if you did not store it."
            />
          )}
          {list.isLoading ? (
            <Loading />
          ) : list.error ? (
            <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
          ) : list.items.length === 0 ? (
            <Empty>No webhook subscriptions in this environment.</Empty>
          ) : (
            <Table head={["Endpoint", "Status", "Events", "Health", "Actions"]}>
              {list.items.map((w) => (
                <tr key={w.webhookId} className={w.webhookId === selected ? "bg-primary/5" : undefined}>
                  <Td>
                    <Mono>{w.url}</Mono>
                    {w.description && <p className="text-xs text-muted-foreground">{w.description}</p>}
                    {w.origin === "MIGRATED_FROM_CALLING" && <p className="text-xs text-muted-foreground">Migrated from Calling webhooks</p>}
                  </Td>
                  <Td>
                    <StatusBadge status={w.status} />
                  </Td>
                  <Td>
                    <span className="text-xs">{w.eventTypes.length} event types</span>
                  </Td>
                  <Td className="text-xs text-muted-foreground">
                    <p>Last success: {formatTimestamp(w.health.lastSuccessAt)}</p>
                    <p>Last failure: {formatTimestamp(w.health.lastFailureAt)}</p>
                    {w.health.consecutiveFailures > 0 && <p className="text-amber-600">{w.health.consecutiveFailures} consecutive failures</p>}
                    {w.health.disabledReason && <p className="text-red-600">Disabled after repeated failures</p>}
                  </Td>
                  <Td className="space-y-1">
                    <Button size="sm" variant="outline" onClick={() => router.replace(portalHref(pathname, { project: params.get("project"), env: environmentId, webhook: w.webhookId }))}>
                      Deliveries
                    </Button>
                    {manage && w.status === "ACTIVE" && (
                      <Button size="sm" variant="ghost" loading={update.isPending} onClick={() => update.mutate({ webhook: w, status: "DISABLED" })}>
                        Disable
                      </Button>
                    )}
                    {manage && w.status === "DISABLED" && (
                      <Button size="sm" variant="ghost" loading={update.isPending} onClick={() => update.mutate({ webhook: w, status: "ACTIVE" })}>
                        Enable
                      </Button>
                    )}
                    {manage && w.status !== "DELETED" && (
                      <ConfirmButton
                        label="Rotate secret"
                        message="Issue a new signing secret? Update your endpoint's verification."
                        pending={rotate.isPending}
                        onConfirm={() => rotate.mutate(w)}
                      />
                    )}
                    {manage && w.status !== "DELETED" && (
                      <ConfirmButton label="Delete" message="Delete this subscription?" destructive pending={remove.isPending} onConfirm={() => remove.mutate(w)} />
                    )}
                  </Td>
                </tr>
              ))}
            </Table>
          )}
          <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
          <ErrorNotice error={update.error} />
          <ErrorNotice error={rotate.error} />
          <ErrorNotice error={remove.error} />
          {manage && <CreateWebhook environmentId={environmentId} invalidate={key} onSecret={(value) => setSecret({ title: "Signing secret", value })} />}
        </div>
      </Section>
      {current && <Deliveries key={current.webhookId} workspace={workspace} environmentId={environmentId} webhook={current} />}
    </>
  );
}

function CreateWebhook({ environmentId, invalidate, onSecret }: { environmentId: string; invalidate: unknown[]; onSecret: (s: OneTimeSecret) => void }) {
  const { client } = useDeveloperPortal();
  const [url, setUrl] = useState("");
  const [description, setDescription] = useState("");
  const [events, setEvents] = useState<string[]>([]);
  const [formError, setFormError] = useState<string | null>(null);
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const groups = groupEventTypes();
  const create = useConsoleMutation(
    () =>
      client.call<ConsoleWebhookWithSecret>({
        operation: "createEnvironmentWebhook",
        path: { environmentId },
        body: { url: url.trim(), eventTypes: events, ...(description.trim() ? { description: description.trim() } : {}) },
        idempotencyKey,
      }),
    [invalidate],
    (result) => {
      onSecret(webhookSecret(result));
      setUrl("");
      setDescription("");
      setEvents([]);
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const toggle = (type: string, on: boolean) => setEvents((prev) => (on ? [...prev, type] : prev.filter((t) => t !== type)));
  return (
    <form
      className="space-y-3 rounded-lg border border-border p-4"
      onSubmit={(e) => {
        e.preventDefault();
        const urlError = validateWebhookUrl(url);
        if (urlError) return setFormError(urlError);
        if (events.length === 0) return setFormError("Select at least one event type.");
        if (events.length > 64) return setFormError("Select at most 64 event types.");
        setFormError(null);
        create.mutate(undefined);
      }}
    >
      <p className="text-sm font-medium">Add a subscription</p>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Endpoint URL" hint="HTTPS, publicly reachable. Private networks are refused by the platform.">
          <input className={inputClass} type="url" required placeholder="https://example.com/cfmeet/webhooks" value={url} onChange={(e) => setUrl(e.target.value)} />
        </Field>
        <Field label="Description (optional)">
          <input className={inputClass} maxLength={500} value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
      </div>
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Event types ({events.length} selected)</legend>
        <div className="grid max-h-72 gap-3 overflow-y-auto rounded-lg border border-border p-3 sm:grid-cols-2 lg:grid-cols-3">
          {Object.entries(groups).map(([family, types]) => (
            <div key={family} className="space-y-1 text-xs">
              <label className="flex items-center gap-1.5 font-semibold uppercase tracking-wide text-muted-foreground">
                <input
                  type="checkbox"
                  checked={types.every((t) => events.includes(t))}
                  onChange={(e) => setEvents((prev) => (e.target.checked ? Array.from(new Set([...prev, ...types])) : prev.filter((t) => !types.includes(t))))}
                />
                {family}
              </label>
              {types.map((t) => (
                <label key={t} className="flex items-center gap-1.5 pl-4">
                  <input type="checkbox" checked={events.includes(t)} onChange={(e) => toggle(t, e.target.checked)} />
                  <Mono>{t}</Mono>
                </label>
              ))}
            </div>
          ))}
        </div>
      </fieldset>
      {formError && <p className="text-sm text-red-600">{formError}</p>}
      <Button type="submit" size="sm" loading={create.isPending}>
        Add subscription
      </Button>
      <ErrorNotice error={create.error} />
    </form>
  );
}

function Deliveries({ workspace, environmentId, webhook }: { workspace: Workspace; environmentId: string; webhook: ConsoleWebhook }) {
  const { client, can } = useDeveloperPortal();
  const [status, setStatus] = useState("");
  const [openAttempts, setOpenAttempts] = useState<string | null>(null);
  const key = ["developer-portal", workspace.id, "deliveries", environmentId, webhook.webhookId, status];
  const list = usePagedList<WebhookDelivery>(key, {
    operation: "listEnvironmentWebhookDeliveries",
    path: { environmentId, webhookId: webhook.webhookId },
    query: { status: status || undefined },
  });
  const replay = useConsoleMutation(
    (delivery: WebhookDelivery) =>
      client.call<WebhookDelivery>({ operation: "replayEnvironmentWebhookDelivery", path: { environmentId, deliveryId: delivery.deliveryId } }),
    [["developer-portal", workspace.id, "deliveries", environmentId, webhook.webhookId]],
  );
  return (
    <Section
      title="Deliveries"
      description={<Mono>{webhook.url}</Mono>}
      actions={
        <select aria-label="Filter by status" className="rounded-md border border-border bg-background px-2 py-1 text-sm" value={status} onChange={(e) => setStatus(e.target.value)}>
          <option value="">All statuses</option>
          {DELIVERY_STATUSES.map((s) => (
            <option key={s} value={s}>
              {s}
            </option>
          ))}
        </select>
      }
    >
      <div className="space-y-3">
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <Empty>No deliveries{status ? ` with status ${status}` : ""}.</Empty>
        ) : (
          <Table head={["Event", "Status", "Attempts", "Created", ""]}>
            {list.items.map((d) => (
              <DeliveryRow
                key={d.deliveryId}
                environmentId={environmentId}
                delivery={d}
                open={openAttempts === d.deliveryId}
                onToggle={() => setOpenAttempts(openAttempts === d.deliveryId ? null : d.deliveryId)}
                replayable={can("WebhooksManage") && canReplayDelivery(webhook.status, d)}
                replaying={replay.isPending && replay.variables?.deliveryId === d.deliveryId}
                onReplay={() => replay.mutate(d)}
              />
            ))}
          </Table>
        )}
        <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        <ErrorNotice error={replay.error} />
        {replay.data && <p className="text-sm text-muted-foreground">Replay queued as delivery {replay.data.deliveryId} ({replay.data.status}).</p>}
      </div>
    </Section>
  );
}

function DeliveryRow({
  environmentId,
  delivery,
  open,
  onToggle,
  replayable,
  replaying,
  onReplay,
}: {
  environmentId: string;
  delivery: WebhookDelivery;
  open: boolean;
  onToggle: () => void;
  replayable: boolean;
  replaying: boolean;
  onReplay: () => void;
}) {
  return (
    <>
      <tr>
        <Td>
          <Mono>{delivery.eventType}</Mono>
          <p className="text-xs text-muted-foreground">
            <Mono>{delivery.eventId}</Mono>
          </p>
          {delivery.replayOf && <p className="text-xs text-muted-foreground">Replay of {delivery.replayOf}</p>}
        </Td>
        <Td>
          <StatusBadge status={delivery.status} />
          <p className="mt-1 max-w-[14rem] text-xs text-muted-foreground">{DELIVERY_STATUS_HELP[delivery.status]}</p>
        </Td>
        <Td>
          {delivery.attemptCount}
          {delivery.lastHttpStatus != null && <span className="text-xs text-muted-foreground"> · HTTP {delivery.lastHttpStatus}</span>}
          {delivery.failureCategory && <p className="text-xs text-muted-foreground">{delivery.failureCategory}</p>}
        </Td>
        <Td>{formatTimestamp(delivery.createdAt)}</Td>
        <Td className="space-y-1 whitespace-nowrap">
          <Button size="sm" variant="ghost" onClick={onToggle}>
            {open ? "Hide attempts" : "Attempts"}
          </Button>
          {replayable && (
            <ConfirmButton label="Replay" message="Send this event again as a new delivery?" pending={replaying} onConfirm={onReplay} />
          )}
        </Td>
      </tr>
      {open && (
        <tr>
          <td colSpan={5} className="bg-muted/30 px-3 py-2">
            <Attempts environmentId={environmentId} deliveryId={delivery.deliveryId} />
          </td>
        </tr>
      )}
    </>
  );
}

function Attempts({ environmentId, deliveryId }: { environmentId: string; deliveryId: string }) {
  const { workspace } = useDeveloperPortal();
  const list = usePagedList<WebhookAttempt>(["developer-portal", workspace?.id, "attempts", environmentId, deliveryId], {
    operation: "listWebhookDeliveryAttempts",
    path: { environmentId, deliveryId },
  });
  if (list.isLoading) return <Loading />;
  if (list.error) return <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />;
  if (list.items.length === 0) return <p className="text-xs text-muted-foreground">No attempts yet.</p>;
  return (
    <ul className="space-y-1 text-xs">
      {list.items.map((a) => (
        <li key={a.attemptNumber} className="flex flex-wrap gap-3">
          <span className="font-medium">#{a.attemptNumber}</span>
          <StatusBadge status={a.outcome === "RETRY" ? "RETRY_SCHEDULED" : a.outcome} />
          <span>{a.httpStatus != null ? `HTTP ${a.httpStatus}` : "no HTTP answer"}</span>
          {a.errorClass && <span className="text-muted-foreground">{a.errorClass}</span>}
          <span className="text-muted-foreground">{formatTimestamp(a.startedAt)}</span>
        </li>
      ))}
    </ul>
  );
}

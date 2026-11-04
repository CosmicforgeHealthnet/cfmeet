"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { callingErrorMessage } from "@/lib/calling/errors";
import { callingResourcesService, type CallingResourceMethod } from "@/services/calling/resources.service";

type ResourceAction = {
  label: string;
  method: "GET" | "POST" | "PUT";
  pathSuffix?: string;
  pathTemplate?: string;
  targetIdLabel?: string;
  needsBody?: boolean;
  confirmation?: string;
};

export type CallingResourceDefinition = {
  title: string;
  description: string;
  listPath?: string;
  detailPath?: string;
  detailHref?: string;
  idKey?: string;
  listQuery?: { name: string; label: string };
  createPath?: string;
  updateMethod?: "PATCH" | "PUT";
  deletePath?: string;
  actions?: ResourceAction[];
  relatedLinks?: Array<{ label: string; href: string }>;
  docsHref?: string;
};

type JsonRecord = Record<string, unknown>;

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function recordsIn(value: unknown): JsonRecord[] {
  if (Array.isArray(value)) return value.filter(isRecord);
  if (!isRecord(value)) return [];
  for (const key of ["data", "items", "contexts", "sessions", "results"]) {
    const nested = value[key];
    if (Array.isArray(nested)) return nested.filter(isRecord);
  }
  return [];
}

function printable(value: unknown): string {
  const redact = (item: unknown): unknown => {
    if (Array.isArray(item)) return item.map(redact);
    if (!isRecord(item)) return item;
    return Object.fromEntries(Object.entries(item).map(([key, nested]) => [
      key,
      /(token|secret|credential|authorization|phone(number)?|telephone|(?:media|storage|signed|access).*?(url|uri)|^url$)/i.test(key)
        ? "[redacted]"
        : redact(nested),
    ]));
  };
  return JSON.stringify(redact(value), null, 2);
}

function apiPath(template: string, id: string): string {
  return template.replace("{id}", encodeURIComponent(id));
}

export default function CallingResourcePage({
  definition,
  detailId,
}: {
  definition: CallingResourceDefinition;
  detailId?: string;
}) {
  const router = useRouter();
  const queryClient = useQueryClient();
  const [payload, setPayload] = useState("{}");
  const [lookupId, setLookupId] = useState("");
  const [listFilter, setListFilter] = useState("");
  const [listFilterInput, setListFilterInput] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [actionResult, setActionResult] = useState<unknown>();
  const [actionPayload, setActionPayload] = useState("{}");
  const [actionTargetId, setActionTargetId] = useState("");
  const listPath = definition.listPath && definition.listQuery && listFilter.trim()
    ? `${definition.listPath}?${encodeURIComponent(definition.listQuery.name)}=${encodeURIComponent(listFilter.trim())}`
    : definition.listPath;
  const list = useQuery({
    queryKey: ["calling", "resource", listPath],
    queryFn: () => callingResourcesService.request("GET", listPath!),
    enabled: !!listPath && !detailId && (!definition.listQuery || !!listFilter.trim()),
    retry: false,
  });
  const detailPath = definition.detailPath && detailId
    ? apiPath(definition.detailPath, detailId)
    : undefined;
  const detail = useQuery({
    queryKey: ["calling", "resource", detailPath],
    queryFn: () => callingResourcesService.request("GET", detailPath!),
    enabled: !!detailPath,
    retry: false,
  });

  const mutation = useMutation({
    mutationFn: ({ method, path, body }: { method: CallingResourceMethod; path: string; body?: JsonRecord }) =>
      callingResourcesService.request(method, path, body),
    onSuccess: async (result) => {
      setError("");
      setMessage("Request completed.");
      setActionResult(result);
      await queryClient.invalidateQueries({ queryKey: ["calling", "resource"] });
    },
    onError: (cause) => {
      setMessage("");
      setError(callingErrorMessage(cause));
    },
  });

  const runMutation = (method: CallingResourceMethod, path: string, rawBody?: string) => {
    setError("");
    setMessage("");
    try {
      let body: JsonRecord | undefined;
      if (rawBody !== undefined) {
        const parsed: unknown = JSON.parse(rawBody);
        if (!isRecord(parsed)) throw new Error("The request body must be a JSON object.");
        body = parsed;
      }
      mutation.mutate({ method, path, body });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Invalid request body.");
    }
  };

  const rows = recordsIn(list.data);
  const detailHref = definition.detailHref;
  const hasChanges = !!definition.createPath && !detailId;

  return (
    <div className="mx-auto max-w-6xl space-y-5 p-4 md:p-6">
      <header className="space-y-2">
        <h1 className="text-2xl font-bold tracking-tight">{definition.title}</h1>
        <p className="max-w-3xl text-sm text-muted-foreground">{definition.description}</p>
        <div className="flex flex-wrap gap-2">
          {definition.relatedLinks?.map((link) => (
            <Button asChild key={link.href} variant="outline" size="sm"><Link href={link.href}>{link.label}</Link></Button>
          ))}
          {definition.docsHref ? <Button asChild variant="ghost" size="sm"><a href={definition.docsHref} target="_blank" rel="noreferrer">API contract</a></Button> : null}
          {detailId && definition.listPath ? <Button asChild variant="ghost" size="sm"><Link href={definition.detailHref?.replace(/\/$/, "") ?? "#"}>Back to list</Link></Button> : null}
          {!detailId && definition.detailPath && definition.detailHref ? (
            <form className="flex flex-wrap gap-2" onSubmit={(event) => {
              event.preventDefault();
              if (lookupId.trim()) router.push(`${definition.detailHref!.replace(/\/$/, "")}/${encodeURIComponent(lookupId.trim())}`);
            }}>
              <input aria-label="Resource identifier" className="h-9 w-56 rounded-md border border-input bg-background px-3 text-sm" value={lookupId} onChange={(event) => setLookupId(event.target.value)} placeholder="Enter an ID / reference" />
              <Button size="sm" type="submit" disabled={!lookupId.trim()}>Look up</Button>
            </form>
          ) : null}
        </div>
      </header>

      {error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error}</p> : null}
      {message ? <p role="status" className="rounded-xl border border-primary/30 bg-primary/5 p-3 text-sm">{message}</p> : null}

      {detailId ? (
        <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
          <h2 className="font-semibold">Resource details</h2>
          {detail.isPending ? <Spinner /> : detail.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(detail.error)}</p> : <pre className="max-h-[34rem] overflow-auto rounded-xl bg-muted p-4 text-xs">{printable(detail.data)}</pre>}
          {definition.deletePath ? <Button variant="destructive" disabled={mutation.isPending} onClick={() => {
            if (window.confirm("Disable or delete this resource? This action may not be reversible.")) runMutation("DELETE", apiPath(definition.deletePath!, detailId));
          }}>Disable / delete</Button> : null}
          {definition.updateMethod ? (
            <div className="space-y-2 border-t border-border pt-4">
              <label className="block text-sm font-medium" htmlFor="resource-update-json">Update payload (JSON)</label>
              <textarea id="resource-update-json" className="min-h-36 w-full rounded-md border border-input bg-background p-3 font-mono text-xs" value={payload} onChange={(event) => setPayload(event.target.value)} spellCheck={false} autoComplete="off" />
              <Button disabled={mutation.isPending} onClick={() => runMutation(definition.updateMethod!, apiPath(definition.detailPath!, detailId), payload)}>Save changes</Button>
            </div>
          ) : null}
          {definition.actions?.map((action) => (
            <div key={action.pathSuffix ?? action.pathTemplate} className="space-y-2 border-t border-border pt-4">
              <p className="text-sm font-medium">{action.label}</p>
              {action.targetIdLabel ? <label className="block space-y-1 text-sm"><span>{action.targetIdLabel}</span><input className="h-9 w-full rounded-md border border-input bg-background px-3" value={actionTargetId} onChange={(event) => setActionTargetId(event.target.value)} required /></label> : null}
              {action.needsBody ? <textarea aria-label={`${action.label} JSON`} className="min-h-28 w-full rounded-md border border-input bg-background p-3 font-mono text-xs" value={actionPayload} onChange={(event) => setActionPayload(event.target.value)} spellCheck={false} autoComplete="off" /> : null}
              <Button variant="outline" disabled={mutation.isPending || (!!action.targetIdLabel && !actionTargetId.trim())} onClick={() => {
                if (action.confirmation && !window.confirm(action.confirmation)) return;
                const path = action.pathTemplate
                  ? action.pathTemplate.replace("{targetId}", encodeURIComponent(actionTargetId.trim()))
                  : `${apiPath(definition.detailPath!, detailId)}${action.pathSuffix ?? ""}`;
                runMutation(action.method, path, action.needsBody ? actionPayload : undefined);
              }}>{action.label}</Button>
              {actionResult !== undefined ? <pre className="max-h-64 overflow-auto rounded-xl bg-muted p-3 text-xs">{printable(actionResult)}</pre> : null}
            </div>
          ))}
        </section>
      ) : definition.listPath ? (
        <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
          <div className="flex items-center justify-between gap-3">
            <h2 className="font-semibold">Available records</h2>
            <Button variant="outline" size="sm" disabled={list.isFetching} onClick={() => void list.refetch()}>Refresh</Button>
          </div>
          {definition.listQuery ? <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => {
            event.preventDefault();
            setListFilter(listFilterInput.trim());
          }}><label className="flex-1 space-y-1 text-sm"><span>{definition.listQuery.label}</span><input className="h-9 w-full rounded-md border border-input bg-background px-3" value={listFilterInput} onChange={(event) => setListFilterInput(event.target.value)} required /></label><Button type="submit" size="sm" disabled={!listFilterInput.trim()}>Load records</Button></form> : null}
          {definition.listQuery && !listFilter.trim() ? <p className="py-5 text-sm text-muted-foreground">Enter the required identifier to load this list.</p> : list.isPending ? <Spinner /> : list.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(list.error)}</p> : rows.length === 0 && isRecord(list.data) ? <pre className="max-h-[34rem] overflow-auto rounded-xl bg-muted p-4 text-xs">{printable(list.data)}</pre> : rows.length === 0 ? <p className="py-5 text-sm text-muted-foreground">No records were returned for this account.</p> : (
            <ul className="divide-y divide-border">
              {rows.map((row, index) => {
                const id = definition.idKey ? row[definition.idKey] : undefined;
                const name = row.name ?? row.title ?? row.displayName ?? row.label ?? row.status ?? `Record ${index + 1}`;
                const href = detailHref && typeof id === "string" ? `${detailHref.replace(/\/$/, "")}/${encodeURIComponent(id)}` : undefined;
                return (
                  <li key={typeof id === "string" ? id : index} className="flex items-center justify-between gap-3 py-3">
                    <div className="min-w-0">
                      <p className="truncate text-sm font-medium">{String(name)}</p>
                      <p className="truncate text-xs text-muted-foreground">{typeof id === "string" ? id : "Identifier unavailable"}</p>
                    </div>
                    {href ? <Button asChild size="sm" variant="outline"><Link href={href}>Open</Link></Button> : <Button size="sm" variant="ghost" onClick={() => setActionResult(row)}>View summary</Button>}
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      ) : null}

      {actionResult !== undefined && !detailId ? <pre className="max-h-64 overflow-auto rounded-xl bg-muted p-4 text-xs">{printable(actionResult)}</pre> : null}

      {hasChanges ? (
        <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
          <h2 className="font-semibold">{detailId ? "Update resource" : "Create resource"}</h2>
          <p className="text-xs text-muted-foreground">Enter the JSON request body defined by the API contract. Changes are sent only after you submit and are authorized by the Calling service.</p>
          {!detailId && definition.createPath ? (
            <>
              <label className="block text-sm font-medium" htmlFor="resource-create-json">Create payload (JSON)</label>
              <textarea id="resource-create-json" className="min-h-40 w-full rounded-md border border-input bg-background p-3 font-mono text-xs" value={payload} onChange={(event) => setPayload(event.target.value)} spellCheck={false} autoComplete="off" />
              <Button disabled={mutation.isPending} onClick={() => runMutation("POST", definition.createPath!, payload)}>Create</Button>
            </>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}

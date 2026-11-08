"use client";

/**
 * Small, console-style building blocks of the Developer Portal: sections, badges, error
 * notices with the gateway request id, paged lists, confirmations and the one-time
 * secret panel.
 */
import { useState } from "react";
import Link from "next/link";
import { AlertTriangle, Check, Copy, KeyRound, Loader2, ShieldAlert } from "lucide-react";
import { useInfiniteQuery, useMutation, useQueryClient, type QueryKey } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { RequestSpec } from "@/lib/developer-platform/client";
import {
  describeError,
  retryDelayMs,
  shouldRetryQuery,
  statusTone,
  type Badge as BadgeModel,
  type OneTimeSecret,
} from "@/lib/developer-platform/presentation";
import type { Page } from "@/lib/developer-platform/types";
import { useDeveloperPortal } from "./DeveloperPortalProvider";

export function PageHeader({ title, description, actions }: { title: string; description?: string; actions?: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-3 border-b border-border pb-5 sm:flex-row sm:items-end sm:justify-between">
      <div>
        <h1 className="text-2xl font-semibold text-foreground">{title}</h1>
        {description && <p className="mt-1 max-w-3xl text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex flex-wrap gap-2">{actions}</div>}
    </div>
  );
}

export function Section({
  title,
  description,
  actions,
  children,
  className,
}: {
  title: string;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("rounded-xl border border-border bg-card", className)}>
      <div className="flex flex-col gap-2 border-b border-border px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-base font-semibold text-foreground">{title}</h2>
          {description && <div className="mt-0.5 text-sm text-muted-foreground">{description}</div>}
        </div>
        {actions && <div className="flex flex-wrap gap-2">{actions}</div>}
      </div>
      <div className="p-5">{children}</div>
    </section>
  );
}

const TONES: Record<BadgeModel["tone"], string> = {
  ok: "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300 border-emerald-500/30",
  warn: "bg-amber-500/10 text-amber-700 dark:text-amber-300 border-amber-500/30",
  bad: "bg-red-500/10 text-red-700 dark:text-red-300 border-red-500/30",
  muted: "bg-muted text-muted-foreground border-border",
};

export function Badge({ label, tone }: BadgeModel) {
  return <span className={cn("inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium", TONES[tone])}>{label}</span>;
}

export function StatusBadge({ status }: { status: string | null | undefined }) {
  return <Badge label={(status ?? "UNKNOWN").replace(/_/g, " ")} tone={statusTone(status)} />;
}

export function ErrorNotice({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (!error) return null;
  const view = describeError(error);
  return (
    <div role="alert" className="flex gap-3 rounded-lg border border-red-500/30 bg-red-500/5 px-4 py-3 text-sm">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-red-600" />
      <div className="space-y-1">
        <p className="font-medium text-foreground">{view.title}</p>
        {view.detail && <p className="text-muted-foreground">{view.detail}</p>}
        {view.requestId && (
          <p className="font-mono text-xs text-muted-foreground">
            Request ID: <span className="select-all">{view.requestId}</span>
          </p>
        )}
        <div className="flex gap-3 pt-1">
          {view.action === "signin" && (
            <Link className="font-medium text-primary underline" href="/login">
              Sign in again
            </Link>
          )}
          {(view.action === "retry" || view.action === "reload") && onRetry && (
            <button type="button" className="font-medium text-primary underline" onClick={onRetry}>
              {view.action === "reload" ? "Reload" : "Try again"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export function Empty({ children }: { children: React.ReactNode }) {
  return <div className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">{children}</div>;
}

export function Loading({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="flex items-center gap-2 py-6 text-sm text-muted-foreground">
      <Loader2 className="h-4 w-4 animate-spin" /> {label}
    </div>
  );
}

export function NotPermitted({ what }: { what: string }) {
  return (
    <div className="flex gap-3 rounded-lg border border-border bg-muted/40 px-4 py-3 text-sm text-muted-foreground">
      <ShieldAlert className="h-4 w-4 shrink-0" />
      <span>
        Your developer workspace role does not include {what}. Developer Platform roles are separate from your CF Meet organization
        roles; ask a workspace owner if you need access.
      </span>
    </div>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <label className="block space-y-1 text-sm">
      <span className="font-medium text-foreground">{label}</span>
      {children}
      {hint && <span className="block text-xs text-muted-foreground">{hint}</span>}
    </label>
  );
}

export const inputClass =
  "w-full rounded-lg border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary disabled:opacity-50";

export function Table({ head, children }: { head: string[]; children: React.ReactNode }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[640px] text-left text-sm">
        <thead>
          <tr className="border-b border-border text-xs uppercase tracking-wide text-muted-foreground">
            {head.map((h) => (
              <th key={h} className="px-3 py-2 font-medium">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="divide-y divide-border">{children}</tbody>
      </table>
    </div>
  );
}

export function Td({ children, className }: { children?: React.ReactNode; className?: string }) {
  return <td className={cn("px-3 py-2 align-top", className)}>{children}</td>;
}

export function Mono({ children }: { children: React.ReactNode }) {
  return <span className="break-all font-mono text-xs">{children}</span>;
}

/** A button that asks for confirmation inline before running a mutation. */
export function ConfirmButton({
  label,
  confirmLabel,
  message,
  onConfirm,
  pending,
  destructive,
  disabled,
}: {
  label: string;
  confirmLabel?: string;
  message: string;
  onConfirm: () => void;
  pending?: boolean;
  destructive?: boolean;
  disabled?: boolean;
}) {
  const [asking, setAsking] = useState(false);
  if (!asking) {
    return (
      <Button size="sm" variant={destructive ? "ghost" : "outline"} className={cn(destructive && "text-red-600")} disabled={disabled} loading={pending} onClick={() => setAsking(true)}>
        {label}
      </Button>
    );
  }
  return (
    <span className="inline-flex flex-wrap items-center gap-2 rounded-lg border border-border bg-muted/50 px-2 py-1 text-xs">
      <span className="max-w-xs text-muted-foreground">{message}</span>
      <Button
        size="sm"
        variant={destructive ? "destructive" : "default"}
        loading={pending}
        onClick={() => {
          setAsking(false);
          onConfirm();
        }}
      >
        {confirmLabel ?? label}
      </Button>
      <Button size="sm" variant="ghost" onClick={() => setAsking(false)}>
        Cancel
      </Button>
    </span>
  );
}

/**
 * Shows a one-time secret (credential secret, webhook signing secret, invitation token)
 * exactly as the backend returned it. It lives only in the caller's component state: never
 * stored, logged or put in a URL, and dropped when the user confirms they saved it.
 */
export function SecretOnce({
  title,
  secret,
  onDone,
  redactedHelp,
}: {
  title: string;
  secret: OneTimeSecret;
  onDone: () => void;
  redactedHelp: string;
}) {
  const [copied, setCopied] = useState(false);
  const [stored, setStored] = useState(false);
  return (
    <div role="dialog" aria-label={title} className="space-y-3 rounded-xl border border-amber-500/40 bg-amber-500/5 p-4">
      <div className="flex items-center gap-2 font-semibold text-foreground">
        <KeyRound className="h-4 w-4 text-amber-600" /> {title}
      </div>
      {secret.kind === "secret" ? (
        <>
          <p className="text-sm text-muted-foreground">
            Copy it now and store it in your secret manager. It is shown only this once: CF Meet cannot show it again. Never put it in
            browser code, a mobile app or a repository.
          </p>
          <div className="flex items-start gap-2">
            <code className="flex-1 select-all break-all rounded-lg border border-border bg-background px-3 py-2 font-mono text-xs" data-secret>
              {secret.value}
            </code>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                void navigator.clipboard?.writeText(secret.value).then(
                  () => setCopied(true),
                  () => setCopied(false),
                );
              }}
            >
              {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
              {copied ? "Copied" : "Copy"}
            </Button>
          </div>
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={stored} onChange={(e) => setStored(e.target.checked)} />I have stored it securely
          </label>
          <Button size="sm" disabled={!stored} onClick={onDone}>
            Done
          </Button>
        </>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">{redactedHelp}</p>
          <Button size="sm" variant="outline" onClick={onDone}>
            Close
          </Button>
        </>
      )}
    </div>
  );
}

/** A paged list operation (`{items, nextCursor}`) with "Load more". */
export function usePagedList<T>(key: QueryKey, spec: Omit<RequestSpec, "query"> & { query?: RequestSpec["query"] }, enabled = true) {
  const { client } = useDeveloperPortal();
  const query = useInfiniteQuery({
    queryKey: key,
    queryFn: ({ pageParam }) =>
      client.call<Page<T>>({ ...spec, query: { ...(spec.query ?? {}), limit: 50, cursor: pageParam ?? undefined } }),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    enabled,
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const items = query.data?.pages.flatMap((p) => p.items) ?? [];
  return { ...query, items };
}

export function LoadMore({ hasNextPage, isFetchingNextPage, fetchNextPage }: { hasNextPage: boolean; isFetchingNextPage: boolean; fetchNextPage: () => void }) {
  if (!hasNextPage) return null;
  return (
    <div className="pt-3">
      <Button size="sm" variant="outline" loading={isFetchingNextPage} onClick={() => fetchNextPage()}>
        Load more
      </Button>
    </div>
  );
}

/**
 * A console mutation: errors are kept for the ErrorNotice (never retried automatically:
 * creates carry an Idempotency-Key the caller reuses on a manual retry), and the listed
 * query keys are invalidated on success.
 */
export function useConsoleMutation<V, R>(fn: (variables: V) => Promise<R>, invalidate: QueryKey[] = [], onSuccess?: (result: R, variables: V) => void) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: fn,
    retry: false,
    onSuccess: (result, variables) => {
      for (const key of invalidate) void queryClient.invalidateQueries({ queryKey: key });
      onSuccess?.(result, variables);
    },
  });
}

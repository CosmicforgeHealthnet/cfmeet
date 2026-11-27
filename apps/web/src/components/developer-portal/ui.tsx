"use client";

/**
 * Small, console-style building blocks of the Developer Portal: sections, badges, error
 * notices with the gateway request id, paged lists, confirmations and the one-time
 * secret panel.
 */
import { useState } from "react";
import Link from "next/link";
import { AlertTriangle, Check, CheckCircle2, Copy, Inbox, KeyRound, RotateCw, ShieldAlert, WifiOff, X } from "lucide-react";
import { useEffect } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
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
    <div className="flex flex-col gap-3 pb-2 sm:flex-row sm:items-end sm:justify-between">
      <div>
        <h1 className="text-2xl font-bold tracking-tight text-foreground sm:text-[28px]">{title}</h1>
        {description && <p className="mt-1 max-w-2xl text-sm text-muted-foreground">{description}</p>}
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
    <section className={cn("overflow-hidden rounded-2xl border border-border bg-card", className)}>
      <div className="flex flex-col gap-2 border-b border-border px-5 py-4 sm:flex-row sm:items-center sm:justify-between sm:px-6">
        <div>
          <h2 className="text-base font-semibold text-foreground">{title}</h2>
          {description && <div className="mt-0.5 text-sm text-muted-foreground">{description}</div>}
        </div>
        {actions && <div className="flex flex-wrap gap-2">{actions}</div>}
      </div>
      <div className="p-5 sm:p-6">{children}</div>
    </section>
  );
}

const TONES: Record<BadgeModel["tone"], string> = {
  ok: "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  warn: "bg-amber-500/15 text-amber-700 dark:text-amber-300",
  bad: "bg-red-500/10 text-red-700 dark:text-red-300",
  muted: "bg-muted text-muted-foreground",
};

function friendlyLabel(label: string) {
  const stamp = label.match(/^(.*?)(\d{4}-\d{2}-\d{2} \d{2}:\d{2}) UTC$/);
  if (stamp) return `${stamp[1]}${friendlyTime(`${stamp[2].replace(" ", "T")}:00Z`)}`;
  return label === label.toUpperCase() ? label.charAt(0) + label.slice(1).toLowerCase() : label;
}

/** "14 Nov 2026, 07:24" in the viewer's time zone (or "Today, 07:24"). */
export function friendlyTime(value: string | null | undefined) {
  if (!value) return "—";
  const d = new Date(value);
  if (Number.isNaN(+d)) return "—";
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (d.toDateString() === new Date().toDateString()) return `today, ${time}`;
  return `${d.toLocaleDateString([], { day: "numeric", month: "short", year: d.getFullYear() === new Date().getFullYear() ? undefined : "numeric" })}, ${time}`;
}

/** cfmk_sbx_bdf650c337038eeb0444a642_*****…b87e → cfmk_sbx_bdf6…b87e */
export function shortKey(masked: string) {
  const m = masked.match(/^(.*?_[a-z]+_[0-9a-f]{4})[0-9a-f]*_?\**([0-9a-z]{2,6})$/i);
  if (m) return `${m[1]}…${m[2]}`;
  return masked.length > 24 ? `${masked.slice(0, 14)}…${masked.slice(-4)}` : masked;
}

export function Badge({ label: raw, tone }: BadgeModel) {
  const label = friendlyLabel(raw);
  return (
    <span className={cn("inline-flex items-center gap-1.5 whitespace-nowrap rounded-full px-2 py-0.5 text-xs font-semibold", TONES[tone])}>
      <span className={cn("size-1.5 rounded-full", tone === "ok" ? "bg-emerald-500" : tone === "warn" ? "bg-amber-500" : tone === "bad" ? "bg-red-500" : "bg-muted-foreground/50")} />
      {label}
    </span>
  );
}

export function StatusBadge({ status }: { status: string | null | undefined }) {
  const text = (status ?? "UNKNOWN").replace(/_/g, " ").toLowerCase();
  return <Badge label={text.charAt(0).toUpperCase() + text.slice(1)} tone={statusTone(status)} />;
}

function RequestId({ id }: { id: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button type="button" title="Copy reference for support" onClick={() => void navigator.clipboard?.writeText(id).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); })}
      className="inline-flex items-center gap-1.5 rounded-full bg-background/70 px-2.5 py-1 font-mono text-[11px] text-muted-foreground ring-1 ring-border hover:text-foreground">
      {copied ? <Check className="size-3" /> : <Copy className="size-3" />}Ref {id.length > 14 ? `${id.slice(0, 12)}…` : id}
    </button>
  );
}

/**
 * Errors, in plain words, with the right next step. `page` fills the content area (nothing
 * else can show); the default sits inline above or inside a section.
 */
export function ErrorNotice({ error, onRetry, page }: { error: unknown; onRetry?: () => void; page?: boolean }) {
  if (!error) return null;
  const view = describeError(error);
  const offline = /reach|connect/i.test(view.title);
  const Icon = offline ? WifiOff : view.action === "signin" ? KeyRound : AlertTriangle;
  const actions = (
    <>
      {view.action === "signin" && <Link className="inline-flex h-10 items-center rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90" href="/login">Sign in</Link>}
      {(view.action === "retry" || view.action === "reload") && onRetry && (
        <button type="button" onClick={onRetry} className={cn("inline-flex items-center gap-2 rounded-full text-sm font-semibold", page ? "h-10 bg-primary px-5 text-primary-foreground hover:bg-primary/90" : "h-8 px-3 text-primary hover:bg-primary/10")}>
          <RotateCw className="size-4" />{view.action === "reload" ? "Reload" : "Try again"}
        </button>
      )}
    </>
  );
  if (page) {
    return (
      <div role="alert" className="flex flex-col items-center rounded-3xl border border-border bg-card px-6 py-16 text-center">
        <span className={cn("flex size-16 items-center justify-center rounded-2xl", offline ? "bg-muted text-muted-foreground" : "bg-red-500/10 text-red-600")}><Icon className="size-7" /></span>
        <h2 className="mt-5 text-xl font-semibold text-foreground">{view.title}</h2>
        {view.detail && <p className="mt-2 max-w-md text-sm leading-relaxed text-muted-foreground">{view.detail}</p>}
        <div className="mt-6 flex flex-wrap items-center justify-center gap-2">
          {actions}
          <Link href="/dashboard/developers" className="inline-flex h-10 items-center rounded-full px-5 text-sm font-semibold ring-1 ring-border hover:bg-muted">Back to CF Meet</Link>
        </div>
        {view.requestId && <div className="mt-5"><RequestId id={view.requestId} /></div>}
      </div>
    );
  }
  return (
    <div role="alert" className="flex gap-3 rounded-2xl bg-red-500/[0.07] p-4 text-sm ring-1 ring-red-500/20">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-red-500/10 text-red-600"><Icon className="size-4" /></span>
      <div className="min-w-0 flex-1">
        <p className="font-semibold text-foreground">{view.title}</p>
        {view.detail && <p className="mt-0.5 text-muted-foreground">{view.detail}</p>}
        {(view.requestId || view.action !== "none") && (
          <div className="mt-2 flex flex-wrap items-center gap-2">{actions}{view.requestId && <RequestId id={view.requestId} />}</div>
        )}
      </div>
    </div>
  );
}

/* ─── Toasts: short confirmations after an action (bottom of the screen) ─── */

type ToastItem = { id: number; text: string; tone: "ok" | "info" };
const TOAST_EVENT = "cfmeet:dev-toast";
let toastSeq = 0;
export function notify(text: string, tone: ToastItem["tone"] = "ok") {
  if (typeof window !== "undefined") window.dispatchEvent(new CustomEvent(TOAST_EVENT, { detail: { id: ++toastSeq, text, tone } }));
}

export function Toaster() {
  const [items, setItems] = useState<ToastItem[]>([]);
  useEffect(() => {
    const add = (e: Event) => {
      const t = (e as CustomEvent<ToastItem>).detail;
      setItems((cur) => [...cur.slice(-2), t]);
      setTimeout(() => setItems((cur) => cur.filter((x) => x.id !== t.id)), 3500);
    };
    window.addEventListener(TOAST_EVENT, add);
    return () => window.removeEventListener(TOAST_EVENT, add);
  }, []);
  return (
    <div aria-live="polite" className="pointer-events-none fixed inset-x-0 bottom-4 z-[70] flex flex-col items-center gap-2 px-4">
      {items.map((t) => (
        <div key={t.id} role="status" className="pointer-events-auto flex items-center gap-3 rounded-full bg-foreground py-2.5 pl-3 pr-2 text-sm font-medium text-background shadow-2xl animate-fade-in">
          <CheckCircle2 className={cn("size-5 shrink-0", t.tone === "ok" ? "text-emerald-400" : "text-sky-400")} />
          {t.text}
          <button type="button" aria-label="Dismiss" onClick={() => setItems((cur) => cur.filter((x) => x.id !== t.id))} className="flex size-7 items-center justify-center rounded-full opacity-60 hover:bg-background/15 hover:opacity-100"><X className="size-4" /></button>
        </div>
      ))}
    </div>
  );
}

export function Empty({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-10 text-center text-sm text-muted-foreground">
      <span className="mb-3 flex size-11 items-center justify-center rounded-xl bg-muted text-muted-foreground"><Inbox className="size-5" /></span>
      {children}
    </div>
  );
}

export function Loading({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="space-y-3 py-2" role="status" aria-label={label}>
      <div className="h-5 w-48 animate-pulse rounded-md bg-muted" />
      <div className="h-24 animate-pulse rounded-2xl bg-muted" />
      <div className="h-24 animate-pulse rounded-2xl bg-muted" />
    </div>
  );
}

export function NotPermitted({ what }: { what: string }) {
  return (
    <div className="flex gap-3 rounded-xl bg-muted/60 px-4 py-3 text-sm text-muted-foreground">
      <ShieldAlert className="mt-0.5 size-4 shrink-0" />
      <span>
        Your role in this developer workspace doesn&apos;t include {what}. Ask a workspace owner if you need it. (Developer roles are separate from your organization roles.)
      </span>
    </div>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <label className="block space-y-1.5 text-sm">
      <span className="font-medium text-foreground">{label}</span>
      {children}
      {hint && <span className="block text-xs text-muted-foreground">{hint}</span>}
    </label>
  );
}

export const inputClass =
  "h-11 w-full rounded-xl border border-border bg-background px-3.5 text-sm text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary disabled:opacity-50 [textarea&]:h-auto [textarea&]:py-2.5";

export function Table({ head, children }: { head: string[]; children: React.ReactNode }) {
  return (
    <div className="no-scrollbar -mx-5 overflow-x-auto sm:-mx-6">
      <table className="w-full min-w-[640px] text-left text-sm">
        <thead>
          <tr className="border-b border-border bg-muted/40 text-xs text-muted-foreground">
            {head.map((h) => (
              <th key={h} className="px-5 py-2.5 font-semibold first:pl-5 sm:first:pl-6">
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
  return <td className={cn("px-5 py-3 align-middle first:pl-5 sm:first:pl-6", className)}>{children}</td>;
}

export function Mono({ children }: { children: React.ReactNode }) {
  return <span className="break-all rounded-md bg-muted px-1.5 py-0.5 font-mono text-xs">{children}</span>;
}

/** A button that asks for confirmation in a small dialog before running a mutation. */
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
  // The question is the first sentence; anything after it explains the consequence.
  const [question, ...rest] = message.split(/(?<=\?)\s+/);
  return (
    <>
      <Button size="sm" variant={destructive ? "ghost" : "outline"} className={cn("rounded-full", destructive && "text-red-600 hover:bg-red-500/10 hover:text-red-600")} disabled={disabled} loading={pending} onClick={() => setAsking(true)}>
        {label}
      </Button>
      <Dialog open={asking} onOpenChange={setAsking}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader className="text-left">
            {destructive && <span className="mb-2 flex size-11 items-center justify-center rounded-xl bg-red-500/10 text-red-600"><AlertTriangle className="size-5" /></span>}
            <DialogTitle className="text-lg">{question}</DialogTitle>
            <DialogDescription>{rest.join(" ") || (destructive ? "This can't be undone." : "You can change this later.")}</DialogDescription>
          </DialogHeader>
          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={() => setAsking(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" onClick={() => { setAsking(false); onConfirm(); }} className={cn("h-10 rounded-full px-5 text-sm font-semibold text-white", destructive ? "bg-red-600 hover:bg-red-700" : "bg-primary hover:bg-primary/90")}>
              {confirmLabel ?? label}
            </button>
          </div>
        </DialogContent>
      </Dialog>
    </>
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
    <div aria-label={title} className="space-y-5">
      <div className="flex items-start gap-4">
        <span className="flex size-12 shrink-0 items-center justify-center rounded-2xl bg-emerald-500/10 text-emerald-600"><KeyRound className="size-6" /></span>
        <div>
          <p className="text-lg font-semibold text-foreground">{secret.kind === "secret" ? `${title} is ready` : title}</p>
          <p className="mt-0.5 text-sm text-muted-foreground">{secret.kind === "secret" ? "Copy it now. You won't be able to see it again." : redactedHelp}</p>
        </div>
      </div>
      {secret.kind === "secret" ? (
        <>
          <div className="rounded-2xl bg-[#0B1220] p-1.5">
            <code className="block max-h-28 select-all overflow-y-auto break-all px-3 py-2.5 font-mono text-[13px] leading-relaxed text-emerald-300" data-secret>
              {secret.value}
            </code>
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard?.writeText(secret.value).then(
                  () => setCopied(true),
                  () => setCopied(false),
                );
              }}
              className={cn("flex h-11 w-full items-center justify-center gap-2 rounded-xl text-sm font-semibold transition-colors", copied ? "bg-emerald-500 text-white" : "bg-white/10 text-white hover:bg-white/15")}
            >
              {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
              {copied ? "Copied to clipboard" : "Copy key"}
            </button>
          </div>
          <ul className="space-y-1.5 text-sm text-muted-foreground">
            <li className="flex gap-2"><ShieldAlert className="mt-0.5 size-4 shrink-0 text-amber-600" />Store it in your server&apos;s secret settings or a password manager.</li>
            <li className="flex gap-2"><ShieldAlert className="mt-0.5 size-4 shrink-0 text-amber-600" />Never put it in website code, a mobile app or a code repository.</li>
          </ul>
          <div className="flex flex-col-reverse gap-3 border-t border-border pt-4 sm:flex-row sm:items-center sm:justify-between">
            <label className="flex cursor-pointer items-center gap-2.5 text-sm font-medium">
              <input type="checkbox" className="sr-only" checked={stored} onChange={(e) => setStored(e.target.checked)} />
              <span className={cn("flex size-5 items-center justify-center rounded-md border", stored ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40 bg-background")}>{stored && <Check className="size-3.5" strokeWidth={3} />}</span>
              I&apos;ve saved it somewhere safe
            </label>
            <Button className="rounded-full px-6" disabled={!stored} onClick={onDone}>Done</Button>
          </div>
        </>
      ) : (
        <div className="flex justify-end"><Button variant="outline" className="rounded-full" onClick={onDone}>Close</Button></div>
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
    <div className="flex justify-center pt-4">
      <Button size="sm" variant="ghost" className="rounded-full text-primary hover:bg-primary/10 hover:text-primary" loading={isFetchingNextPage} onClick={() => fetchNextPage()}>
        Show more
      </Button>
    </div>
  );
}

/**
 * A console mutation: errors are kept for the ErrorNotice (never retried automatically:
 * creates carry an Idempotency-Key the caller reuses on a manual retry), and the listed
 * query keys are invalidated on success.
 */
export function useConsoleMutation<V, R>(fn: (variables: V) => Promise<R>, invalidate: QueryKey[] = [], onSuccess?: (result: R, variables: V) => void, successMessage = "Done. Your changes are saved.") {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: fn,
    retry: false,
    onSuccess: (result, variables) => {
      for (const key of invalidate) void queryClient.invalidateQueries({ queryKey: key });
      onSuccess?.(result, variables);
      if (successMessage) notify(successMessage);
    },
  });
}

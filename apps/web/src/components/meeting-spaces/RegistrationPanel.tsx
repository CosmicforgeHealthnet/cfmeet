"use client";

import { useMemo, useState } from "react";
import { Check, ChevronDown, ClipboardList, Copy, Download, ExternalLink, Search } from "lucide-react";
import type { MeetingSpaceResponse, RegistrationView } from "@/types/v2";
import { useCancelRegistration, useRegistrations } from "@/hooks/v2/useRegistrations";
import { useProfilePolicy } from "@/hooks/v2/useProfilePolicy";
import { cn } from "@/lib/utils";
import { ActionDialog, RequestError } from "./ManagementUI";

type Filter = "active" | "cancelled" | "all";

const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
const day = (iso: string) => new Date(iso).toLocaleDateString(undefined, { day: "numeric", month: "short" });

function toCsv(rows: RegistrationView[]) {
  const extra = [...new Set(rows.flatMap((r) => Object.keys(r.customFields)))];
  const cell = (v: unknown) => `"${String(v ?? "").replaceAll('"', '""')}"`;
  const head = ["Name", "Email", "Organization", "Status", "Registered", ...extra];
  const body = rows.map((r) => [r.displayName, r.email, r.organization, r.status === "CANCELLED" ? "Cancelled" : "Registered", new Date(r.createdAt).toISOString(), ...extra.map((k) => r.customFields[k])].map(cell).join(","));
  return [head.map(cell).join(","), ...body].join("\n");
}

function Row({ item, cancel }: { item: RegistrationView; cancel: (id: string) => Promise<unknown> }) {
  const [open, setOpen] = useState(false);
  const cancelled = item.status === "CANCELLED";
  const fields = Object.entries(item.customFields);
  return (
    <li className={cn("px-4 py-3", cancelled && "opacity-60")}>
      <div className="flex items-center gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary">{initials(item.displayName)}</span>
        <div className="min-w-0 flex-1">
          <p className="flex items-center gap-2 text-sm font-semibold text-foreground"><span className="truncate">{item.displayName}</span>{cancelled && <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">Cancelled</span>}</p>
          <p className="truncate text-xs text-muted-foreground">{[item.email, item.organization].filter(Boolean).join(" · ") || "No contact details"}</p>
        </div>
        <span className="hidden shrink-0 text-xs text-muted-foreground sm:block">{day(item.createdAt)}</span>
        {fields.length > 0 && <button type="button" aria-expanded={open} aria-label="Show answers" onClick={() => setOpen(!open)} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ChevronDown className={cn("size-4 transition-transform", open && "rotate-180")} /></button>}
        {!cancelled && <ActionDialog label="Cancel" destructive title={`Cancel ${item.displayName}'s registration?`} description="Their join link stops working. They can register again if registration is still open." submit={() => cancel(item.id)} />}
      </div>
      {open && (
        <dl className="ml-[3.25rem] mt-3 grid gap-x-6 gap-y-2 rounded-xl bg-muted/50 p-3 text-sm sm:grid-cols-2">
          {fields.map(([key, value]) => <div key={key}><dt className="text-xs text-muted-foreground">{key}</dt><dd className="break-words text-foreground">{String(value) || "—"}</dd></div>)}
        </dl>
      )}
    </li>
  );
}

export default function RegistrationPanel({ space }: { space: MeetingSpaceResponse }) {
  const allowed = ["OWNER", "HOST", "CO_HOST"].includes(space.viewerRole || "");
  const meetingTypeEnabled = space.meetingType === "WEBINAR" || space.meetingType === "TOWN_HALL";
  const policy = useProfilePolicy(space.id, allowed && meetingTypeEnabled);
  const registrationEnabled = policy.data?.effective.policy.registration.enabled === true;
  const list = useRegistrations(space.id, allowed && meetingTypeEnabled && registrationEnabled);
  const cancel = useCancelRegistration(space.id);
  const [filter, setFilter] = useState<Filter>("active");
  const [query, setQuery] = useState("");
  const [copied, setCopied] = useState(false);

  const all = list.data?.registrations ?? [];
  const counts = { active: all.filter((r) => r.status !== "CANCELLED").length, cancelled: all.filter((r) => r.status === "CANCELLED").length, all: all.length };
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all
      .filter((r) => filter === "all" || (filter === "cancelled" ? r.status === "CANCELLED" : r.status !== "CANCELLED"))
      .filter((r) => !q || [r.displayName, r.email, r.organization].some((v) => v?.toLowerCase().includes(q)))
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt));
  }, [all, filter, query]);

  if (!allowed || !meetingTypeEnabled) return null;
  const formUrl = typeof window === "undefined" ? "" : `${window.location.origin}/register/${encodeURIComponent(space.meetingCode)}`;

  const copy = () => { void navigator.clipboard.writeText(formUrl); setCopied(true); setTimeout(() => setCopied(false), 2000); };
  const exportCsv = () => {
    const blob = new Blob([toCsv(shown)], { type: "text/csv;charset=utf-8" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `${space.title || "registrations"} - registrations.csv`;
    a.click();
    URL.revokeObjectURL(a.href);
  };

  return (
    <section className="space-y-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold text-foreground">Registrations{list.data && <span className="ml-1.5 text-muted-foreground">{counts.active}</span>}</h2>
          <p className="text-sm text-muted-foreground">People who signed up for this {space.meetingType === "TOWN_HALL" ? "town hall" : "webinar"}.</p>
        </div>
        {registrationEnabled && (
          <div className="flex gap-2">
            <button type="button" onClick={copy} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90">{copied ? <Check className="size-4" /> : <Copy className="size-4" />}{copied ? "Copied" : "Copy sign-up link"}</button>
            <a href={formUrl} target="_blank" rel="noopener noreferrer" aria-label="Open sign-up page" title="Open sign-up page" className="flex size-9 items-center justify-center rounded-full border border-border text-foreground hover:bg-muted"><ExternalLink className="size-4" /></a>
          </div>
        )}
      </div>

      {policy.isLoading && <div className="h-24 animate-pulse rounded-2xl bg-muted" />}
      {policy.isError && <RequestError error={policy.error} retry={() => void policy.refetch()} />}
      {policy.data && !registrationEnabled && (
        <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-10 text-center">
          <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><ClipboardList className="size-5" /></span>
          <p className="mt-3 font-semibold text-foreground">Sign-ups are off</p>
          <p className="mt-1 max-w-sm text-sm text-muted-foreground">This meeting&apos;s settings don&apos;t allow public registration. An organization admin can turn it on in the meeting profile.</p>
        </div>
      )}

      {registrationEnabled && (
        <>
          {list.isError && <RequestError error={list.error} retry={() => void list.refetch()} />}
          {cancel.isError && <RequestError error={cancel.error} />}
          {all.length > 0 && (
            <div className="flex flex-wrap items-center gap-2">
              <div role="tablist" className="flex rounded-full bg-muted p-1">
                {([["active", "Registered"], ["cancelled", "Cancelled"], ["all", "All"]] as const).map(([id, label]) => (
                  <button key={id} role="tab" type="button" aria-selected={filter === id} onClick={() => setFilter(id)} className={cn("h-8 rounded-full px-3 text-sm font-medium transition-colors", filter === id ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>
                    {label} <span className="tabular-nums text-muted-foreground">{counts[id]}</span>
                  </button>
                ))}
              </div>
              <label className="relative min-w-0 flex-1 sm:max-w-xs">
                <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search name, email, company" className="h-10 w-full rounded-full border border-border bg-background pl-9 pr-3 text-sm outline-none focus:border-primary" />
              </label>
              <button type="button" onClick={exportCsv} disabled={!shown.length} className="inline-flex h-10 items-center gap-1.5 rounded-full border border-border px-4 text-sm font-semibold text-foreground hover:bg-muted disabled:opacity-50 sm:ml-auto"><Download className="size-4" />Export CSV</button>
            </div>
          )}
          {list.isLoading && <div className="space-y-2">{[0, 1, 2].map((i) => <div key={i} className="h-14 animate-pulse rounded-xl bg-muted" />)}</div>}
          {list.data && all.length === 0 && (
            <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-10 text-center">
              <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><ClipboardList className="size-5" /></span>
              <p className="mt-3 font-semibold text-foreground">No sign-ups yet</p>
              <p className="mt-1 max-w-sm text-sm text-muted-foreground">Share the sign-up link. Everyone who registers shows up here with their details.</p>
            </div>
          )}
          {all.length > 0 && (shown.length ? (
            <ul className="divide-y divide-border rounded-2xl border border-border">{shown.map((item) => <Row key={item.id} item={item} cancel={(id) => cancel.mutateAsync(id)} />)}</ul>
          ) : <p className="py-6 text-center text-sm text-muted-foreground">No one matches.</p>)}
        </>
      )}
    </section>
  );
}

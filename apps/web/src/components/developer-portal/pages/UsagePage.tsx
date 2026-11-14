"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, BarChart3, ChevronDown, Download, Gauge, Package } from "lucide-react";
import { Button } from "@/components/ui/button";
import { NiceSelect } from "@/components/ui/nice-select";
import { cn } from "@/lib/utils";
import { BILLING_NOT_AVAILABLE, formatQuantity, isoDay, limitRatio, retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import {
  USAGE_TYPES,
  type CommercialPlan,
  type EnvironmentLimits,
  type Entitlements,
  type OrganizationCommercial,
  type Page,
  type UsageSummary,
  type UsageType,
  type UsageWarnings,
  type Workspace,
} from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { EnvironmentPicker, useEnvironmentSelection } from "../EnvironmentPicker";
import { RequireWorkspace } from "../PortalShell";
import { Empty, ErrorNotice, Field, friendlyTime, inputClass, Loading, NotPermitted, PageHeader } from "../ui";

const nice = (v: string) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : v);
const retry = {
  retry: (count: number, error: unknown) => shouldRetryQuery(count, error),
  retryDelay: (attempt: number, error: unknown) => retryDelayMs(attempt, error),
};
const isForbidden = (error: unknown) => (error as { status?: number } | null)?.status === 403;

function Block({ icon: Icon, title, description, actions, children }: { icon: typeof Gauge; title: string; description?: string; actions?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div className="flex items-center gap-3">
          <span className="flex size-9 items-center justify-center rounded-xl bg-muted text-foreground"><Icon className="size-4" /></span>
          <div><h2 className="text-lg font-semibold">{title}</h2>{description && <p className="text-sm text-muted-foreground">{description}</p>}</div>
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

export default function UsagePage() {
  return (
    <RequireWorkspace>
      <Usage />
    </RequireWorkspace>
  );
}

function Usage() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  const { environmentId } = useEnvironmentSelection();
  return (
    <div className="space-y-8">
      <PageHeader title="Usage & limits" description="How much each environment uses, the limits that apply, and your plan." />
      <p className="rounded-2xl bg-muted/60 px-4 py-3 text-sm text-muted-foreground">{BILLING_NOT_AVAILABLE}</p>
      <EnvironmentPicker workspace={workspace} />
      {environmentId ? (
        <>
          {can("UsageRead") ? <UsageSection key={`u-${environmentId}`} workspace={workspace} environmentId={environmentId} /> : <NotPermitted what="seeing usage" />}
          {can("LimitsRead") && <LimitsSection key={`l-${environmentId}`} workspace={workspace} environmentId={environmentId} />}
        </>
      ) : (
        <Empty>Choose a project and environment above to see its usage and limits.</Empty>
      )}
      {can("CommercialRead") ? <PlansSection workspace={workspace} /> : <PublicPlans />}
    </div>
  );
}

const RANGES = [
  { value: "7", label: "Last 7 days" },
  { value: "30", label: "Last 30 days" },
  { value: "90", label: "Last 90 days" },
  { value: "custom", label: "Custom range" },
];

function UsageSection({ workspace, environmentId }: { workspace: Workspace; environmentId: string }) {
  const { client, can } = useDeveloperPortal();
  const [range, setRange] = useState("30");
  const [from, setFrom] = useState(() => isoDay(Date.now(), 30));
  const [to, setTo] = useState(() => isoDay(Date.now()));
  const [usageType, setUsageType] = useState<UsageType | "">("");
  const [exportError, setExportError] = useState<unknown>(null);
  const [exporting, setExporting] = useState(false);
  const [showDays, setShowDays] = useState(false);
  const query = { from, to, usageType: usageType || undefined };
  const pickRange = (v: string) => {
    setRange(v);
    if (v !== "custom") { setFrom(isoDay(Date.now(), Number(v))); setTo(isoDay(Date.now())); }
  };
  const summary = useQuery({
    queryKey: ["developer-portal", workspace.id, "usage", environmentId, from, to, usageType],
    queryFn: () => client.call<UsageSummary>({ operation: "getEnvironmentUsage", path: { environmentId }, query }),
    ...retry,
  });
  const warnings = useQuery({
    queryKey: ["developer-portal", workspace.id, "usage-warnings", environmentId],
    queryFn: () => client.call<UsageWarnings>({ operation: "listEnvironmentUsageWarnings", path: { environmentId } }),
    ...retry,
  });

  const download = async () => {
    setExporting(true);
    setExportError(null);
    try {
      const { blob } = await client.download({ operation: "exportEnvironmentUsage", path: { environmentId }, query: { ...query, format: "csv", granularity: "daily" }, accept: "text/csv" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `cfmeet-usage-${from}-to-${to}.csv`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (error) {
      setExportError(error);
    } finally {
      setExporting(false);
    }
  };

  const s = summary.data;
  return (
    <Block icon={BarChart3} title="Usage"
      actions={can("UsageExport") ? <Button size="sm" variant="outline" className="rounded-full" loading={exporting} onClick={() => void download()}><Download className="size-4" />Download CSV</Button> : null}>
      <div className="grid gap-3 sm:grid-cols-[repeat(2,minmax(0,13rem))]">
        <NiceSelect value={range} onChange={pickRange} options={RANGES} />
        <NiceSelect value={usageType} onChange={(v) => setUsageType(v as UsageType | "")} options={[{ value: "", label: "Everything" }, ...USAGE_TYPES.map((t) => ({ value: t, label: nice(t) }))]} />
      </div>
      {range === "custom" && (
        <div className="grid max-w-md grid-cols-2 gap-3">
          <Field label="From"><input className={inputClass} type="date" value={from} max={to} onChange={(e) => setFrom(e.target.value)} /></Field>
          <Field label="To"><input className={inputClass} type="date" value={to} min={from} onChange={(e) => setTo(e.target.value)} /></Field>
        </div>
      )}
      <ErrorNotice error={exportError} />
      {(warnings.data?.items ?? []).map((w) => (
        <div key={w.limit} className={cn("flex items-start gap-3 rounded-2xl px-4 py-3 text-sm", w.level === "EXHAUSTED" ? "bg-red-500/10 text-red-700 dark:text-red-300" : "bg-amber-500/10 text-amber-800 dark:text-amber-200")}>
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span><span className="font-semibold">{w.level === "EXHAUSTED" ? "Limit reached" : "Getting close to a limit"}:</span> {nice(w.limit)} is at {w.current} of {w.ceiling} ({Math.round(w.ratio * 100)}%).</span>
        </div>
      ))}
      <ErrorNotice error={warnings.error} />
      {summary.isLoading ? (
        <Loading />
      ) : summary.error ? (
        <ErrorNotice error={summary.error} onRetry={() => void summary.refetch()} />
      ) : s ? (
        <>
          {s.sandbox && <p className="text-sm text-muted-foreground">This is a Sandbox: usage is counted for testing and limits only.</p>}
          {s.totals.length === 0 ? (
            <Empty>Nothing used in this period yet.</Empty>
          ) : (
            <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {s.totals.map((t) => (
                <li key={t.usageType} className="rounded-2xl border border-border bg-card p-5">
                  <p className="text-sm text-muted-foreground">{nice(t.usageType)}</p>
                  <p className="mt-1 text-2xl font-bold tabular-nums">{formatQuantity(t.quantity, t.unit)}</p>
                  {t.factCount != null && <p className="mt-1 text-xs text-muted-foreground">from {t.factCount} records</p>}
                </li>
              ))}
            </ul>
          )}
          {s.days.length > 0 && (
            <div>
              <button type="button" onClick={() => setShowDays(!showDays)} className="inline-flex items-center gap-1 text-sm font-semibold text-primary hover:underline">{showDays ? "Hide" : "Show"} day by day ({s.days.length})<ChevronDown className={cn("size-4 transition-transform", showDays && "rotate-180")} /></button>
              {showDays && (
                <ul className="mt-2 divide-y divide-border rounded-2xl border border-border bg-card text-sm">
                  {s.days.map((d) => (
                    <li key={`${d.day}-${d.usageType}`} className="flex items-center justify-between gap-3 px-4 py-2.5">
                      <span className="text-muted-foreground">{new Date(`${d.day}T12:00:00Z`).toLocaleDateString([], { day: "numeric", month: "short" })} · {nice(d.usageType)}</span>
                      <span className="font-semibold tabular-nums">{formatQuantity(d.quantity, d.unit)}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
        </>
      ) : null}
    </Block>
  );
}

function LimitsSection({ workspace, environmentId }: { workspace: Workspace; environmentId: string }) {
  const { client } = useDeveloperPortal();
  const limits = useQuery({ queryKey: ["developer-portal", workspace.id, "limits", environmentId], queryFn: () => client.call<EnvironmentLimits>({ operation: "getEnvironmentLimits", path: { environmentId } }), ...retry });
  const entitlements = useQuery({ queryKey: ["developer-portal", workspace.id, "entitlements", environmentId], queryFn: () => client.call<Entitlements>({ operation: "getEnvironmentEntitlements", path: { environmentId } }), ...retry });
  return (
    <Block icon={Gauge} title="Limits" description="What this environment may use, and how close it is.">
      {limits.isLoading ? (
        <Loading />
      ) : limits.error ? (
        <ErrorNotice error={limits.error} onRetry={() => void limits.refetch()} />
      ) : limits.data && limits.data.limits.length > 0 ? (
        <ul className="grid gap-3 md:grid-cols-2">
          {limits.data.limits.map((l) => {
            const ratio = limitRatio(l.current, l.ceiling);
            return (
              <li key={`${l.limit}-${l.window}`} className="rounded-2xl border border-border bg-card p-4">
                <div className="flex items-baseline justify-between gap-2">
                  <p className="font-semibold">{nice(l.limit)}</p>
                  <p className="text-xs text-muted-foreground">per {l.window.toLowerCase()}</p>
                </div>
                <div className="mt-3 h-2 overflow-hidden rounded-full bg-muted">
                  <div className={cn("h-full rounded-full", ratio != null && ratio >= 1 ? "bg-red-500" : ratio != null && ratio >= 0.8 ? "bg-amber-500" : "bg-emerald-500")} style={{ width: `${Math.min(1, ratio ?? 0) * 100}%` }} />
                </div>
                <p className="mt-2 flex justify-between text-xs text-muted-foreground">
                  <span><span className="font-semibold text-foreground">{l.current ?? 0}</span> of {l.ceiling}</span>
                  <span>set by {(l.limitingLevel ?? "platform").replace(/_/g, " ").toLowerCase()}</span>
                </p>
              </li>
            );
          })}
        </ul>
      ) : (
        <Empty>No limits apply here.</Empty>
      )}
      {entitlements.data && Object.keys(entitlements.data.domainEntitlements).length > 0 && (
        <div className="flex flex-wrap gap-2">
          {Object.entries(entitlements.data.domainEntitlements).map(([name, value]) => (
            <span key={name} className="rounded-full bg-muted px-3 py-1 text-xs"><span className="text-muted-foreground">{nice(name)}:</span> <span className="font-semibold">{String(value).toLowerCase()}</span></span>
          ))}
        </div>
      )}
      <ErrorNotice error={entitlements.error} />
    </Block>
  );
}

function PlansSection({ workspace }: { workspace: Workspace }) {
  const { client } = useDeveloperPortal();
  const commercial = useQuery({ queryKey: ["developer-portal", workspace.id, "commercial"], queryFn: () => client.call<OrganizationCommercial>({ operation: "getOrganizationCommercial", path: { organizationId: workspace.id } }), ...retry });
  const c = commercial.data;
  return (
    <Block icon={Package} title="Plan" description="CF Meet sets your plan. You can't change it or pay from here yet.">
      {commercial.isLoading ? (
        <Loading />
      ) : commercial.error ? (
        <ErrorNotice error={commercial.error} onRetry={() => void commercial.refetch()} />
      ) : c ? (
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="rounded-2xl border border-border bg-card p-5">
            <p className="text-sm text-muted-foreground">Your plan</p>
            <p className="mt-1 text-lg font-semibold">{c.planAssignment ? c.planAssignment.plan?.displayName ?? c.planAssignment.planCode : "Standard (platform defaults)"}</p>
            {c.planAssignment && <p className="mt-1 text-xs text-muted-foreground">Since {friendlyTime(c.planAssignment.effectiveFrom)}</p>}
          </div>
          <div className="rounded-2xl border border-border bg-card p-5">
            <p className="text-sm text-muted-foreground">Billing account</p>
            <p className="mt-1 text-lg font-semibold">{c.billingAccount ? `${nice(c.billingAccount.classification)} · ${c.billingAccount.status.toLowerCase()}` : "None"}</p>
            <p className="mt-1 text-xs text-muted-foreground">For records only. Nothing is charged from it.</p>
          </div>
          {c.projectPlanAssignments.length > 0 && (
            <ul className="divide-y divide-border rounded-2xl border border-border bg-card text-sm sm:col-span-2">
              {c.projectPlanAssignments.map((a) => (
                <li key={a.id} className="flex justify-between gap-3 px-4 py-2.5"><span className="truncate font-mono text-xs text-muted-foreground">{(a.projectId ?? "").slice(0, 8)}</span><span className="font-semibold">{a.plan?.displayName ?? a.planCode}</span></li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
      <PublicPlans embedded />
    </Block>
  );
}

function PublicPlans({ embedded }: { embedded?: boolean }) {
  const { client } = useDeveloperPortal();
  const plans = useQuery({ queryKey: ["developer-portal", "commercial-plans"], queryFn: () => client.call<Page<CommercialPlan>>({ operation: "listCommercialPlans" }), staleTime: 5 * 60_000, ...retry });
  if (plans.error && isForbidden(plans.error)) return null;
  const body = plans.isLoading ? (
    <Loading />
  ) : plans.error ? (
    <ErrorNotice error={plans.error} onRetry={() => void plans.refetch()} />
  ) : plans.data && plans.data.items.length > 0 ? (
    <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      {plans.data.items.map((p) => (
        <li key={p.id} className="rounded-2xl border border-border bg-card p-5">
          <p className="font-semibold">{p.displayName}</p>
          <ul className="mt-3 space-y-1 text-sm">
            {Object.entries(p.entitlements).length === 0 ? <li className="text-muted-foreground">Platform defaults</li> : Object.entries(p.entitlements).map(([k, v]) => (
              <li key={k} className="flex justify-between gap-3"><span className="text-muted-foreground">{nice(k)}</span><span className="font-semibold">{String(v)}</span></li>
            ))}
          </ul>
        </li>
      ))}
    </ul>
  ) : (
    <Empty>No plans published yet.</Empty>
  );
  return (
    <div className="space-y-2">
      <p className={cn("font-semibold", !embedded && "text-lg")}>{embedded ? "Available plans" : "Plans"}</p>
      {!embedded && <p className="text-sm text-muted-foreground">What each plan includes. Plans can&apos;t be bought here yet.</p>}
      {body}
    </div>
  );
}

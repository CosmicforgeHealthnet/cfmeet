"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import {
  BILLING_NOT_AVAILABLE,
  describeError,
  formatQuantity,
  formatTimestamp,
  isoDay,
  limitRatio,
  retryDelayMs,
  shouldRetryQuery,
} from "@/lib/developer-platform/presentation";
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
import { Badge, Empty, ErrorNotice, Field, inputClass, Loading, NotPermitted, PageHeader, Section, Table, Td } from "../ui";

const retry = {
  retry: (count: number, error: unknown) => shouldRetryQuery(count, error),
  retryDelay: (attempt: number, error: unknown) => retryDelayMs(attempt, error),
};

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
    <div className="space-y-6">
      <PageHeader title="Usage, limits & plans" description="Metered usage per environment, the limits that apply to it, and the plan records of this workspace." />
      <div className="rounded-xl border border-amber-500/40 bg-amber-500/5 px-4 py-3 text-sm text-muted-foreground">{BILLING_NOT_AVAILABLE}</div>
      <EnvironmentPicker workspace={workspace} />
      {environmentId ? (
        <>
          {can("UsageRead") ? <UsageSection key={`u-${environmentId}`} workspace={workspace} environmentId={environmentId} /> : <NotPermitted what="usage" />}
          {can("LimitsRead") && <LimitsSection key={`l-${environmentId}`} workspace={workspace} environmentId={environmentId} />}
        </>
      ) : (
        <Empty>Select a project and environment to see usage and limits.</Empty>
      )}
      {can("CommercialRead") ? <PlansSection workspace={workspace} /> : <PublicPlans />}
    </div>
  );
}

function UsageSection({ workspace, environmentId }: { workspace: Workspace; environmentId: string }) {
  const { client, can } = useDeveloperPortal();
  const [from, setFrom] = useState(() => isoDay(Date.now(), 30));
  const [to, setTo] = useState(() => isoDay(Date.now()));
  const [usageType, setUsageType] = useState<UsageType | "">("");
  const [exportError, setExportError] = useState<unknown>(null);
  const [exporting, setExporting] = useState(false);
  const query = { from, to, usageType: usageType || undefined };
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
      const { blob } = await client.download({
        operation: "exportEnvironmentUsage",
        path: { environmentId },
        query: { ...query, format: "csv", granularity: "daily" },
        accept: "text/csv",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `cfmeet-usage-${environmentId}-${from}-${to}.csv`;
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
    <Section
      title="Usage"
      description="Daily rollups. A product's usage appears once its usage exporter runs in this deployment."
      actions={
        can("UsageExport") && (
          <Button size="sm" variant="outline" loading={exporting} onClick={() => void download()}>
            Export CSV
          </Button>
        )
      }
    >
      <div className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-3">
          <Field label="From (UTC day)">
            <input className={inputClass} type="date" value={from} max={to} onChange={(e) => setFrom(e.target.value)} />
          </Field>
          <Field label="To (UTC day)">
            <input className={inputClass} type="date" value={to} min={from} onChange={(e) => setTo(e.target.value)} />
          </Field>
          <Field label="Usage type">
            <select className={inputClass} value={usageType} onChange={(e) => setUsageType(e.target.value as UsageType | "")}>
              <option value="">All</option>
              {USAGE_TYPES.map((t) => (
                <option key={t} value={t}>
                  {t.replace(/_/g, " ").toLowerCase()}
                </option>
              ))}
            </select>
          </Field>
        </div>
        <ErrorNotice error={exportError} />
        {(warnings.data?.items ?? []).map((w) => (
          <div key={w.limit} className="flex items-center gap-2 text-sm">
            <Badge label={w.level} tone={w.level === "EXHAUSTED" ? "bad" : "warn"} />
            {w.limit}: {w.current} of {w.ceiling} ({Math.round(w.ratio * 100)}%)
          </div>
        ))}
        <ErrorNotice error={warnings.error} />
        {summary.isLoading ? (
          <Loading />
        ) : summary.error ? (
          <ErrorNotice error={summary.error} onRetry={() => void summary.refetch()} />
        ) : s ? (
          <>
            {s.sandbox && <p className="text-sm text-muted-foreground">Sandbox environment: usage is recorded for testing and limits only.</p>}
            {s.totals.length === 0 ? (
              <Empty>No usage recorded in this range.</Empty>
            ) : (
              <Table head={["Usage type", "Total", "Facts"]}>
                {s.totals.map((t) => (
                  <tr key={t.usageType}>
                    <Td>{t.usageType.replace(/_/g, " ").toLowerCase()}</Td>
                    <Td>{formatQuantity(t.quantity, t.unit)}</Td>
                    <Td>{t.factCount ?? "—"}</Td>
                  </tr>
                ))}
              </Table>
            )}
            {s.days.length > 0 && (
              <details className="text-sm">
                <summary className="cursor-pointer text-muted-foreground">Daily breakdown ({s.days.length} rows)</summary>
                <div className="mt-2">
                  <Table head={["Day", "Usage type", "Quantity"]}>
                    {s.days.map((d) => (
                      <tr key={`${d.day}-${d.usageType}`}>
                        <Td>{d.day}</Td>
                        <Td>{d.usageType.replace(/_/g, " ").toLowerCase()}</Td>
                        <Td>{formatQuantity(d.quantity, d.unit)}</Td>
                      </tr>
                    ))}
                  </Table>
                </div>
              </details>
            )}
          </>
        ) : null}
      </div>
    </Section>
  );
}

function LimitsSection({ workspace, environmentId }: { workspace: Workspace; environmentId: string }) {
  const { client } = useDeveloperPortal();
  const limits = useQuery({
    queryKey: ["developer-portal", workspace.id, "limits", environmentId],
    queryFn: () => client.call<EnvironmentLimits>({ operation: "getEnvironmentLimits", path: { environmentId } }),
    ...retry,
  });
  const entitlements = useQuery({
    queryKey: ["developer-portal", workspace.id, "entitlements", environmentId],
    queryFn: () => client.call<Entitlements>({ operation: "getEnvironmentEntitlements", path: { environmentId } }),
    ...retry,
  });
  return (
    <Section title="Limits & entitlements" description="The effective limits and where each comes from (platform default, plan, operator override, environment, service account or key).">
      <div className="space-y-4">
        {limits.isLoading ? (
          <Loading />
        ) : limits.error ? (
          <ErrorNotice error={limits.error} onRetry={() => void limits.refetch()} />
        ) : limits.data && limits.data.limits.length > 0 ? (
          <Table head={["Limit", "Window", "Used", "Ceiling", "Set by"]}>
            {limits.data.limits.map((l) => {
              const ratio = limitRatio(l.current, l.ceiling);
              return (
                <tr key={`${l.limit}-${l.window}`}>
                  <Td>{l.limit}</Td>
                  <Td>{l.window.toLowerCase()}</Td>
                  <Td>
                    {l.current ?? "—"}
                    {ratio != null && (
                      <div className="mt-1 h-1.5 w-24 rounded bg-muted">
                        <div className={`h-1.5 rounded ${ratio >= 1 ? "bg-red-500" : ratio >= 0.8 ? "bg-amber-500" : "bg-emerald-500"}`} style={{ width: `${ratio * 100}%` }} />
                      </div>
                    )}
                  </Td>
                  <Td>{l.ceiling}</Td>
                  <Td className="text-xs">{(l.limitingLevel ?? "—").replace(/_/g, " ").toLowerCase()}</Td>
                </tr>
              );
            })}
          </Table>
        ) : (
          <Empty>No limits reported.</Empty>
        )}
        {entitlements.data && (
          <dl className="grid gap-3 text-sm sm:grid-cols-3">
            {Object.entries(entitlements.data.domainEntitlements).map(([name, value]) => (
              <div key={name}>
                <dt className="text-muted-foreground">{name}</dt>
                <dd className="font-medium">{value}</dd>
              </div>
            ))}
          </dl>
        )}
        <ErrorNotice error={entitlements.error} />
      </div>
    </Section>
  );
}

function PlanEntitlements({ plan }: { plan: CommercialPlan }) {
  const entries = Object.entries(plan.entitlements);
  if (entries.length === 0) return <span className="text-muted-foreground">—</span>;
  return (
    <span className="text-xs">
      {entries.map(([k, v]) => (
        <span key={k} className="mr-2 inline-block">
          {k}: {v}
        </span>
      ))}
    </span>
  );
}

function PlansSection({ workspace }: { workspace: Workspace }) {
  const { client } = useDeveloperPortal();
  const commercial = useQuery({
    queryKey: ["developer-portal", workspace.id, "commercial"],
    queryFn: () => client.call<OrganizationCommercial>({ operation: "getOrganizationCommercial", path: { organizationId: workspace.id } }),
    ...retry,
  });
  const c = commercial.data;
  return (
    <Section title="Plan" description="Plan assignments are made by CF Meet. Self-service plan changes and payments are not available.">
      <div className="space-y-4 text-sm">
        {commercial.isLoading ? (
          <Loading />
        ) : commercial.error ? (
          <ErrorNotice error={commercial.error} onRetry={() => void commercial.refetch()} />
        ) : c ? (
          <>
            <dl className="grid gap-3 sm:grid-cols-2">
              <div>
                <dt className="text-muted-foreground">Workspace plan</dt>
                <dd className="font-medium">
                  {c.planAssignment ? `${c.planAssignment.plan?.displayName ?? c.planAssignment.planCode} (v${c.planAssignment.planVersion})` : "No plan assigned (platform defaults apply)"}
                </dd>
                {c.planAssignment && (
                  <dd className="text-xs text-muted-foreground">
                    Since {formatTimestamp(c.planAssignment.effectiveFrom)} · source {c.planAssignment.source.toLowerCase().replace(/_/g, " ")}
                  </dd>
                )}
              </div>
              <div>
                <dt className="text-muted-foreground">Billing account reference</dt>
                <dd className="font-medium">
                  {c.billingAccount ? `${c.billingAccount.classification.toLowerCase().replace(/_/g, " ")} · ${c.billingAccount.status.toLowerCase()}` : "None"}
                </dd>
                <dd className="text-xs text-muted-foreground">A reference record only; no charges are made from it.</dd>
              </div>
            </dl>
            {c.projectPlanAssignments.length > 0 && (
              <Table head={["Project", "Plan", "Since"]}>
                {c.projectPlanAssignments.map((a) => (
                  <tr key={a.id}>
                    <Td>{a.projectId}</Td>
                    <Td>{a.plan?.displayName ?? `${a.planCode} v${a.planVersion}`}</Td>
                    <Td>{formatTimestamp(a.effectiveFrom)}</Td>
                  </tr>
                ))}
              </Table>
            )}
          </>
        ) : null}
        <PublicPlans embedded />
      </div>
    </Section>
  );
}

function PublicPlans({ embedded }: { embedded?: boolean }) {
  const { client } = useDeveloperPortal();
  const plans = useQuery({
    queryKey: ["developer-portal", "commercial-plans"],
    queryFn: () => client.call<Page<CommercialPlan>>({ operation: "listCommercialPlans" }),
    staleTime: 5 * 60_000,
    ...retry,
  });
  const body = plans.isLoading ? (
    <Loading />
  ) : plans.error ? (
    describeError(plans.error).title === "Not allowed" ? null : <ErrorNotice error={plans.error} onRetry={() => void plans.refetch()} />
  ) : plans.data && plans.data.items.length > 0 ? (
    <Table head={["Plan", "Version", "Entitlements"]}>
      {plans.data.items.map((p) => (
        <tr key={p.id}>
          <Td className="font-medium">{p.displayName}</Td>
          <Td>v{p.version}</Td>
          <Td>
            <PlanEntitlements plan={p} />
          </Td>
        </tr>
      ))}
    </Table>
  ) : (
    <Empty>No plans published.</Empty>
  );
  if (embedded) {
    return (
      <div className="space-y-2">
        <p className="font-medium">Available plans (entitlements only, no prices)</p>
        {body}
      </div>
    );
  }
  return (
    <Section title="Available plans" description="Entitlement records only: no prices exist and plans cannot be bought here.">
      {body}
    </Section>
  );
}

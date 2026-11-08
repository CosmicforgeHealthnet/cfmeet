"use client";

/**
 * Organization Enterprise Calling administration (Section 12 follow-up, release step 7).
 *
 * Shown for an organization workspace only, and only when the capabilities read grants
 * `calling.enterprise.manage` (default deny). Every read goes through Meeting's relay
 * `/api/v2/organizations/{id}/calling/{path}` in the ORGANIZATION's own Calling scope — never
 * the shared CF Meet scope / CALLING_ADMIN_PRINCIPAL_IDS allowlist. The backend is behind
 * flags (`ORGANIZATION_CALLING_SCOPE=shared` by default → 409 ORGANIZATION_CALLING_DISABLED);
 * that answer is shown as "not enabled for this organization", never as an empty success.
 */

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Building2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useWorkspace, workspaceQueryKey } from "@/context/WorkspaceContext";
import { capabilityReasonText } from "@/lib/workspace/workspace-core";
import { callingErrorMessage } from "@/lib/calling/errors";
import { organizationCallingService, organizationCallingUnavailable, type OrganizationCallingUnavailable } from "@/services/calling/organization-calling.service";

export const ORGANIZATION_CALLING_CAPABILITY = "calling.enterprise.manage";

type Tab = "overview" | "queues" | "extensions" | "recordings";

const unavailableText: Record<OrganizationCallingUnavailable, { title: string; body: string }> = {
  NOT_ENABLED: { title: "Enterprise calling is not enabled for this organization", body: "Your CF Meet operator has not turned on organization calling for this organization yet. Personal calls are not affected." },
  PROVISIONING_PENDING: { title: "Organization calling is being set up", body: "The organization's calling space is still being provisioned. Try again later." },
  FORBIDDEN: { title: "Not available for your role", body: "Managing enterprise calling needs the organization's calling administration permission, and the organization policy must allow calling." },
  SUSPENDED: { title: "Organization suspended", body: "Calling administration is unavailable while the organization is suspended." },
  UNAVAILABLE: { title: "Calling is temporarily unavailable", body: "Organization calling could not be reached. Try again in a moment." },
};

function useOrganizationCalling<T>(organizationId: string, path: string, params?: Record<string, string | number | undefined>, enabled = true) {
  return useQuery({
    queryKey: workspaceQueryKey(organizationId, "calling", path, params ?? {}),
    queryFn: () => organizationCallingService.request<T>(organizationId, "GET", path, { params }),
    enabled,
    retry: false,
    staleTime: 15_000,
  });
}

export default function OrganizationCallingAdmin() {
  const workspace = useWorkspace();
  const organizationId = workspace.organizationId;
  const decision = workspace.decision(ORGANIZATION_CALLING_CAPABILITY);

  if (workspace.isLoading) return <div className="p-6"><Spinner /></div>;
  if (workspace.kind !== "ORGANIZATION" || !organizationId) {
    return <Notice title="Choose an organization" body="Organization calling is managed per organization. Switch to an organization workspace you administer." />;
  }
  if (!decision.granted) {
    return <Notice title="Not available in this workspace" body={capabilityReasonText(decision.reason)} />;
  }
  return <OrganizationCallingTabs key={organizationId} organizationId={organizationId} name={workspace.workspace?.displayName ?? "Organization"} />;
}

function Notice({ title, body }: { title: string; body: string }) {
  return (
    <main className="mx-auto max-w-3xl space-y-2 p-4 md:p-6">
      <h1 className="text-2xl font-bold tracking-tight">Organization calling</h1>
      <section role="status" className="rounded-2xl border border-border bg-card p-5"><h2 className="font-semibold">{title}</h2><p className="mt-1 text-sm text-muted-foreground">{body}</p></section>
    </main>
  );
}

function OrganizationCallingTabs({ organizationId, name }: { organizationId: string; name: string }) {
  const [tab, setTab] = useState<Tab>("overview");
  // The effective policy doubles as the "is the relay live for this organization" probe.
  const policy = useOrganizationCalling<{ policyId: string | null; version: number; rules: Record<string, unknown> }>(organizationId, "v1/enterprise/effective-policy");
  const blocked = organizationCallingUnavailable(policy.error);
  if (policy.isPending) return <div className="p-6"><Spinner /></div>;
  if (blocked) return <Notice title={unavailableText[blocked].title} body={unavailableText[blocked].body} />;

  return (
    <main className="mx-auto max-w-6xl space-y-5 p-4 md:p-6">
      <header className="flex items-center gap-3">
        <Building2 className="size-6" />
        <div><h1 className="text-2xl font-bold tracking-tight">Organization calling</h1><p className="text-sm text-muted-foreground">{name} · changes are audited with your name.</p></div>
      </header>
      <nav aria-label="Organization calling" className="flex flex-wrap gap-2">
        {(["overview", "queues", "extensions", "recordings"] as Tab[]).map((t) => (
          <Button key={t} size="sm" variant={tab === t ? "default" : "outline"} aria-pressed={tab === t} onClick={() => setTab(t)}>{t[0].toUpperCase() + t.slice(1)}</Button>
        ))}
      </nav>
      {policy.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(policy.error)}</p> : null}
      {tab === "overview" ? <Overview organizationId={organizationId} policy={policy.data} /> : null}
      {tab === "queues" ? <Queues organizationId={organizationId} /> : null}
      {tab === "extensions" ? <Extensions organizationId={organizationId} /> : null}
      {tab === "recordings" ? <Recordings organizationId={organizationId} /> : null}
    </main>
  );
}

function Panel({ title, query, children }: { title: string; query: { isPending: boolean; error: unknown }; children: React.ReactNode }) {
  const blocked = organizationCallingUnavailable(query.error);
  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
      <h2 className="font-semibold">{title}</h2>
      {query.isPending ? <Spinner /> : query.error ? <p role="alert" className="text-sm text-destructive">{blocked ? unavailableText[blocked].title : callingErrorMessage(query.error)}</p> : children}
    </section>
  );
}

type Limits = { limits: Array<{ limit: string; window: string; scope: string; ceiling: number; current: number; remaining: number }> };

function Overview({ organizationId, policy }: { organizationId: string; policy?: { policyId: string | null; version: number; rules: Record<string, unknown> } }) {
  const limits = useOrganizationCalling<Limits>(organizationId, "v1/calling/limits");
  const queues = useOrganizationCalling<{ queues: Array<{ queueId: string; waiting?: number }> }>(organizationId, "v1/enterprise/queues", { limit: 100, status: "ACTIVE" });
  const extensions = useOrganizationCalling<{ extensions: unknown[]; nextCursor: string | null }>(organizationId, "v1/enterprise/extensions", { limit: 100, status: "ACTIVE" });
  return (
    <div className="grid gap-4 md:grid-cols-2">
      <Panel title="At a glance" query={{ isPending: queues.isPending || extensions.isPending, error: queues.error ?? extensions.error }}>
        <dl className="grid grid-cols-2 gap-3 text-sm">
          <div><dt className="text-muted-foreground">Active extensions</dt><dd className="text-xl font-semibold">{extensions.data?.extensions.length ?? 0}{extensions.data?.nextCursor ? "+" : ""}</dd></div>
          <div><dt className="text-muted-foreground">Active queues</dt><dd className="text-xl font-semibold">{queues.data?.queues.length ?? 0}</dd></div>
          <div><dt className="text-muted-foreground">Calls waiting</dt><dd className="text-xl font-semibold">{queues.data?.queues.reduce((n, q) => n + (typeof q.waiting === "number" ? q.waiting : 0), 0) ?? 0}</dd></div>
          <div><dt className="text-muted-foreground">Calling policy</dt><dd className="text-sm font-medium">{policy?.policyId ? `Version ${policy.version}` : "Default policy"}</dd></div>
        </dl>
      </Panel>
      <Panel title="Limits" query={limits}>
        {limits.data?.limits.length ? (
          <ul className="space-y-2 text-sm">{limits.data.limits.map((l) => (
            <li key={`${l.limit}-${l.scope}`} className="flex justify-between gap-2"><span>{l.limit.replaceAll("_", " ").toLowerCase()}{l.window !== "NONE" ? ` / ${l.window.toLowerCase()}` : ""}</span><span className="tabular-nums text-muted-foreground">{l.current} of {l.ceiling}</span></li>
          ))}</ul>
        ) : <p className="text-sm text-muted-foreground">No limits are reported.</p>}
      </Panel>
    </div>
  );
}

type Queue = { queueId: string; name: string; capacity: number; maxWaitSeconds: number; members: unknown[]; waiting?: number; status: string };
type QueueEntry = { entryId: string; state: string; position: number | null; enteredAt: string; offerCount: number };

function Queues({ organizationId }: { organizationId: string }) {
  const queues = useOrganizationCalling<{ queues: Queue[] }>(organizationId, "v1/enterprise/queues", { limit: 100 });
  const [open, setOpen] = useState<string | null>(null);
  const entries = useOrganizationCalling<{ entries: QueueEntry[] }>(organizationId, `v1/enterprise/queues/${open ?? "none"}/entries`, { limit: 100 }, !!open);
  return (
    <Panel title="Call queues" query={queues}>
      {!queues.data?.queues.length ? <p className="text-sm text-muted-foreground">No queues are configured.</p> : (
        <ul className="divide-y divide-border">{queues.data.queues.map((q) => (
          <li key={q.queueId} className="space-y-2 py-3">
            <div className="flex flex-wrap items-center gap-3 text-sm">
              <span className="flex-1 font-medium">{q.name}</span>
              <span className="text-muted-foreground">{q.members.length} agents · capacity {q.capacity} · max wait {q.maxWaitSeconds}s · {q.status.toLowerCase()}</span>
              <Button size="sm" variant="outline" onClick={() => setOpen(open === q.queueId ? null : q.queueId)}>{open === q.queueId ? "Hide" : "Live entries"}</Button>
            </div>
            {open === q.queueId ? (entries.isPending ? <Spinner /> : entries.error ? <p className="text-sm text-destructive">{callingErrorMessage(entries.error)}</p> : !entries.data?.entries.length ? <p className="text-sm text-muted-foreground">No calls waiting.</p> : (
              <ul className="space-y-1 text-xs text-muted-foreground">{entries.data.entries.map((e) => <li key={e.entryId}>#{e.position ?? "–"} · {e.state.toLowerCase()} · since {new Date(e.enteredAt).toLocaleTimeString()} · {e.offerCount} offers</li>)}</ul>
            )) : null}
          </li>
        ))}</ul>
      )}
    </Panel>
  );
}

type Extension = { extensionId: string; extensionNumber: string; displayLabel: string; destination: { type: string } | null; status: string };

function Extensions({ organizationId }: { organizationId: string }) {
  const extensions = useOrganizationCalling<{ extensions: Extension[]; nextCursor: string | null }>(organizationId, "v1/enterprise/extensions", { limit: 100 });
  return (
    <Panel title="Extensions" query={extensions}>
      {!extensions.data?.extensions.length ? <p className="text-sm text-muted-foreground">No extensions are configured.</p> : (
        <table className="w-full text-left text-sm">
          <thead className="text-muted-foreground"><tr><th className="py-1">Number</th><th>Label</th><th>Routes to</th><th>Status</th></tr></thead>
          <tbody>{extensions.data.extensions.map((x) => <tr key={x.extensionId} className="border-t border-border"><td className="py-2 font-mono">{x.extensionNumber}</td><td>{x.displayLabel}</td><td>{x.destination?.type.replaceAll("_", " ").toLowerCase() ?? "—"}</td><td>{x.status.toLowerCase()}</td></tr>)}</tbody>
        </table>
      )}
      {extensions.data?.nextCursor ? <p className="text-xs text-muted-foreground">Showing the first 100 extensions.</p> : null}
    </Panel>
  );
}

type Recording = { recordingId: string; callId: string; state: string; startedAt: string; durationMs: number | null; stopReason: string | null };

function Recordings({ organizationId }: { organizationId: string }) {
  const recordings = useOrganizationCalling<{ recordings: Recording[]; nextCursor: string | null }>(organizationId, "v1/enterprise/recordings", { limit: 50 });
  const [error, setError] = useState<string | null>(null);
  const [playing, setPlaying] = useState<{ id: string; url: string } | null>(null);
  const play = async (recordingId: string) => {
    setError(null);
    try {
      // Access is audited for this administrator by Calling (actorPrincipalId); URL ≤ 900 s, never stored.
      const access = await organizationCallingService.request<{ url: string; contentType: string; sha256: string }>(organizationId, "POST", `v1/enterprise/recordings/${recordingId}/media-access`, { body: {} });
      const response = await fetch(access.url, { credentials: "omit", cache: "no-store", referrerPolicy: "no-referrer" });
      if (!response.ok) throw new Error("The recording could not be downloaded.");
      const bytes = await response.arrayBuffer();
      const digest = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (b) => b.toString(16).padStart(2, "0")).join("");
      if (access.sha256 && digest !== access.sha256.toLowerCase()) throw new Error("The recording failed its integrity check and was not played.");
      if (playing) URL.revokeObjectURL(playing.url);
      setPlaying({ id: recordingId, url: URL.createObjectURL(new Blob([bytes], { type: access.contentType })) });
    } catch (e) { setError(callingErrorMessage(e)); }
  };
  return (
    <Panel title="Recordings" query={recordings}>
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {!recordings.data?.recordings.length ? <p className="text-sm text-muted-foreground">No recordings.</p> : (
        <ul className="divide-y divide-border">{recordings.data.recordings.map((r) => (
          <li key={r.recordingId} className="space-y-2 py-2 text-sm">
            <div className="flex flex-wrap items-center gap-3">
              <span className="flex-1">{new Date(r.startedAt).toLocaleString()} · {r.state.toLowerCase()}{r.durationMs !== null ? ` · ${Math.round(r.durationMs / 1000)} s` : ""}{r.stopReason ? ` · ${r.stopReason.replaceAll("_", " ").toLowerCase()}` : ""}</span>
              <Button size="sm" variant="outline" disabled={r.state !== "READY"} onClick={() => void play(r.recordingId)}>Play</Button>
            </div>
            {playing?.id === r.recordingId ? <audio controls autoPlay src={playing.url} className="w-full" /> : null}
          </li>
        ))}</ul>
      )}
      {recordings.data?.nextCursor ? <p className="text-xs text-muted-foreground">Showing the 50 most recent recordings.</p> : null}
    </Panel>
  );
}

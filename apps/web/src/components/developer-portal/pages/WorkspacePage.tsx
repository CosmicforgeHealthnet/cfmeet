"use client";

import { useState } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { Building2, Check, LifeBuoy, Mail, Plus, Settings2, ShieldCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { NiceSelect } from "@/components/ui/nice-select";
import { cn } from "@/lib/utils";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { PORTAL_BASE } from "@/lib/developer-platform/navigation";
import { retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import type { CredentialPolicy, SupportProfile, Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, WORKSPACES_QUERY_KEY } from "../DeveloperPortalProvider";
import { ConfirmButton, ErrorNotice, Field, friendlyTime, inputClass, Loading, PageHeader, StatusBadge, useConsoleMutation } from "../ui";

const nice = (v?: string | null) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : "—");
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
const retry = { retry: (count: number, error: unknown) => shouldRetryQuery(count, error), retryDelay: (attempt: number, error: unknown) => retryDelayMs(attempt, error) };

function Card({ icon: Icon, title, description, children, tone }: { icon: typeof Building2; title: string; description?: string; children: React.ReactNode; tone?: "danger" }) {
  return (
    <section className={cn("rounded-2xl border bg-card p-5 sm:p-6", tone === "danger" ? "border-red-500/30" : "border-border")}>
      <div className="flex items-start gap-3">
        <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-xl", tone === "danger" ? "bg-red-500/10 text-red-600" : "bg-muted text-foreground")}><Icon className="size-5" /></span>
        <div className="min-w-0">
          <h2 className="font-semibold">{title}</h2>
          {description && <p className="mt-0.5 text-sm text-muted-foreground">{description}</p>}
        </div>
      </div>
      <div className="mt-5">{children}</div>
    </section>
  );
}

export default function WorkspacePage() {
  const { workspaces, workspace, selectWorkspace } = useDeveloperPortal();
  const [creating, setCreating] = useState(false);
  return (
    <div className="space-y-8">
      <PageHeader
        title="Workspaces"
        description="Your company's home on the developer platform. It's separate from the organizations you use for meetings and chat."
        actions={
          <>
            <Link href={`${PORTAL_BASE}/invitations/accept`} className="inline-flex h-10 items-center gap-2 rounded-full px-4 text-sm font-semibold ring-1 ring-border hover:bg-muted"><Mail className="size-4" />Use an invitation</Link>
            <button type="button" onClick={() => setCreating(true)} className="inline-flex h-10 items-center gap-2 rounded-full bg-primary pl-3.5 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New workspace</button>
          </>
        }
      />

      {workspaces.length === 0 ? (
        <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-16 text-center">
          <span className="flex size-16 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Building2 className="size-7" /></span>
          <p className="mt-4 text-lg font-semibold">No workspace yet</p>
          <p className="mt-1 max-w-sm text-sm text-muted-foreground">Create one for your company. You&apos;ll be its owner and can invite your team.</p>
          <button type="button" onClick={() => setCreating(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New workspace</button>
        </div>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {workspaces.map((w) => {
            const on = workspace?.id === w.id;
            return (
              <li key={w.id}>
                <button type="button" onClick={() => selectWorkspace(w.id)} aria-current={on ? "true" : undefined}
                  className={cn("flex w-full items-center gap-3 rounded-2xl border bg-card p-4 text-left transition-colors", on ? "border-primary/50 ring-1 ring-primary/30" : "border-border hover:bg-muted/40")}>
                  <span className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-[#0B1220] text-sm font-semibold text-sky-300">{initials(w.displayName)}</span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-semibold">{w.displayName}</span>
                    <span className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">{nice(w.myRole)}{w.state !== "ACTIVE" && <StatusBadge status={w.state} />}</span>
                  </span>
                  {on ? <span className="flex items-center gap-1 rounded-full bg-primary/10 px-2.5 py-1 text-xs font-semibold text-primary"><Check className="size-3.5" />Current</span> : <span className="text-xs font-semibold text-primary">Switch</span>}
                </button>
              </li>
            );
          })}
        </ul>
      )}

      {workspace && (
        <div className="space-y-4">
          <h2 className="text-lg font-semibold">{workspace.displayName} settings</h2>
          <div className="grid items-start gap-4 lg:grid-cols-2">
            <WorkspaceSettings key={workspace.id} workspace={workspace} />
            <CredentialPolicySection key={`policy-${workspace.id}`} workspace={workspace} />
            <SupportProfileSection key={`support-${workspace.id}`} workspace={workspace} />
            <CloseWorkspace key={`close-${workspace.id}`} workspace={workspace} />
          </div>
        </div>
      )}

      <CreateWorkspace open={creating} onOpenChange={setCreating} />
    </div>
  );
}

function CreateWorkspace({ open, onOpenChange }: { open: boolean; onOpenChange: (v: boolean) => void }) {
  const { client, selectWorkspace } = useDeveloperPortal();
  const [displayName, setDisplayName] = useState("");
  const [supportEmail, setSupportEmail] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const create = useConsoleMutation(
    () =>
      client.call<Workspace>({
        operation: "createOrganization",
        body: { displayName: displayName.trim(), ...(supportEmail.trim() ? { supportEmail: supportEmail.trim() } : {}) },
        idempotencyKey,
      }),
    [WORKSPACES_QUERY_KEY],
    (created) => {
      selectWorkspace(created.id);
      setDisplayName("");
      setSupportEmail("");
      setIdempotencyKey(newIdempotencyKey());
      onOpenChange(false);
    },
    "Workspace created.",
  );
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader className="text-left">
          <DialogTitle>New workspace</DialogTitle>
          <DialogDescription>You&apos;ll be its owner. You can rename it later.</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); if (displayName.trim()) create.mutate(undefined); }}>
          <Field label="Name">
            <input autoFocus className={inputClass} value={displayName} maxLength={120} required placeholder="e.g. Cosmic Forge Health" onChange={(e) => setDisplayName(e.target.value)} />
          </Field>
          <Field label="Support email (optional)" hint="Where CF Meet can reach your team about this workspace.">
            <input className={inputClass} type="email" value={supportEmail} placeholder="dev@yourcompany.com" onChange={(e) => setSupportEmail(e.target.value)} />
          </Field>
          <ErrorNotice error={create.error} />
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={() => onOpenChange(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <Button type="submit" className="rounded-full" loading={create.isPending} disabled={!displayName.trim()}>Create workspace</Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function WorkspaceSettings({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const [displayName, setDisplayName] = useState(workspace.displayName);
  const rename = useConsoleMutation(
    () =>
      client.call<Workspace>({
        operation: "updateOrganization",
        path: { organizationId: workspace.id },
        body: { expectedVersion: workspace.version, displayName: displayName.trim() },
      }),
    [WORKSPACES_QUERY_KEY],
    undefined,
    "Name saved.",
  );
  const changed = displayName.trim() && displayName.trim() !== workspace.displayName;
  return (
    <Card icon={Settings2} title="General" description={`Created ${friendlyTime(workspace.createdAt)} · you're ${nice(workspace.myRole).toLowerCase()}`}>
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); if (changed) rename.mutate(undefined); }}>
        <Field label="Workspace name">
          <input className={inputClass} value={displayName} maxLength={120} disabled={!can("OrganizationUpdate")} onChange={(e) => setDisplayName(e.target.value)} />
        </Field>
        {can("OrganizationUpdate") && changed && (
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => setDisplayName(workspace.displayName)} className="h-9 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <Button type="submit" size="sm" className="rounded-full" loading={rename.isPending}>Save</Button>
          </div>
        )}
        <ErrorNotice error={rename.error} />
      </form>
    </Card>
  );
}

const LIFETIMES = [
  { value: "30", label: "30 days" },
  { value: "90", label: "90 days", hint: "Recommended" },
  { value: "180", label: "6 months" },
  { value: "365", label: "1 year" },
];
const OVERLAPS = [
  { value: "0", label: "No overlap", hint: "The old key stops at once" },
  { value: "60", label: "1 hour" },
  { value: "1440", label: "24 hours", hint: "Recommended" },
  { value: "10080", label: "7 days" },
];

function CredentialPolicySection({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "credential-policy"];
  const policy = useQuery({ queryKey: key, queryFn: () => client.call<CredentialPolicy>({ operation: "getCredentialPolicy", path: { organizationId: workspace.id } }), ...retry });
  const [editing, setEditing] = useState(false);
  const [lifetime, setLifetime] = useState("90");
  const [overlap, setOverlap] = useState("1440");
  const save = useConsoleMutation(
    () => client.call<CredentialPolicy>({ operation: "putCredentialPolicy", path: { organizationId: workspace.id }, body: { maxLifetimeDays: Number(lifetime), maxRotationOverlapMinutes: Number(overlap) } }),
    [key],
    () => setEditing(false),
    "Key rules saved.",
  );
  const d = policy.data;
  const fmtOverlap = (m: number) => (m === 0 ? "none" : m < 60 ? `${m} min` : m < 1440 ? `${m / 60} h` : `${m / 1440} days`);
  return (
    <Card icon={ShieldCheck} title="Key rules" description="Limits every API key in this workspace must follow.">
      {policy.isLoading ? <Loading /> : policy.error ? <ErrorNotice error={policy.error} onRetry={() => void policy.refetch()} /> : d ? (
        editing ? (
          <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); save.mutate(undefined); }}>
            <NiceSelect label="Keys expire after at most" value={lifetime} onChange={setLifetime} options={LIFETIMES} />
            <NiceSelect label="When rotating, the old key may keep working for up to" value={overlap} onChange={setOverlap} options={OVERLAPS} />
            <ErrorNotice error={save.error} />
            <div className="flex justify-end gap-2">
              <button type="button" onClick={() => setEditing(false)} className="h-9 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
              <Button type="submit" size="sm" className="rounded-full" loading={save.isPending}>Save rules</Button>
            </div>
          </form>
        ) : (
          <div className="space-y-3">
            <dl className="divide-y divide-border rounded-xl bg-muted/40 text-sm">
              <div className="flex justify-between gap-3 px-4 py-3"><dt className="text-muted-foreground">Longest key life</dt><dd className="font-semibold">{d.configured && d.maxLifetimeDays != null ? `${d.maxLifetimeDays} days` : "Platform default"}</dd></div>
              <div className="flex justify-between gap-3 px-4 py-3"><dt className="text-muted-foreground">Rotation overlap</dt><dd className="font-semibold">{d.configured && d.maxRotationOverlapMinutes != null ? fmtOverlap(d.maxRotationOverlapMinutes) : "Platform default"}</dd></div>
            </dl>
            {can("OrganizationUpdate") && <button type="button" onClick={() => setEditing(true)} className="text-sm font-semibold text-primary hover:underline">Change rules</button>}
          </div>
        )
      ) : null}
    </Card>
  );
}

function SupportProfileSection({ workspace }: { workspace: Workspace }) {
  const { client } = useDeveloperPortal();
  const profile = useQuery({ queryKey: ["developer-portal", workspace.id, "support-profile"], queryFn: () => client.call<SupportProfile>({ operation: "getSupportProfile", path: { organizationId: workspace.id } }), ...retry });
  const d = profile.data;
  return (
    <Card icon={LifeBuoy} title="Support" description="Quote your reference when you contact CF Meet.">
      {profile.isLoading ? <Loading /> : profile.error ? <ErrorNotice error={profile.error} onRetry={() => void profile.refetch()} /> : d ? (
        <dl className="divide-y divide-border rounded-xl bg-muted/40 text-sm">
          <div className="flex justify-between gap-3 px-4 py-3"><dt className="text-muted-foreground">Support plan</dt><dd className="font-semibold">{nice(d.supportTier)}</dd></div>
          <div className="flex items-center justify-between gap-3 px-4 py-3"><dt className="text-muted-foreground">Production</dt><dd><StatusBadge status={d.productionStatus} /></dd></div>
          <div className="flex justify-between gap-3 px-4 py-3"><dt className="text-muted-foreground">Your reference</dt><dd className="font-mono text-xs font-semibold">{d.supportReference ?? "—"}</dd></div>
        </dl>
      ) : null}
    </Card>
  );
}

function CloseWorkspace({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const close = useConsoleMutation(() => client.call<Workspace>({ operation: "closeOrganization", path: { organizationId: workspace.id } }), [WORKSPACES_QUERY_KEY], undefined, "Workspace closed.");
  if (!can("OrganizationClose")) return null;
  return (
    <Card icon={Building2} title="Close workspace" description="Stops every project, environment and API key. You can still read its history." tone="danger">
      <ConfirmButton label="Close workspace" message="Close this workspace? Every key stops working and this can't be undone here." destructive pending={close.isPending} onConfirm={() => close.mutate(undefined)} />
      <div className="mt-3"><ErrorNotice error={close.error} /></div>
    </Card>
  );
}

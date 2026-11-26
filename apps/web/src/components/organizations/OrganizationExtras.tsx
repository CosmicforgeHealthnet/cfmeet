"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import { NiceSelect } from "@/components/ui/nice-select";
import apiV2 from "@/services/v2/api";
import { cn } from "@/lib/utils";
import type { BusinessOrganization } from "@/types/generated/meeting-service";
import { OrgError, Section } from "./shared";

const org = (id: string) => `/organizations/${encodeURIComponent(id)}`;
const errorCode = (e: unknown) => (e as { response?: { data?: { error?: { code?: string } } } } | null)?.response?.data?.error?.code;

/* ─── Policies ─────────────────────────────────────────────────────────── */

type Policy = {
  memberDirectory: { visibility: "MEMBERS" | "MANAGERS" | "ADMINS"; searchable: boolean };
  guests: { invitationsAllowed: boolean };
  meetings: { hostCreation: "MEMBERS" | "MANAGERS" | "ADMINS" };
  features: { meetings: boolean; messaging: boolean; calling: boolean };
  retention: { hintDays: number | null };
  delegatedAdministration: { managersMayInvite: boolean; managersMayCreateTeams: boolean; adminsMayManagePolicies: boolean };
};
type PolicyView = { revision: number; policy: Policy; updatedAt: string | null };

const ROLE_OPTIONS = [
  { value: "MEMBERS", label: "Everyone" },
  { value: "MANAGERS", label: "Managers and admins" },
  { value: "ADMINS", label: "Admins only" },
];

/**
 * The organization's policy (GET/PATCH /organizations/:id/policies): who sees members, guests,
 * who hosts meetings, which features are on, a retention hint, and (owner) delegated
 * administration. Each change is a compare-and-swap on `revision`; a conflict reloads.
 */
export function OrganizationPolicies({ organization, owner }: { organization: BusinessOrganization; owner: boolean }) {
  const client = useQueryClient();
  const key = ["v2", "organizations", organization.id, "policies"];
  const view = useQuery({ queryKey: key, queryFn: async () => (await apiV2.get<PolicyView>(`${org(organization.id)}/policies`)).data, retry: false });
  const [days, setDays] = useState<string | null>(null);
  const save = useMutation({
    mutationFn: async (patch: Record<string, unknown>) =>
      (await apiV2.patch<PolicyView>(`${org(organization.id)}/policies`, { expectedRevision: view.data!.revision, policy: patch })).data,
    onSuccess: (v) => client.setQueryData(key, v),
    onError: (e) => { if (errorCode(e) === "VERSION_CONFLICT") void view.refetch(); },
  });
  if (view.isPending) return <Section title="Organization policies"><div className="h-24 animate-pulse rounded-xl bg-muted" /></Section>;
  if (view.isError || !view.data) return <Section title="Organization policies"><OrgError error={view.error} retry={() => void view.refetch()} /></Section>;
  const p = view.data.policy;
  const toggle = (label: string, hint: string, on: boolean, patch: Record<string, unknown>, testId: string) => (
    <button type="button" role="switch" aria-checked={on} disabled={save.isPending} onClick={() => save.mutate(patch)} data-testid={testId}
      className="flex w-full items-center gap-3 py-2 text-left disabled:opacity-60">
      <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{label}</span><span className="block text-xs text-muted-foreground">{hint}</span></span>
      <span aria-hidden className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", on ? "bg-primary" : "bg-muted-foreground/30")}>
        <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", on ? "left-[1.125rem]" : "left-0.5")} />
      </span>
    </button>
  );
  const choose = (label: string, value: string, patch: (v: string) => Record<string, unknown>) => (
    <div className="flex items-center gap-3 py-2">
      <span className="min-w-0 flex-1 text-sm font-medium">{label}</span>
      <NiceSelect className="w-52" value={value} onChange={(v) => save.mutate(patch(v))} options={ROLE_OPTIONS} />
    </div>
  );
  return (
    <Section title="Organization policies" description="What people in the organization may do. Owners always keep full administration.">
      <div className="divide-y divide-border" data-testid="org-policies">
        {choose("Who can see the member list", p.memberDirectory.visibility, (v) => ({ memberDirectory: { visibility: v } }))}
        {toggle("Members can find each other", "Colleague search suggests people in this organization.", p.memberDirectory.searchable, { memberDirectory: { searchable: !p.memberDirectory.searchable } }, "policy-searchable")}
        {toggle("Guests can be invited", "Admins can invite people from outside as guests.", p.guests.invitationsAllowed, { guests: { invitationsAllowed: !p.guests.invitationsAllowed } }, "policy-guests")}
        {choose("Who can host meetings", p.meetings.hostCreation, (v) => ({ meetings: { hostCreation: v } }))}
        {toggle("Meetings", "Meetings for this organization.", p.features.meetings, { features: { meetings: !p.features.meetings } }, "policy-feature-meetings")}
        {toggle("Chat", "Chats for this organization.", p.features.messaging, { features: { messaging: !p.features.messaging } }, "policy-feature-messaging")}
        {toggle("Calls", "Voice and video calls for this organization.", p.features.calling, { features: { calling: !p.features.calling } }, "policy-feature-calling")}
        <form className="flex items-center gap-3 py-2" onSubmit={(e) => {
          e.preventDefault();
          const n = (days ?? "").trim() === "" ? null : Number(days);
          if (n !== null && (!Number.isInteger(n) || n < 1 || n > 3650)) return;
          save.mutate({ retention: { hintDays: n } }, { onSuccess: () => setDays(null) });
        }}>
          <span className="min-w-0 flex-1"><span className="block text-sm font-medium">Keep content for</span><span className="block text-xs text-muted-foreground">A hint, in days (1–3650). Leave empty for no hint.</span></span>
          <input inputMode="numeric" aria-label="Retention in days" value={days ?? (p.retention.hintDays?.toString() ?? "")} onChange={(e) => setDays(e.target.value.replace(/\D/g, ""))}
            className="h-10 w-24 rounded-xl border border-border bg-background px-3 text-sm" placeholder="No hint" />
          <Button type="submit" size="sm" variant="outline" disabled={days === null || save.isPending}>Save</Button>
        </form>
        {owner ? (
          <>
            {toggle("Managers can invite people", "Managers may invite members and guests.", p.delegatedAdministration.managersMayInvite, { delegatedAdministration: { managersMayInvite: !p.delegatedAdministration.managersMayInvite } }, "policy-managers-invite")}
            {toggle("Managers can create teams", "Managers may create teams.", p.delegatedAdministration.managersMayCreateTeams, { delegatedAdministration: { managersMayCreateTeams: !p.delegatedAdministration.managersMayCreateTeams } }, "policy-managers-teams")}
            {toggle("Admins can change these policies", "Turn off to keep policies to owners.", p.delegatedAdministration.adminsMayManagePolicies, { delegatedAdministration: { adminsMayManagePolicies: !p.delegatedAdministration.adminsMayManagePolicies } }, "policy-admins-policies")}
          </>
        ) : null}
      </div>
      {save.isError ? <OrgError error={save.error} /> : null}
    </Section>
  );
}

/* ─── Export ───────────────────────────────────────────────────────────── */

/** Owner: download everything the organization holds as one JSON file (GET /organizations/:id/export). */
export function OrganizationExport({ organization }: { organization: BusinessOrganization }) {
  const run = useMutation({
    mutationFn: async () => {
      const data = (await apiV2.get<unknown>(`${org(organization.id)}/export`)).data;
      const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = `${(organization.name || "organization").replace(/[\\/:*?"<>|]+/g, " ").trim()} export.json`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
    },
  });
  return (
    <Section title="Export organization data" description="The organization, its policies, members, teams, invitations and recent audit history, as one file. Each export is recorded in the audit log.">
      <Button variant="outline" disabled={run.isPending} onClick={() => run.mutate()} data-testid="org-export"><Download className="size-4" />{run.isPending ? "Preparing…" : "Export data"}</Button>
      {run.isError ? <OrgError error={run.error} /> : null}
    </Section>
  );
}

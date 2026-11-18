"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Flag } from "lucide-react";
import apiV2 from "@/services/v2/api";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ORG_CAPABILITIES, orgCan } from "@/lib/organizations/organization-core";
import type { BusinessOrganization } from "@/types/generated/meeting-service";
import { OrgError, Section, formatDateTime } from "./shared";

/** Messaging moderation needs this capability (server: `messaging.organization.moderate`). */
export const MODERATE_CAPABILITY = "messaging.organization.moderate";
export const canModerate = (org: BusinessOrganization) => org.viewer?.status === "ACTIVE" && Array.isArray(org.viewer.capabilities) && org.viewer.capabilities.includes(MODERATE_CAPABILITY);

type Report = { reportId: string; conversationId: string; securityProfile: string; reportedPrincipalId: string | null; messageIds: string[]; category: string; status: string; createdAt: string };
type ReportMessage = { messageId: string; managed?: { text: string | null }; e2ee?: { reporterDisclosure: string | null } };

const nice = (v: string) => v.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase());
const btn = "inline-flex h-9 items-center justify-center rounded-xl px-3 text-sm font-semibold disabled:opacity-50";

/** Organization → Moderation: messages people reported in the organization's chats. */
export default function OrganizationModeration({ org }: { org: BusinessOrganization }) {
  const base = `/organizations/${encodeURIComponent(org.id)}/messaging/moderation`;
  const reports = useQuery({ queryKey: ["org-moderation", org.id], queryFn: async () => (await apiV2.get<{ reports: Report[] }>(`${base}/reports`)).data.reports ?? [], retry: false });
  const [open, setOpen] = useState<Report | null>(null);
  const items = reports.data ?? [];
  return (
    <Section title="Reported messages" description="Messages people in your organization reported. Every action is recorded.">
      {reports.isLoading ? <div className="h-16 animate-pulse rounded-xl bg-muted" /> : null}
      {reports.isError ? <OrgError error={reports.error} retry={() => void reports.refetch()} /> : null}
      {reports.data && items.length === 0 ? <p className="text-sm text-muted-foreground">Nothing reported.</p> : null}
      <ul className="divide-y divide-border">
        {items.map((r) => (
          <li key={r.reportId}>
            <button type="button" onClick={() => setOpen(r)} className="flex w-full items-center gap-3 py-3 text-left hover:bg-muted/40">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-destructive/10 text-destructive"><Flag className="size-4" /></span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium">{nice(r.category)}</span>
                <span className="block text-xs text-muted-foreground">{r.messageIds.length} {r.messageIds.length === 1 ? "message" : "messages"} · {formatDateTime(r.createdAt)}</span>
              </span>
              <span className={r.status === "OPEN" ? "text-xs font-semibold text-amber-600" : "text-xs text-muted-foreground"}>{nice(r.status)}</span>
            </button>
          </li>
        ))}
      </ul>
      {open ? <ReportDialog base={base} orgId={org.id} report={open} onClose={() => setOpen(null)} /> : null}
    </Section>
  );
}

function ReportDialog({ base, orgId, report, onClose }: { base: string; orgId: string; report: Report; onClose: () => void }) {
  const queryClient = useQueryClient();
  const detail = useQuery({ queryKey: ["org-moderation", orgId, report.reportId], queryFn: async () => (await apiV2.get<{ messages?: ReportMessage[] }>(`${base}/reports/${report.reportId}`)).data, retry: false });
  const act = useMutation({
    mutationFn: async (actions: Record<string, unknown>[]) => {
      for (const a of actions) await apiV2.post(`${base}/moderation/actions`, { reason: `Report ${report.reportId}`, ...a });
    },
    onSuccess: async () => { await queryClient.invalidateQueries({ queryKey: ["org-moderation", orgId] }); onClose(); },
  });
  const messages = detail.data?.messages ?? [];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{nice(report.category)}</DialogTitle>
          <DialogDescription>{report.securityProfile === "PRIVATE_E2EE" ? "A private chat: you only see what the reporter shared." : "A managed chat."}</DialogDescription>
        </DialogHeader>
        {detail.isLoading ? <div className="h-16 animate-pulse rounded-xl bg-muted" /> : null}
        {detail.isError ? <OrgError error={detail.error} /> : null}
        <ul className="space-y-2">
          {messages.map((m) => {
            const text = m.managed?.text ?? m.e2ee?.reporterDisclosure ?? null;
            return <li key={m.messageId} className="rounded-xl bg-muted/60 px-3 py-2 text-sm">{text ?? <span className="text-muted-foreground">Content not available</span>}</li>;
          })}
        </ul>
        {act.isError ? <OrgError error={act.error} /> : null}
        {report.status === "OPEN" ? (
          <div className="flex flex-wrap justify-end gap-2">
            <button type="button" disabled={act.isPending} className={`${btn} border border-border hover:bg-muted`} onClick={() => act.mutate([{ kind: "DISMISS_REPORT", reportId: report.reportId }])}>Dismiss</button>
            {report.reportedPrincipalId ? <button type="button" disabled={act.isPending} className={`${btn} border border-border hover:bg-muted`} onClick={() => act.mutate([{ kind: "REMOVE_MEMBER", conversationId: report.conversationId, principalId: report.reportedPrincipalId, reportId: report.reportId }])}>Remove sender from chat</button> : null}
            <button type="button" disabled={act.isPending} className={`${btn} bg-destructive text-white hover:bg-destructive/90`} onClick={() => act.mutate(report.messageIds.map((messageId) => ({ kind: "REMOVE_MESSAGE", conversationId: report.conversationId, messageId, reportId: report.reportId })))}>Remove {report.messageIds.length === 1 ? "message" : "messages"}</button>
          </div>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

/** Organization → Settings: meeting rules that apply to every meeting space of the organization. */
export function MeetingRules({ org }: { org: BusinessOrganization }) {
  const allowed = orgCan(org, ORG_CAPABILITIES.policiesManage);
  const url = `/organizations/${encodeURIComponent(org.id)}/meeting-profile-policy`;
  const queryClient = useQueryClient();
  const policy = useQuery({ queryKey: ["org-meeting-policy", org.id], queryFn: async () => (await apiV2.get<{ policy: Record<string, Record<string, unknown>> }>(url)).data.policy ?? {}, enabled: allowed, retry: false });
  const save = useMutation({
    mutationFn: async (next: Record<string, unknown>) => (await apiV2.put(url, { policy: next })).data,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["org-meeting-policy", org.id] }),
  });
  if (!allowed) return null;
  const current = policy.data ?? {};
  // A rule left at its default isn't stored, so spaces keep their own setting.
  const toggle = (rule: Rule) => {
    const section = { ...(current[rule.section] ?? {}) };
    const on = (section[rule.key] ?? rule.fallback) === true;
    if (!on === rule.fallback) delete section[rule.key]; else section[rule.key] = !on;
    const next: Record<string, unknown> = { ...current, [rule.section]: section };
    if (!Object.keys(section).length) delete next[rule.section];
    save.mutate(next);
  };
  return (
    <Section title="Meeting rules" description="Applies to every meeting space in this organization. A space can be stricter, never looser.">
      {policy.isError ? <OrgError error={policy.error} retry={() => void policy.refetch()} /> : null}
      {save.isError ? <OrgError error={save.error} /> : null}
      <div className="divide-y divide-border">
        {RULES.map((rule) => (
          <label key={`${rule.section}.${rule.key}`} className="flex items-center justify-between gap-4 py-3">
            <span><span className="block text-sm font-medium">{rule.label}</span><span className="block text-xs text-muted-foreground">{rule.hint}</span></span>
            <input type="checkbox" role="switch" className="size-5 shrink-0 accent-primary" checked={(current[rule.section]?.[rule.key] ?? rule.fallback) === true} disabled={policy.isLoading || save.isPending} onChange={() => toggle(rule)} />
          </label>
        ))}
      </div>
    </Section>
  );
}

type Rule = { section: string; key: string; label: string; hint: string; fallback: boolean };
const RULES: Rule[] = [
  { section: "recording", key: "allowed", label: "Allow recording", hint: "Turn off to stop anyone recording meetings.", fallback: true },
  { section: "transcription", key: "allowed", label: "Allow transcripts", hint: "Turn off to stop live captions and transcripts.", fallback: true },
  { section: "exports", key: "chatExportAllowed", label: "Allow chat downloads", hint: "Lets hosts download in-meeting chat.", fallback: true },
  { section: "exports", key: "attendanceReportAllowed", label: "Allow attendance reports", hint: "Lets hosts see who joined and for how long.", fallback: true },
  { section: "exports", key: "pollReportAllowed", label: "Allow poll reports", hint: "Lets hosts download poll results.", fallback: true },
  { section: "dataSharing", key: "allowed", label: "Allow sharing meeting data", hint: "Turn off to keep meeting data inside the organization.", fallback: true },
  { section: "access", key: "publicAccessAllowed", label: "Allow people without an account", hint: "Turn off so only signed-in people can join.", fallback: true },
  { section: "access", key: "waitingRoomForced", label: "Always use a waiting room", hint: "Hosts admit everyone before they join.", fallback: false },
  { section: "intelligence", key: "consentRequired", label: "Ask before AI notes", hint: "Everyone must agree before AI notes start.", fallback: false },
  { section: "audit", key: "enhanced", label: "Detailed activity log", hint: "Records more about who did what in meetings.", fallback: false },
];

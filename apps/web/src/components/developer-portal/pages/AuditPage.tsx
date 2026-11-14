"use client";

import { useState } from "react";
import { Bot, CheckCircle2, Filter, KeyRound, ScrollText, ShieldCheck, User, Webhook, X, XCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ActivityEvent, Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { RequireWorkspace } from "../PortalShell";
import { Empty, ErrorNotice, Field, friendlyTime, inputClass, LoadMore, Loading, NotPermitted, PageHeader, usePagedList } from "../ui";

export default function AuditPage() {
  return (
    <RequireWorkspace>
      <Audit />
    </RequireWorkspace>
  );
}

function Audit() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  return (
    <div className="space-y-6">
      <PageHeader title="Audit log" description="Everything that changed in this workspace, who did it and when. Newest first." />
      {can("ActivityRead") ? <ActivityList key={workspace.id} workspace={workspace} /> : <NotPermitted what="the audit log" />}
    </div>
  );
}

const ACTION_PATTERN = /^[a-z0-9_.]{0,80}$/;
const ID_PATTERN = /^[0-9A-Za-z-]{0,64}$/;

/** credential.revoked → "Credential revoked" */
const sentence = (action: string) => {
  const t = action.replace(/[._]+/g, " ").trim();
  return t.charAt(0).toUpperCase() + t.slice(1);
};
const iconFor = (e: ActivityEvent) =>
  /credential|key/.test(e.action) ? KeyRound : /webhook/.test(e.action) ? Webhook : /member|invitation|role/.test(e.action) ? User : /scope|production|policy/.test(e.action) ? ShieldCheck : ScrollText;

function ActivityList({ workspace }: { workspace: Workspace }) {
  const [action, setAction] = useState("");
  const [projectId, setProjectId] = useState("");
  const [showFilters, setShowFilters] = useState(false);
  const [applied, setApplied] = useState<{ action: string; projectId: string }>({ action: "", projectId: "" });
  const list = usePagedList<ActivityEvent>(["developer-portal", workspace.id, "activity", applied.action, applied.projectId], {
    operation: "listActivity",
    path: { organizationId: workspace.id },
    query: { action: applied.action || undefined, projectId: applied.projectId || undefined },
  });
  const valid = ACTION_PATTERN.test(action.trim()) && ID_PATTERN.test(projectId.trim());
  const filtered = !!(applied.action || applied.projectId);
  const clear = () => { setAction(""); setProjectId(""); setApplied({ action: "", projectId: "" }); };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" onClick={() => setShowFilters(!showFilters)} aria-expanded={showFilters} className={cn("inline-flex h-9 items-center gap-1.5 rounded-full px-4 text-sm font-semibold ring-1 transition-colors", filtered ? "bg-primary/10 text-primary ring-primary/40" : "ring-border hover:bg-muted")}>
          <Filter className="size-4" />Filter{filtered ? " · on" : ""}
        </button>
        {filtered && <button type="button" onClick={clear} className="inline-flex h-9 items-center gap-1 rounded-full px-3 text-sm font-medium text-muted-foreground hover:bg-muted"><X className="size-4" />Clear</button>}
      </div>
      {showFilters && (
        <form className="grid gap-3 rounded-2xl border border-border bg-card p-4 sm:grid-cols-[1fr_1fr_auto] sm:items-end" onSubmit={(e) => { e.preventDefault(); if (valid) setApplied({ action: action.trim(), projectId: projectId.trim() }); }}>
          <Field label="What happened" hint="e.g. credential.revoked">
            <input className={cn(inputClass, "font-mono")} value={action} placeholder="credential.revoked" onChange={(e) => setAction(e.target.value.toLowerCase())} />
          </Field>
          <Field label="Project ID">
            <input className={cn(inputClass, "font-mono")} value={projectId} placeholder="Any project" onChange={(e) => setProjectId(e.target.value)} />
          </Field>
          <button type="submit" disabled={!valid} className="h-11 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">Apply</button>
        </form>
      )}
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : list.items.length === 0 ? (
        <Empty>{filtered ? "Nothing matches those filters." : "Nothing has happened here yet."}</Empty>
      ) : (
        <ol className="divide-y divide-border rounded-2xl border border-border bg-card">
          {list.items.map((e) => {
            const Icon = iconFor(e);
            const ok = /success|ok|allowed|succeeded/i.test(e.outcome);
            return (
              <li key={e.id} className="flex gap-3 px-4 py-3">
                <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-foreground"><Icon className="size-4" /></span>
                <div className="min-w-0 flex-1">
                  <p className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="font-semibold">{sentence(e.action)}</span>
                    {ok ? <CheckCircle2 className="size-4 text-emerald-500" aria-label="Succeeded" /> : <span className="inline-flex items-center gap-1 text-xs font-semibold text-red-600"><XCircle className="size-3.5" />{e.outcome.toLowerCase()}</span>}
                  </p>
                  <p className="mt-0.5 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
                    <span className="inline-flex items-center gap-1">{e.actorKind === "SERVICE" ? <Bot className="size-3" /> : <User className="size-3" />}{e.actorKind.toLowerCase()} <span className="font-mono">{e.actorRef.slice(0, 8)}</span></span>
                    <span>·</span>
                    <span>{e.targetType.toLowerCase().replace(/_/g, " ")} <span className="font-mono">{e.targetId.slice(0, 8)}</span></span>
                  </p>
                </div>
                <div className="shrink-0 text-right">
                  <p className="text-xs text-muted-foreground">{friendlyTime(e.occurredAt)}</p>
                  <p className="mt-0.5 font-mono text-[10px] text-muted-foreground/70" title={e.requestId}>ref {e.requestId.slice(0, 8)}</p>
                </div>
              </li>
            );
          })}
        </ol>
      )}
      <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
    </div>
  );
}

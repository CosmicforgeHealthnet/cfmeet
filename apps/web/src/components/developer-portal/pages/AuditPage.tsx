"use client";

import { useState } from "react";
import { formatTimestamp } from "@/lib/developer-platform/presentation";
import type { ActivityEvent, Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { RequireWorkspace } from "../PortalShell";
import { Empty, ErrorNotice, Field, inputClass, LoadMore, Loading, Mono, NotPermitted, PageHeader, Section, StatusBadge, Table, Td, usePagedList } from "../ui";

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
      <PageHeader title="Audit log" description="Who did what in this developer workspace: members, operators and services. Newest first." />
      {can("ActivityRead") ? <ActivityList key={workspace.id} workspace={workspace} /> : <NotPermitted what="the audit log" />}
    </div>
  );
}

const ACTION_PATTERN = /^[a-z0-9_.]{0,80}$/;
const ID_PATTERN = /^[0-9A-Za-z-]{0,64}$/;

function ActivityList({ workspace }: { workspace: Workspace }) {
  const [action, setAction] = useState("");
  const [projectId, setProjectId] = useState("");
  const [applied, setApplied] = useState<{ action: string; projectId: string }>({ action: "", projectId: "" });
  const list = usePagedList<ActivityEvent>(["developer-portal", workspace.id, "activity", applied.action, applied.projectId], {
    operation: "listActivity",
    path: { organizationId: workspace.id },
    query: { action: applied.action || undefined, projectId: applied.projectId || undefined },
  });
  const valid = ACTION_PATTERN.test(action.trim()) && ID_PATTERN.test(projectId.trim());
  return (
    <Section title="Activity">
      <div className="space-y-4">
        <form
          className="grid gap-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end"
          onSubmit={(e) => {
            e.preventDefault();
            if (valid) setApplied({ action: action.trim(), projectId: projectId.trim() });
          }}
        >
          <Field label="Action (optional)" hint="For example credential.revoked">
            <input className={inputClass} value={action} onChange={(e) => setAction(e.target.value)} />
          </Field>
          <Field label="Project ID (optional)">
            <input className={inputClass} value={projectId} onChange={(e) => setProjectId(e.target.value)} />
          </Field>
          <button type="submit" disabled={!valid} className="h-9 rounded-lg border border-primary px-4 text-sm font-medium text-primary disabled:opacity-50">
            Filter
          </button>
        </form>
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <Empty>No activity.</Empty>
        ) : (
          <Table head={["When", "Actor", "Action", "Target", "Outcome", "Request ID"]}>
            {list.items.map((e) => (
              <tr key={e.id}>
                <Td className="whitespace-nowrap">{formatTimestamp(e.occurredAt)}</Td>
                <Td>
                  <span className="text-xs text-muted-foreground">{e.actorKind.toLowerCase()}</span>
                  <br />
                  <Mono>{e.actorRef}</Mono>
                </Td>
                <Td>
                  <Mono>{e.action}</Mono>
                </Td>
                <Td>
                  <span className="text-xs text-muted-foreground">{e.targetType.toLowerCase().replace(/_/g, " ")}</span>
                  <br />
                  <Mono>{e.targetId}</Mono>
                </Td>
                <Td>
                  <StatusBadge status={e.outcome} />
                </Td>
                <Td>
                  <Mono>{e.requestId}</Mono>
                </Td>
              </tr>
            ))}
          </Table>
        )}
        <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
      </div>
    </Section>
  );
}

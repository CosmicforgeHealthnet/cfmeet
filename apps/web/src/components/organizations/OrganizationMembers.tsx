"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { useWorkspace } from "@/context/WorkspaceContext";
import {
  useBusinessOrganizationMembers,
  useLeaveOrganization,
  useUpdateMembership,
} from "@/hooks/v2/useBusinessOrganizations";
import {
  MEMBERSHIP_STATUSES,
  ORG_CAPABILITIES,
  assignableRoles,
  canManageMember,
  memberRole,
  orgCan,
  shortId,
} from "@/lib/organizations/organization-core";
import { PERSONAL_WORKSPACE_ID } from "@/lib/workspace/workspace-core";
import type { BusinessOrganization, OrganizationMemberResponse } from "@/types/generated/meeting-service";
import { OrgError, Section, formatDateTime } from "./shared";

const PAGE_SIZE = 25;

/**
 * ORG-006: the member directory exactly as served by `GET /organizations/{id}/members`
 * (role, status, joined date). The API serves neither display names nor pagination yet
 * (backend gap): rows show a shortened account id, and the client pages the served array.
 */
export default function OrganizationMembers({ org }: { org: BusinessOrganization }) {
  const canRead = orgCan(org, ORG_CAPABILITIES.membersRead) || orgCan(org, ORG_CAPABILITIES.membersManage);
  const members = useBusinessOrganizationMembers(org.id, canRead);
  const [page, setPage] = useState(0);
  const list = members.data ?? [];
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  const current = Math.min(page, pages - 1);
  const visible = list.slice(current * PAGE_SIZE, current * PAGE_SIZE + PAGE_SIZE);

  return (
    <div className="space-y-4">
      {canRead ? (
        <Section
          title="Members"
          description="Names are not shown yet: the organization service only returns account ids for members."
        >
          {members.isLoading && <p role="status" className="text-sm">Loading members…</p>}
          {members.isError && <OrgError error={members.error} retry={() => void members.refetch()} />}
          {members.data && list.length === 0 && <p className="text-sm text-muted-foreground">No members.</p>}
          {visible.length > 0 && (
            <ul className="divide-y divide-border rounded-md border border-border">
              {visible.map((m) => (
                <MemberRow key={m.id} org={org} member={m} />
              ))}
            </ul>
          )}
          {pages > 1 && (
            <div className="flex items-center gap-2 text-sm">
              <Button size="sm" variant="outline" disabled={current === 0} onClick={() => setPage(current - 1)}>
                Previous
              </Button>
              <span>
                Page {current + 1} of {pages} · {list.length} members
              </span>
              <Button size="sm" variant="outline" disabled={current >= pages - 1} onClick={() => setPage(current + 1)}>
                Next
              </Button>
            </div>
          )}
        </Section>
      ) : (
        <p className="rounded-lg border border-border p-4 text-sm text-muted-foreground">
          Your role in this organization does not include viewing the member list.
        </p>
      )}
      {org.viewer && <LeaveOrganization org={org} />}
    </div>
  );
}

function MemberRow({ org, member }: { org: BusinessOrganization; member: OrganizationMemberResponse }) {
  const update = useUpdateMembership(org.id);
  const role = memberRole(member);
  const manageable = canManageMember(org, member);
  const roles = assignableRoles(org.viewer?.role, role);
  const [nextRole, setNextRole] = useState(role);
  const [nextStatus, setNextStatus] = useState<string>(member.status);
  const isSelf = org.viewer?.membershipId === member.id;
  const changed = nextRole !== role || nextStatus !== member.status;

  const save = () => {
    if (typeof member.version !== "number") return;
    update.mutate({
      membershipId: member.id,
      body: {
        expectedVersion: member.version,
        ...(nextRole !== role ? { role: nextRole } : {}),
        ...(nextStatus !== member.status ? { status: nextStatus } : {}),
      },
    });
  };

  return (
    <li className="flex flex-wrap items-center justify-between gap-3 p-3">
      <div className="min-w-0">
        <p className="font-mono text-xs" title={member.userId}>
          {isSelf ? "You" : `Account ${shortId(member.userId)}`}
        </p>
        <p className="text-sm text-muted-foreground">
          {role} · {member.status} · joined {formatDateTime(member.createdAt)}
        </p>
        {update.isError && <OrgError error={update.error} />}
      </div>
      {manageable && (
        <div className="flex flex-wrap items-center gap-2">
          <select
            aria-label={`Role of ${shortId(member.userId)}`}
            className="h-9 rounded-md border border-input bg-background px-2 text-sm"
            value={nextRole}
            onChange={(e) => setNextRole(e.target.value)}
          >
            {(roles.includes(role as never) ? roles : [role, ...roles]).map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
          <select
            aria-label={`Status of ${shortId(member.userId)}`}
            className="h-9 rounded-md border border-input bg-background px-2 text-sm"
            value={nextStatus}
            onChange={(e) => setNextStatus(e.target.value)}
          >
            {MEMBERSHIP_STATUSES.map((s) => (
              <option key={s} value={s}>
                {s === "REMOVED" ? "REMOVED (ends membership)" : s}
              </option>
            ))}
          </select>
          <Button size="sm" disabled={!changed || update.isPending} onClick={save}>
            {update.isPending ? "Saving…" : "Save"}
          </Button>
        </div>
      )}
    </li>
  );
}

function LeaveOrganization({ org }: { org: BusinessOrganization }) {
  const router = useRouter();
  const leave = useLeaveOrganization(org.id);
  const { workspaceId, selectWorkspace } = useWorkspace();
  const [confirming, setConfirming] = useState(false);

  const run = async () => {
    try {
      await leave.mutateAsync();
      if (workspaceId === org.id) selectWorkspace(PERSONAL_WORKSPACE_ID);
      router.push("/dashboard/organizations");
    } catch {
      /* LAST_OWNER etc. shown below */
    }
  };

  return (
    <Section title="Leave organization" description="You lose access to this organization's meetings, colleagues and settings.">
      {leave.isError && <OrgError error={leave.error} />}
      {confirming ? (
        <div className="flex flex-wrap gap-2">
          <Button variant="destructive" size="sm" disabled={leave.isPending} onClick={() => void run()}>
            {leave.isPending ? "Leaving…" : `Leave ${org.name}`}
          </Button>
          <Button variant="outline" size="sm" disabled={leave.isPending} onClick={() => setConfirming(false)}>
            Cancel
          </Button>
        </div>
      ) : (
        <Button variant="outline" size="sm" onClick={() => setConfirming(true)}>
          Leave…
        </Button>
      )}
    </Section>
  );
}

"use client";

import { NiceSelect } from "@/components/ui/nice-select";
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import apiV2 from "@/services/v2/api";
import type { OrganizationTeamPage } from "@/types/generated/meeting-service";
import type { MeetingSpaceResponse, MemberResponse, Role } from "@/types/v2";
import { useMembers, useAddMember, useUpdateMember, useRevokeMember } from "@/hooks/v2/useMembers";
import { grantableRoles } from "@/lib/meeting-scheduling";
import { storageStore } from "@/store";
import { useProfile } from "@/hooks/useUser";
import { ActionDialog, Field, RequestError } from "./ManagementUI";

function RolePicker({ roles, value, onChange }: { roles: Role[]; value: Role; onChange: (role: Role) => void }) {
  return <NiceSelect label="Role" value={value} onChange={onChange} options={roles.map(role => ({ value: role, label: role.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase()) }))} />;
}

export default function MembersPanel({ space }: { space: MeetingSpaceResponse }) {
  const query = useMembers(space.id);
  const add = useAddMember(space.id);
  const roles = space.status === "ACTIVE" ? grantableRoles(space.viewerRole) : [];
  const [email, setEmail] = useState("");
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<Role>("PARTICIPANT");
  const queryClient = useQueryClient();
  const orgId = (space as { organizationId?: string | null }).organizationId ?? null;
  const [teamId, setTeamId] = useState("");
  const [teamRole, setTeamRole] = useState<Role>("PARTICIPANT");
  const [teamNotice, setTeamNotice] = useState<string | null>(null);
  const teams = useQuery({
    queryKey: ["org-teams", orgId],
    queryFn: async () => (await apiV2.get<OrganizationTeamPage>(`/organizations/${encodeURIComponent(orgId!)}/teams`, { params: { limit: 100 } })).data.items,
    enabled: !!orgId && roles.length > 0,
    retry: false,
  });
  return <section className="space-y-4"><div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-base font-semibold text-foreground">Members</h2><p className="text-sm text-muted-foreground">People who belong to this meeting and every session of it.</p></div>
    {roles.length > 0 && <ActionDialog label="Add member" description="Add someone by email (matched to their verified CF Meet account) or by user ID. This doesn't send them a message." submit={async () => {
      if (!!email.trim() === !!userId.trim()) throw new Error("Enter exactly one email address or user ID.");
      await add.mutateAsync({ ...(email.trim() ? { email: email.trim() } : { userId: userId.trim() }), role }); setEmail(""); setUserId("");
    }}><Field label="Email" type="email" value={email} onChange={e => setEmail(e.target.value)} /><Field label="Or their user ID" value={userId} onChange={e => setUserId(e.target.value)} /><RolePicker roles={roles} value={role} onChange={setRole} /></ActionDialog>}
    {orgId && roles.length > 0 && (teams.data?.length ?? 0) > 0 && <ActionDialog label="Add a team" description="Everyone in the team becomes a member. People already here are skipped." submit={async () => {
      if (!teamId) throw new Error("Pick a team.");
      const r = (await apiV2.post<{ added: unknown[]; skipped: number }>(`/meeting-spaces/${encodeURIComponent(space.id)}/members/team`, { teamId, role: teamRole })).data;
      await queryClient.invalidateQueries({ queryKey: ["v2", "members"] });
      setTeamNotice(`Added ${r.added.length}${r.skipped ? `, ${r.skipped} already here` : ""}.`); setTeamId("");
    }}><NiceSelect value={teamId} onChange={setTeamId} placeholder="Choose a team" options={(teams.data ?? []).map(t => ({ value: t.id, label: t.name }))} /><RolePicker roles={roles} value={teamRole} onChange={setTeamRole} /></ActionDialog>}
    </div>
    {teamNotice && <p role="status" className="rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground">{teamNotice}</p>}
    {query.isLoading && <div aria-busy="true" className="h-20 animate-pulse rounded-2xl bg-muted" />}
    {query.isError && <RequestError error={query.error} retry={() => void query.refetch()} />}
    {query.data?.items.length === 0 && <p className="rounded-xl bg-muted/50 px-4 py-3 text-sm text-muted-foreground">No members yet besides you.</p>}
    {query.data?.items.map(member => <MemberRow key={`${member.id}:${member.version}`} spaceId={space.id} member={member} roles={roles} />)}
  </section>;
}

function MemberRow({ spaceId, member, roles }: { spaceId: string; member: MemberResponse; roles: Role[] }) {
  const update = useUpdateMember(spaceId), revoke = useRevokeMember(spaceId);
  const [role, setRole] = useState(member.role);
  const user = storageStore.getUser();
  const profile = useProfile();
  const self = (!!member.userId && member.userId === user?.id) || (!!member.email && member.email.toLowerCase() === profile.data?.email?.toLowerCase());
  const manageable = member.status === "ACTIVE" && roles.includes(member.role);
  const name = member.email || member.userId || member.id;
  return <article className="flex flex-wrap items-center gap-3 rounded-2xl border border-border bg-card p-4"><span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary">{name.slice(0, 2).toUpperCase()}</span><div className="min-w-0 flex-1"><p className="truncate font-medium text-foreground">{name}{self ? <span className="text-muted-foreground"> (you)</span> : null}</p><div className="mt-1 flex gap-1.5 text-xs"><span className="rounded-md bg-primary/10 px-2 py-0.5 font-medium text-primary">{member.role.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase())}</span>{member.status !== "ACTIVE" && <span className="rounded-md bg-muted px-2 py-0.5 text-muted-foreground">{member.status.toLowerCase()}</span>}</div></div><div className="flex gap-2">
    {manageable && !self && <ActionDialog label="Change role" description="Pick their new role. If someone else changed it meanwhile, you'll be asked to refresh and check again." submit={() => update.mutateAsync({ memberId: member.id, data: { role, version: member.version } })}><RolePicker roles={roles} value={role} onChange={setRole} /></ActionDialog>}
    {(manageable || (self && roles.length > 0 && member.status === "ACTIVE")) && <ActionDialog label="Remove" destructive description="Remove this person from the meeting. Anyone currently in a live session is handled separately." submit={() => revoke.mutateAsync(member.id)} />}
  </div></article>;
}

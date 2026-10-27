"use client";

import { useState } from "react";
import type { MeetingSpaceResponse, MemberResponse, Role } from "@/types/v2";
import { useMembers, useAddMember, useUpdateMember, useRevokeMember } from "@/hooks/v2/useMembers";
import { grantableRoles } from "@/lib/meeting-scheduling";
import { storageStore } from "@/store";
import { useProfile } from "@/hooks/useUser";
import { ActionDialog, Field, RequestError } from "./ManagementUI";

function RolePicker({ roles, value, onChange }: { roles: Role[]; value: Role; onChange: (role: Role) => void }) {
  return <label className="grid gap-2 text-sm font-medium">Role<select className="h-10 rounded-md border border-input bg-background px-3" value={value} onChange={event => onChange(event.target.value as Role)}>{roles.map(role => <option key={role} value={role}>{role.replaceAll("_", " ")}</option>)}</select></label>;
}

export default function MembersPanel({ space }: { space: MeetingSpaceResponse }) {
  const query = useMembers(space.id);
  const add = useAddMember(space.id);
  const roles = space.status === "ACTIVE" ? grantableRoles(space.viewerRole) : [];
  const [email, setEmail] = useState("");
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<Role>("PARTICIPANT");
  return <section className="space-y-4"><div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-lg font-semibold">Persistent members</h2><p className="text-sm text-muted-foreground">Membership applies across occurrences. The owner is managed separately through ownership transfer.</p></div>
    {roles.length > 0 && <ActionDialog label="Add member" description="Supply either a registered user ID or an email address. Email membership is matched to verified accounts; this does not send an invitation." submit={async () => {
      if (!!email.trim() === !!userId.trim()) throw new Error("Enter exactly one email address or user ID.");
      await add.mutateAsync({ ...(email.trim() ? { email: email.trim() } : { userId: userId.trim() }), role }); setEmail(""); setUserId("");
    }}><Field label="Email" type="email" value={email} onChange={e => setEmail(e.target.value)} /><Field label="User ID (alternative to email)" value={userId} onChange={e => setUserId(e.target.value)} /><RolePicker roles={roles} value={role} onChange={setRole} /></ActionDialog>}
    </div>
    {query.isLoading && <p role="status">Loading members…</p>}
    {query.isError && <RequestError error={query.error} retry={() => void query.refetch()} />}
    {query.data?.items.length === 0 && <p className="text-sm text-muted-foreground">No additional members.</p>}
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
  return <article className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-card p-4"><div><p className="font-medium break-all">{member.email || member.userId || member.id}{self ? " (you)" : ""}</p><p className="text-sm text-muted-foreground">{member.role.replaceAll("_", " ")} · {member.status}</p></div><div className="flex gap-2">
    {manageable && !self && <ActionDialog label="Change role" description="Save this membership role using the version you reviewed. A conflicting change requires a refresh and review." submit={() => update.mutateAsync({ memberId: member.id, data: { role, version: member.version } })}><RolePicker roles={roles} value={role} onChange={setRole} /></ActionDialog>}
    {(manageable || (self && roles.length > 0 && member.status === "ACTIVE")) && <ActionDialog label="Revoke membership" destructive description="Remove this persistent membership. Conference participation and admission are separate." submit={() => revoke.mutateAsync(member.id)} />}
  </div></article>;
}

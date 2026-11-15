"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, Pencil, Plus, UserMinus, Users } from "lucide-react";
import apiV2 from "@/services/v2/api";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { SidePage, SideRow, SideSection } from "@/components/ui/side-page";
import { ORG_CAPABILITIES, orgCan } from "@/lib/organizations/organization-core";
import { useBusinessOrganizationMembers } from "@/hooks/v2/useBusinessOrganizations";
import type {
  BusinessOrganization,
  OrganizationTeam,
  OrganizationTeamMember,
  OrganizationTeamMemberPage,
  OrganizationTeamPage,
} from "@/types/generated/meeting-service";
import { OrgError, Section } from "./shared";

const base = (orgId: string) => `/organizations/${encodeURIComponent(orgId)}/teams`;
const keys = {
  teams: (orgId: string) => ["org-teams", orgId] as const,
  members: (orgId: string, teamId: string) => ["org-teams", orgId, teamId, "members"] as const,
};
const ROLE_LABEL: Record<string, string> = { TEAM_MANAGER: "Team manager", TEAM_MEMBER: "Member" };
const field = "h-11 w-full rounded-xl border border-border bg-background px-3.5 text-sm outline-none focus:border-primary focus:ring-2 focus:ring-primary/20";
const primaryBtn = "inline-flex h-10 shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50";
const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (k: string) => COLORS[([...k].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
const initials = (n: string) => n.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

/** Organization → Teams: groups of members (e.g. "Design", "Support"). Managing needs organization.teams.manage. */
export default function OrganizationTeams({ org }: { org: BusinessOrganization }) {
  const canManage = orgCan(org, ORG_CAPABILITIES.teamsManage);
  const teams = useQuery({
    queryKey: keys.teams(org.id),
    queryFn: async () => (await apiV2.get<OrganizationTeamPage>(base(org.id), { params: { limit: 100 } })).data.items,
    retry: false,
  });
  const [creating, setCreating] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const active = (teams.data ?? []).filter((t) => t.status !== "ARCHIVED");
  const open = active.find((t) => t.id === openId) ?? null;

  return (
    <Section title="Teams" description="Group people in your organization, like Design or Support."
      actions={canManage ? <button type="button" className={primaryBtn} onClick={() => setCreating(true)}><Plus className="size-4" />New team</button> : undefined}>
      {teams.isLoading ? <div className="grid gap-3 sm:grid-cols-2">{[0, 1].map((i) => <div key={i} className="h-20 animate-pulse rounded-2xl bg-muted" />)}</div> : null}
      {teams.isError ? <OrgError error={teams.error} retry={() => void teams.refetch()} /> : null}
      {teams.data && active.length === 0 ? (
        <div className="flex flex-col items-center rounded-2xl bg-muted/40 px-6 py-10 text-center">
          <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Users className="size-6" /></span>
          <p className="mt-3 font-semibold">No teams yet</p>
          <p className="mt-1 max-w-xs text-sm text-muted-foreground">{canManage ? "Create a team to group people who work together." : "Teams your admins create show up here."}</p>
        </div>
      ) : null}
      {active.length > 0 ? (
        <ul className="grid gap-3 sm:grid-cols-2">
          {active.map((t) => (
            <li key={t.id}>
              <button type="button" onClick={() => setOpenId(t.id)} className="flex w-full items-center gap-3 rounded-2xl border border-border p-4 text-left transition-colors hover:bg-muted/50">
                <span className="flex size-11 shrink-0 items-center justify-center rounded-xl text-sm font-semibold text-white" style={{ backgroundColor: colorFor(t.id) }}>{initials(t.name)}</span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-semibold">{t.name}</span>
                  <span className="block truncate text-sm text-muted-foreground">{t.memberCount} {t.memberCount === 1 ? "member" : "members"}{t.description ? ` · ${t.description}` : ""}</span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      <TeamForm org={org} open={creating} onOpenChange={setCreating} />
      {open ? <TeamPage org={org} team={open} canManage={canManage} onClose={() => setOpenId(null)} /> : null}
    </Section>
  );
}

/** Create a team, or rename / describe one (team given). */
function TeamForm({ org, team, open, onOpenChange }: { org: BusinessOrganization; team?: OrganizationTeam; open: boolean; onOpenChange: (v: boolean) => void }) {
  const client = useQueryClient();
  const [name, setName] = useState(team?.name ?? "");
  const [description, setDescription] = useState(team?.description ?? "");
  const save = useMutation({
    mutationFn: async () => team
      ? (await apiV2.patch(`${base(org.id)}/${team.id}`, { name: name.trim(), description: description.trim() || null, expectedVersion: team.version })).data
      : (await apiV2.post(base(org.id), { name: name.trim(), description: description.trim() || null })).data,
    onSuccess: () => { void client.invalidateQueries({ queryKey: keys.teams(org.id) }); onOpenChange(false); if (!team) { setName(""); setDescription(""); } },
  });
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader><DialogTitle>{team ? "Edit team" : "New team"}</DialogTitle><DialogDescription>{team ? "Change the team's name or description." : "You can add people right after."}</DialogDescription></DialogHeader>
        <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); if (name.trim()) save.mutate(); }}>
          <label className="block space-y-1.5"><span className="text-sm font-medium">Name</span><input autoFocus className={field} value={name} maxLength={120} onChange={(e) => setName(e.target.value)} placeholder="e.g. Design" /></label>
          <label className="block space-y-1.5"><span className="text-sm font-medium">Description <span className="font-normal text-muted-foreground">(optional)</span></span><input className={field} value={description} maxLength={500} onChange={(e) => setDescription(e.target.value)} placeholder="What this team does" /></label>
          {save.isError ? <OrgError error={save.error} /> : null}
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={() => onOpenChange(false)} className="h-10 rounded-xl px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="submit" disabled={!name.trim() || save.isPending} className={primaryBtn}>{team ? "Save" : "Create team"}</button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** One team: its people and (for managers) adding, role changes, removal, editing and archiving. */
function TeamPage({ org, team, canManage, onClose }: { org: BusinessOrganization; team: OrganizationTeam; canManage: boolean; onClose: () => void }) {
  const client = useQueryClient();
  const members = useQuery({
    queryKey: keys.members(org.id, team.id),
    queryFn: async () => (await apiV2.get<OrganizationTeamMemberPage>(`${base(org.id)}/${team.id}/members`, { params: { limit: 200 } })).data.items,
    retry: false,
  });
  const orgMembers = useBusinessOrganizationMembers(org.id, canManage);
  const [adding, setAdding] = useState("");
  const [editing, setEditing] = useState(false);
  const [archiving, setArchiving] = useState(false);
  const refresh = () => { void client.invalidateQueries({ queryKey: keys.members(org.id, team.id) }); void client.invalidateQueries({ queryKey: keys.teams(org.id) }); };
  const add = useMutation({ mutationFn: async (membershipId: string) => (await apiV2.post(`${base(org.id)}/${team.id}/members`, { membershipId, role: "TEAM_MEMBER" })).data, onSuccess: () => { setAdding(""); refresh(); } });
  const setRole = useMutation({ mutationFn: async ({ m, role }: { m: OrganizationTeamMember; role: string }) => (await apiV2.patch(`${base(org.id)}/${team.id}/members/${m.id}`, { role, expectedVersion: m.version })).data, onSuccess: refresh });
  const remove = useMutation({ mutationFn: async (m: OrganizationTeamMember) => (await apiV2.delete(`${base(org.id)}/${team.id}/members/${m.id}`)).data, onSuccess: refresh });
  const archive = useMutation({ mutationFn: async () => (await apiV2.post(`${base(org.id)}/${team.id}/archive`, { expectedVersion: team.version })).data, onSuccess: () => { refresh(); onClose(); } });

  const current = (members.data ?? []).filter((m) => m.status === "ACTIVE" && !m.removedAt);
  const inTeam = new Set(current.map((m) => m.membershipId));
  const candidates = (orgMembers.data ?? []).filter((m) => m.status === "ACTIVE" && !inTeam.has(m.id));
  const error = add.error ?? setRole.error ?? remove.error ?? archive.error;

  return (
    <SidePage open onOpenChange={(v) => !v && onClose()} title={team.name} description="Team">
      <div className="flex flex-col items-center px-6 pb-5 pt-7 text-center">
        <span className="flex size-20 items-center justify-center rounded-3xl text-2xl font-semibold text-white" style={{ backgroundColor: colorFor(team.id) }}>{initials(team.name)}</span>
        <p className="mt-3 text-xl font-semibold">{team.name}</p>
        {team.description ? <p className="mt-0.5 text-sm text-muted-foreground">{team.description}</p> : null}
        <p className="mt-1 text-xs text-muted-foreground">{current.length} {current.length === 1 ? "member" : "members"}</p>
      </div>
      {error ? <div className="px-4"><OrgError error={error} /></div> : null}
      {canManage ? (
        <SideSection title="Add people">
          <div className="flex items-center gap-2 p-3">
            <NiceSelect className="min-w-0 flex-1" value={adding} onChange={setAdding} placeholder={candidates.length ? "Choose a member" : "Everyone is already in this team"}
              options={candidates.map((m) => ({ value: m.id, label: m.displayName || m.username || m.email || "Member" }))} />
            <button type="button" disabled={!adding || add.isPending} onClick={() => add.mutate(adding)} className={primaryBtn}>Add</button>
          </div>
        </SideSection>
      ) : null}
      <SideSection title="Members">
        {members.isLoading ? <div className="h-16 animate-pulse bg-muted" /> : null}
        {members.isError ? <div className="p-3"><OrgError error={members.error} retry={() => void members.refetch()} /></div> : null}
        {members.data && current.length === 0 ? <p className="p-4 text-sm text-muted-foreground">No one in this team yet.</p> : null}
        {current.map((m) => {
          const name = m.displayName || m.username || "Member";
          return (
            <div key={m.id} className="flex items-center gap-3 border-t border-border px-3 py-2.5 first:border-t-0">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white" style={{ backgroundColor: colorFor(m.userId) }}>{initials(name)}</span>
              <span className="min-w-0 flex-1 truncate text-[15px]">{name}</span>
              {canManage ? (
                <>
                  <NiceSelect className="w-32 shrink-0 sm:w-40" value={m.role} onChange={(role) => role !== m.role && setRole.mutate({ m, role })} options={Object.entries(ROLE_LABEL).map(([value, label]) => ({ value, label }))} />
                  <button type="button" aria-label={`Remove ${name} from the team`} title="Remove from team" disabled={remove.isPending} onClick={() => remove.mutate(m)} className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive"><UserMinus className="size-4" /></button>
                </>
              ) : <span className="shrink-0 text-xs text-muted-foreground">{ROLE_LABEL[m.role] ?? m.role}</span>}
            </div>
          );
        })}
      </SideSection>
      {canManage ? (
        <div className="pb-8">
          <SideSection>
            <SideRow icon={<Pencil />} label="Edit name and description" onClick={() => setEditing(true)} />
            <SideRow icon={<Archive />} label="Archive team" hint="Hides the team; members stay in the organization" danger onClick={() => setArchiving(true)} />
          </SideSection>
        </div>
      ) : null}
      <TeamForm org={org} team={team} open={editing} onOpenChange={setEditing} />
      <Dialog open={archiving} onOpenChange={setArchiving}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader><DialogTitle>Archive {team.name}?</DialogTitle><DialogDescription>The team is hidden. Its members stay in the organization.</DialogDescription></DialogHeader>
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => setArchiving(false)} className="h-10 rounded-xl px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" disabled={archive.isPending} onClick={() => archive.mutate()} className="h-10 rounded-xl bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-50">Archive</button>
          </div>
        </DialogContent>
      </Dialog>
    </SidePage>
  );
}

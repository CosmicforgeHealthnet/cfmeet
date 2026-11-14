"use client";

import { useState } from "react";
import { Building2, Check, ChevronRight, Copy, Pencil, UserPlus, Users } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { cn } from "@/lib/utils";
import { useProfile } from "@/hooks";
import {
  useMyOrganizations,
  useOrganizationMembers,
  useUpsertOrganizationMember,
} from "@/hooks/v2/useOrganizations";
import type {
  OrganizationMemberStatus,
  OrganizationRole,
} from "@/types/v2";
import { OrgMark } from "./shared";

const niceLabel = (v: string) => v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : "All";

const userIdPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

const ROLE_HINT: Record<string, string> = { OWNER: "Full control", ADMIN: "Manages members", MEMBER: "Uses the organization" };
const STATUS_HINT: Record<string, string> = { ACTIVE: "Can use the organization", SUSPENDED: "Paused, keeps their place", REMOVED: "No longer a member" };

/**
 * Organizations while the legacy membership directory is authoritative: your organizations,
 * and (for owners / admins) the member list with add / change. People are added by account ID,
 * so anyone can copy their own ID to give to an admin.
 */
export default function LegacyOrganizationsDirectory() {
  const organizations = useMyOrganizations();
  const profile = useProfile();
  const [selectedId, setSelectedId] = useState("");
  const [editing, setEditing] = useState<{ userId: string; role: OrganizationRole; status: OrganizationMemberStatus; existing: boolean } | null>(null);
  const [copied, setCopied] = useState(false);
  const list = organizations.data ?? [];
  const selected = list.find(item => item.organization.id === selectedId) ?? list[0];
  const canManage = selected?.role === "OWNER" || selected?.role === "ADMIN";
  const organizationId = selected?.organization.id ?? "";
  const members = useOrganizationMembers(organizationId, !!canManage);
  const myId = profile.data?.id ?? "";

  const copyId = () => {
    if (!myId) return;
    void navigator.clipboard?.writeText(myId).then(() => { setCopied(true); setTimeout(() => setCopied(false), 2000); });
  };

  const idCard = (
    <div className="flex flex-wrap items-center gap-3 rounded-2xl bg-muted/50 p-4">
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold">Your account ID</p>
        <p className="text-xs text-muted-foreground">Give this to an organization admin so they can add you.</p>
        <p className="mt-1 truncate font-mono text-xs text-muted-foreground">{myId || "…"}</p>
      </div>
      <button type="button" disabled={!myId} onClick={copyId} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-background px-4 text-sm font-semibold ring-1 ring-border hover:bg-muted disabled:opacity-50">
        {copied ? <Check className="size-4 text-primary" /> : <Copy className="size-4" />}{copied ? "Copied" : "Copy ID"}
      </button>
    </div>
  );

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Organizations</h1>
        <p className="mt-0.5 text-sm text-muted-foreground">Teams and companies you belong to.</p>
      </header>

      <div className="mt-6">
        {organizations.isLoading && <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{[0, 1].map((i) => <div key={i} className="h-24 animate-pulse rounded-2xl bg-muted" />)}</div>}
        {organizations.isError && <RequestError error={organizations.error} retry={() => void organizations.refetch()} />}

        {!organizations.isLoading && !organizations.isError && list.length === 0 && (
          <div className="mx-auto max-w-xl">
            <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-14 text-center">
              <span className="flex size-16 items-center justify-center rounded-2xl bg-violet-500/10 text-violet-600"><Building2 className="size-7" /></span>
              <p className="mt-4 text-lg font-semibold">You&apos;re not in an organization yet</p>
              <p className="mt-1 max-w-sm text-sm text-muted-foreground">When your company or team adds you, it shows up here with its meetings and people.</p>
            </div>
            <div className="mt-4">{idCard}</div>
          </div>
        )}

        {list.length > 0 && selected && (
          <div className="grid items-start gap-6 lg:grid-cols-[20rem_minmax(0,1fr)]">
            <div className="space-y-4">
              <ul className="space-y-2">
                {list.map((item) => {
                  const active = item.organization.id === organizationId;
                  return (
                    <li key={item.organization.id}>
                      <button type="button" onClick={() => setSelectedId(item.organization.id)} aria-current={active ? "true" : undefined}
                        className={cn("flex w-full items-center gap-3 rounded-2xl border p-3 text-left transition-colors", active ? "border-primary/40 bg-primary/5" : "border-border bg-card hover:bg-muted/50")}>
                        <OrgMark name={item.organization.name} size="size-11" />
                        <span className="min-w-0 flex-1">
                          <span className="block truncate font-semibold">{item.organization.name}</span>
                          <span className="mt-0.5 flex items-center gap-1.5 text-xs">
                            <span className="rounded-full bg-muted px-2 py-0.5 font-medium text-muted-foreground">{niceLabel(item.role)}</span>
                            {item.organization.status !== "ACTIVE" && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 font-medium text-amber-700 dark:text-amber-300">{niceLabel(item.organization.status)}</span>}
                          </span>
                        </span>
                        <ChevronRight className={cn("size-4 shrink-0 text-muted-foreground", active && "text-primary")} />
                      </button>
                    </li>
                  );
                })}
              </ul>
              {idCard}
            </div>

            <section className="rounded-2xl border border-border bg-card">
              <header className="flex flex-wrap items-center gap-4 border-b border-border p-5">
                <OrgMark name={selected.organization.name} size="size-14" className="text-lg" />
                <div className="min-w-0 flex-1">
                  <h2 className="truncate text-xl font-bold tracking-tight">{selected.organization.name}</h2>
                  <p className="mt-1 flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
                    <span className="rounded-full bg-primary/10 px-2.5 py-0.5 text-xs font-semibold text-primary">You&apos;re {niceLabel(selected.role).toLowerCase()}</span>
                    {members.data ? <span>{members.data.filter((m) => m.status === "ACTIVE").length} active members</span> : null}
                  </p>
                </div>
                {canManage && <button type="button" onClick={() => setEditing({ userId: "", role: "MEMBER", status: "ACTIVE", existing: false })} className="inline-flex h-10 items-center gap-2 rounded-full bg-primary pl-3.5 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><UserPlus className="size-4" />Add member</button>}
              </header>

              <div className="p-5">
                {!canManage ? (
                  <div className="flex items-start gap-3 rounded-xl bg-muted/50 p-4 text-sm text-muted-foreground">
                    <Users className="mt-0.5 size-5 shrink-0" />
                    <span>You&apos;re a member here. Only owners and admins can see and manage the member list.</span>
                  </div>
                ) : (
                  <>
                    <p className="mb-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Members</p>
                    {members.isLoading && <div className="space-y-2">{[0, 1, 2].map((i) => <div key={i} className="h-14 animate-pulse rounded-xl bg-muted" />)}</div>}
                    {members.isError && <RequestError error={members.error} retry={() => void members.refetch()} />}
                    {members.data?.length === 0 && <p className="py-8 text-center text-sm text-muted-foreground">No members yet. Add someone with their account ID.</p>}
                    <ul className="divide-y divide-border">
                      {members.data?.map((member) => {
                        const self = member.userId === myId;
                        const locked = selected.role === "ADMIN" && member.role !== "MEMBER";
                        return (
                          <li key={member.id} className="flex items-center gap-3 py-3">
                            <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-semibold text-muted-foreground">{member.userId.slice(0, 2).toUpperCase()}</span>
                            <span className="min-w-0 flex-1">
                              <span className="block truncate text-sm font-semibold">{self ? "You" : `Account ${member.userId.slice(0, 8)}`}</span>
                              <span className="mt-0.5 flex items-center gap-1.5 text-xs">
                                <span className="text-muted-foreground">{niceLabel(member.role)}</span>
                                {member.status !== "ACTIVE" && <span className={cn("rounded-full px-2 py-0.5 font-medium", member.status === "REMOVED" ? "bg-muted text-muted-foreground" : "bg-amber-500/15 text-amber-700 dark:text-amber-300")}>{niceLabel(member.status)}</span>}
                              </span>
                            </span>
                            {!self && !locked && (
                              <button type="button" aria-label="Change membership" title="Change role or status" onClick={() => setEditing({ userId: member.userId, role: member.role, status: member.status, existing: true })} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><Pencil className="size-4" /></button>
                            )}
                            {locked && <span className="text-xs text-muted-foreground">Owner only</span>}
                          </li>
                        );
                      })}
                    </ul>
                  </>
                )}
              </div>
            </section>
          </div>
        )}
      </div>

      {editing && selected && (
        <MemberDialog
          key={editing.userId || "new"}
          organizationId={organizationId}
          viewerRole={selected.role}
          myId={myId}
          initial={editing}
          knownIds={new Set(members.data?.map((m) => m.userId) ?? [])}
          onClose={() => setEditing(null)}
        />
      )}
    </div>
  );
}

function MemberDialog({ organizationId, viewerRole, myId, initial, knownIds, onClose }: {
  organizationId: string;
  viewerRole: OrganizationRole;
  myId: string;
  initial: { userId: string; role: OrganizationRole; status: OrganizationMemberStatus; existing: boolean };
  knownIds: Set<string>;
  onClose: () => void;
}) {
  const upsert = useUpsertOrganizationMember(organizationId);
  const [userId, setUserId] = useState(initial.userId);
  const [role, setRole] = useState<OrganizationRole>(initial.role);
  const [status, setStatus] = useState<OrganizationMemberStatus>(initial.status);
  const [problem, setProblem] = useState("");
  const target = userId.trim();
  const known = knownIds.has(target);

  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    setProblem("");
    if (!userIdPattern.test(target)) return setProblem("That doesn't look like an account ID. Ask them to copy it from their Organizations page.");
    if (target === myId) return setProblem("You can't change your own membership.");
    if (viewerRole === "ADMIN" && role !== "MEMBER") return setProblem("Admins can only add or change members.");
    if (status === "REMOVED" && !known) return setProblem("This person isn't a member, so there's nothing to remove.");
    try {
      await upsert.mutateAsync({ userId: target, data: { role, status } });
      onClose();
    } catch {
      /* shown below */
    }
  };

  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader className="text-left">
          <DialogTitle>{initial.existing ? "Change membership" : "Add member"}</DialogTitle>
          <DialogDescription>{initial.existing ? `Account ${initial.userId.slice(0, 8)}` : "Paste the account ID they copied from their Organizations page."}</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={(e) => void save(e)}>
          {!initial.existing && (
            <label className="grid gap-1.5 text-sm">
              <span className="font-medium">Account ID</span>
              <input autoFocus value={userId} onChange={(e) => setUserId(e.target.value)} placeholder="e.g. 3f2a9c1e-…" autoComplete="off" required
                className="h-11 rounded-xl border border-border bg-background px-3.5 font-mono text-sm outline-none focus:border-primary focus:ring-1 focus:ring-primary" />
            </label>
          )}
          <NiceSelect label="Role" value={role} onChange={(v) => setRole(v as OrganizationRole)} options={(viewerRole === "OWNER" ? ["OWNER", "ADMIN", "MEMBER"] : ["MEMBER"]).map((v) => ({ value: v, label: niceLabel(v), hint: ROLE_HINT[v] }))} />
          {initial.existing && (
            <NiceSelect label="Status" value={status} onChange={(v) => setStatus(v as OrganizationMemberStatus)} options={(["ACTIVE", "SUSPENDED", "REMOVED"] as const).map((v) => ({ value: v, label: niceLabel(v), hint: STATUS_HINT[v] }))} />
          )}
          {problem && <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{problem}</p>}
          {upsert.isError && <RequestError error={upsert.error} />}
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={onClose} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="submit" disabled={upsert.isPending} className={cn("h-10 rounded-full px-5 text-sm font-semibold text-white disabled:opacity-50", status === "REMOVED" ? "bg-destructive hover:bg-destructive/90" : "bg-primary hover:bg-primary/90")}>
              {upsert.isPending ? "Saving…" : status === "REMOVED" ? "Remove member" : initial.existing ? "Save" : "Add"}
            </button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

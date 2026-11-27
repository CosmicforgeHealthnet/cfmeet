"use client";

import { NiceSelect } from "@/components/ui/nice-select";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Crown, LogOut, Mail, MoreHorizontal, UserMinus, UserPlus, Users } from "lucide-react";
import { Popover } from "radix-ui";
import { cn } from "@/lib/utils";
import { storageStore } from "@/store";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { actionsFor, can as canDo, memberActions, ROLE_DESCRIPTIONS } from "@/lib/developer-platform/permissions";
import { formatTimestamp, invitationToken, type OneTimeSecret } from "@/lib/developer-platform/presentation";
import { ROLES, type Invitation, type InvitationWithToken, type Member, type Role, type Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace, WORKSPACES_QUERY_KEY } from "../DeveloperPortalProvider";
import { RequireWorkspace } from "../PortalShell";
import {
  ConfirmButton,
  Empty,
  ErrorNotice,
  Field,
  inputClass,
  LoadMore,
  Loading,
  Mono,
  NotPermitted,
  PageHeader,
  SecretOnce,
  Section,
  StatusBadge,
  Table,
  Td,
  friendlyTime,
  useConsoleMutation,
  usePagedList,
} from "../ui";


const nice = (v: string) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : v);
export default function MembersPage() {
  return (
    <RequireWorkspace>
      <Members />
    </RequireWorkspace>
  );
}

function Members() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  return (
    <div className="space-y-6">
      <PageHeader
        title="Team"
        description="Who can build in this workspace, and what each person can do. These roles only apply here."
      />
      {can("MembersRead") ? <MemberList key={workspace.id} workspace={workspace} /> : <NotPermitted what="seeing the team" />}
      <InvitationsSection key={`inv-${workspace.id}`} workspace={workspace} />
      <RoleTable />
    </div>
  );
}

function MemberList({ workspace }: { workspace: Workspace }) {
  const { client } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "members"];
  const list = usePagedList<Member>(key, { operation: "listMembers", path: { organizationId: workspace.id } });
  const viewer = storageStore.getUser()?.id ?? null;
  const active = list.items.filter((m) => m.status === "ACTIVE");
  const owners = active.filter((m) => m.role === "OWNER").length;

  const changeRole = useConsoleMutation(
    (v: { member: Member; role: Role }) =>
      client.call<Member>({
        operation: "changeMemberRole",
        path: { organizationId: workspace.id, memberId: v.member.id },
        body: { role: v.role, expectedVersion: v.member.version },
      }),
    [key],
  );
  const remove = useConsoleMutation(
    (member: Member) => client.call<void>({ operation: "removeMember", path: { organizationId: workspace.id, memberId: member.id } }),
    [key, WORKSPACES_QUERY_KEY],
  );
  const transfer = useConsoleMutation(
    (member: Member) => client.call<unknown>({ operation: "transferOwnership", path: { organizationId: workspace.id }, body: { memberId: member.id } }),
    [key, WORKSPACES_QUERY_KEY],
  );

  const menu = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted";
  const [asking, setAsking] = useState<{ title: string; body: string; label: string; danger?: boolean; run: () => void } | null>(null);
  return (
    <section className="space-y-3">
      <h2 className="text-lg font-semibold">Members{active.length ? <span className="ml-1.5 text-muted-foreground">{active.length}</span> : null}</h2>
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : active.length === 0 ? (
        <Empty>No members yet.</Empty>
      ) : (
        <>
          <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
            {active.map((m) => {
              const actions = memberActions(workspace, m, viewer, owners);
              const you = viewer === m.userId;
              const hasMenu = actions.transferTo || actions.remove || actions.leave;
              return (
                <li key={m.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
                  <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-full text-xs font-semibold", m.role === "OWNER" ? "bg-amber-500/15 text-amber-700" : "bg-muted text-muted-foreground")}>{m.role === "OWNER" ? <Crown className="size-4" /> : m.userId.slice(0, 2).toUpperCase()}</span>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-semibold">{you ? "You" : `Account ${m.userId.slice(0, 8)}`}</p>
                    <p className="text-xs text-muted-foreground">Joined {friendlyTime(m.createdAt)} · {m.joinedVia.toLowerCase() === "invitation" ? "by invitation" : m.joinedVia.toLowerCase() === "creator" ? "created the workspace" : m.joinedVia.toLowerCase()}</p>
                  </div>
                  {actions.changeRole ? (
                    <NiceSelect className="w-40" value={m.role} disabled={changeRole.isPending} onChange={(v) => changeRole.mutate({ member: m, role: v as Role })} options={ROLES.map((r) => ({ value: r, label: nice(r) }))} />
                  ) : (
                    <span className="rounded-full bg-muted px-3 py-1 text-xs font-semibold text-muted-foreground">{nice(m.role)}</span>
                  )}
                  {hasMenu ? (
                    <Popover.Root>
                      <Popover.Trigger asChild><button type="button" aria-label="More" className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreHorizontal className="size-5" /></button></Popover.Trigger>
                      <Popover.Portal>
                        <Popover.Content align="end" sideOffset={4} className="z-50 w-52 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                          {actions.transferTo && <Popover.Close asChild><button type="button" className={menu} onClick={() => setAsking({ title: "Make this person the owner?", body: "They'll have full control of the workspace. You'll become an admin.", label: "Make owner", run: () => transfer.mutate(m) })}><Crown className="size-4 text-muted-foreground" />Make owner</button></Popover.Close>}
                          {actions.remove && <Popover.Close asChild><button type="button" className={cn(menu, "text-red-600")} onClick={() => setAsking({ title: "Remove this member?", body: "They lose access to this workspace straight away.", label: "Remove", danger: true, run: () => remove.mutate(m) })}><UserMinus className="size-4" />Remove</button></Popover.Close>}
                          {actions.leave && <Popover.Close asChild><button type="button" className={cn(menu, "text-red-600")} onClick={() => setAsking({ title: "Leave this workspace?", body: "You'll lose access until someone invites you again.", label: "Leave", danger: true, run: () => remove.mutate(m) })}><LogOut className="size-4" />Leave workspace</button></Popover.Close>}
                        </Popover.Content>
                      </Popover.Portal>
                    </Popover.Root>
                  ) : <span className="w-9" />}
                </li>
              );
            })}
          </ul>
          <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        </>
      )}
      <div className="space-y-2">
        <ErrorNotice error={changeRole.error} />
        <ErrorNotice error={remove.error} />
        <ErrorNotice error={transfer.error} />
      </div>
      <Dialog open={!!asking} onOpenChange={(v) => !v && setAsking(null)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader className="text-left"><DialogTitle className="text-lg">{asking?.title}</DialogTitle><DialogDescription>{asking?.body}</DialogDescription></DialogHeader>
          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={() => setAsking(null)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" onClick={() => { asking?.run(); setAsking(null); }} className={cn("h-10 rounded-full px-5 text-sm font-semibold text-white", asking?.danger ? "bg-red-600 hover:bg-red-700" : "bg-primary hover:bg-primary/90")}>{asking?.label}</button>
          </div>
        </DialogContent>
      </Dialog>
    </section>
  );
}

function InvitationsSection({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "invitations"];
  const list = usePagedList<Invitation>(key, { operation: "listInvitations", path: { organizationId: workspace.id } });
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("DEVELOPER");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const [token, setToken] = useState<OneTimeSecret | null>(null);
  const [inviting, setInviting] = useState(false);

  const create = useConsoleMutation(
    () =>
      client.call<InvitationWithToken>({
        operation: "createInvitation",
        path: { organizationId: workspace.id },
        body: { email: email.trim(), role },
        idempotencyKey,
      }),
    [key],
    (result) => {
      setToken(invitationToken(result));
      setInviting(false);
      setEmail("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const revoke = useConsoleMutation(
    (invitation: Invitation) =>
      client.call<Invitation>({ operation: "revokeInvitation", path: { organizationId: workspace.id, invitationId: invitation.id } }),
    [key],
  );

  const pending = list.items.filter((inv) => inv.status === "PENDING");
  const past = list.items.filter((inv) => inv.status !== "PENDING");
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-lg font-semibold">Invitations{pending.length ? <span className="ml-1.5 text-muted-foreground">{pending.length}</span> : null}</h2>
        {can("MembersManage") && <button type="button" onClick={() => setInviting(true)} className="inline-flex h-9 items-center gap-1.5 rounded-full bg-primary pl-3 pr-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><UserPlus className="size-4" />Invite someone</button>}
      </div>
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : list.items.length === 0 ? (
        <div className="flex items-center gap-3 rounded-2xl bg-muted/50 px-4 py-4 text-sm text-muted-foreground"><Users className="size-5 shrink-0" />No invitations yet. Invite a teammate to build with you.</div>
      ) : (
        <>
          <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
            {[...pending, ...past].map((inv) => (
              <li key={inv.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
                <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary"><Mail className="size-4" /></span>
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-semibold">{inv.email}</p>
                  <p className="text-xs text-muted-foreground">{nice(inv.role)} · {inv.status === "PENDING" ? `expires ${friendlyTime(inv.expiresAt)}` : nice(inv.status)}</p>
                </div>
                <StatusBadge status={inv.status} />
                {inv.status === "PENDING" && can("MembersManage") ? (
                  <ConfirmButton label="Cancel" message="Cancel this invitation? The link will stop working." destructive pending={revoke.isPending && revoke.variables?.id === inv.id} onConfirm={() => revoke.mutate(inv)} />
                ) : null}
              </li>
            ))}
          </ul>
          <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        </>
      )}
      <ErrorNotice error={revoke.error} />

      <Dialog open={inviting} onOpenChange={setInviting}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader className="text-left">
            <DialogTitle>Invite someone</DialogTitle>
            <DialogDescription>They join with their own CF Meet account, signed in with this email.</DialogDescription>
          </DialogHeader>
          <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); if (email.trim()) create.mutate(undefined); }}>
            <Field label="Email">
              <input autoFocus className={inputClass} type="email" required placeholder="teammate@company.com" value={email} onChange={(e) => setEmail(e.target.value)} />
            </Field>
            <NiceSelect label="Role" value={role} onChange={(v) => setRole(v as Role)} options={ROLES.map((r) => ({ value: r, label: nice(r), hint: ROLE_DESCRIPTIONS[r] }))} />
            <ErrorNotice error={create.error} />
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => setInviting(false)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
              <Button type="submit" className="rounded-full" loading={create.isPending} disabled={!email.trim()}>Send invite</Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>

      <Dialog open={!!token} onOpenChange={() => { /* closed only via Done */ }}>
        <DialogContent className="sm:max-w-lg [&>button]:hidden">
          <DialogHeader className="sr-only"><DialogTitle>Invitation code</DialogTitle><DialogDescription>Send this code to the person you invited.</DialogDescription></DialogHeader>
          {token && (
            <div className="space-y-4">
              <SecretOnce title="Invitation code" secret={token} onDone={() => setToken(null)}
                redactedHelp="This invitation was already made earlier, so its code isn't shown again. Cancel it and invite again if the code was lost." />
              {token.kind === "secret" && <p className="rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground"><span className="font-semibold">CF Meet doesn&apos;t email this yet.</span> Send it to them yourself. They paste it at Developers → Workspaces → Use an invitation.</p>}
            </div>
          )}
        </DialogContent>
      </Dialog>
    </section>
  );
}

const ACTIVE_STATE = "ACTIVE" as const;

function RoleTable() {
  return (
    <section className="space-y-3">
      <h2 className="text-lg font-semibold">What each role can do</h2>
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {ROLES.map((role) => (
          <div key={role} className="rounded-2xl border border-border bg-card p-4 text-sm">
            <p className="flex items-center gap-2 font-semibold">{role === "OWNER" && <Crown className="size-4 text-amber-600" />}{nice(role)}</p>
            <p className="mt-1 text-muted-foreground">{ROLE_DESCRIPTIONS[role]}</p>
            <p className="mt-3 text-xs font-medium text-muted-foreground">{actionsFor(role).filter((a) => canDo({ myRole: role, state: ACTIVE_STATE }, a)).length} things they can do</p>
          </div>
        ))}
      </div>
    </section>
  );
}

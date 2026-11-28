"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import {
  ArrowDown, ArrowLeft, ArrowUp, Ban, Check, Copy, Crown, Link2, Lock, LogOut, Megaphone, MessageSquare, Plus, Shield, ShieldOff, Trash2, Unlink, UserMinus, UserPlus, X,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { toast } from "@/components/ui/toast";
import QrCode from "@/components/communications/QrCode";
import { useGroupPhoto, listTime } from "@/components/messaging/ChatListRow";
import { MessagingApiError, messagingErrorMessage } from "@/lib/messaging/errors";
import type { MessagingEngine } from "@/lib/messaging/engine";
import { communitiesService as api } from "@/services/v2/communities.service";
import {
  canAddGroups, canDelete, canDemote, canInvite, canLeave, canManageSettings, canPromote, canRemove, canTransfer, cleanText, draftError, draftOf, groupLabel,
  inviteLinkUrl, isAdmin, moveGroup, outcomeText, patchBody, splitGroups, LINK_DURATIONS, MAX_GROUP_DESCRIPTION, MAX_GROUP_TITLE, MAX_INVITEES, PRIVACY_LABELS, ROLE_LABELS,
  type Community, type CommunityDraft, type CommunityGroup, type CommunityInviteLink, type CommunityMember, type GroupAccess,
} from "@/lib/communities";
import {
  useCommunitiesRefresh, useCommunity, useCommunityBans, useCommunityGroups, useCommunityInvitations, useCommunityLinks, useCommunityMembers, useCommunityRequests,
} from "@/hooks/v2/useCommunities";
import { CommunityForm } from "./CommunitiesContent";
import { CommunityAvatar, Empty, ErrorNote, Section, chip, field, useCommunityEngine, useEngineSnapshot, useNames } from "./bits";
import { cn } from "@/lib/utils";

type Tab = "groups" | "members" | "invite" | "requests" | "settings";

/** One community: its groups, members, invitations, join requests and settings. */
export default function CommunityPage({ communityId }: { communityId: string }) {
  const engine = useCommunityEngine();
  const community = useCommunity(communityId);
  const groups = useCommunityGroups(communityId, community.isSuccess);
  const [tab, setTab] = useState<Tab>("groups");
  if (community.isPending) return <p role="status" className="p-6 text-sm text-muted-foreground">Loading community…</p>;
  if (community.error || !community.data) {
    const gone = community.error instanceof MessagingApiError && community.error.code === "NOT_FOUND";
    return (
      <div className="mx-auto max-w-md space-y-3 p-6 text-center">
        <p className="font-semibold">{gone ? "This community isn't available" : "The community couldn't be loaded"}</p>
        <p className="text-sm text-muted-foreground">{gone ? "You may have left it, or it was deleted." : messagingErrorMessage(community.error)}</p>
        <Link href="/dashboard/communities" className={chip}>Back to Communities</Link>
      </div>
    );
  }
  const c = community.data;
  const decidesGroups = (groups.data ?? []).some((g) => g.canManage && !g.announcements);
  const tabs: [Tab, string][] = [["groups", "Groups"], ["members", "Members"]];
  if (canInvite(c.privacy, c.myRole)) tabs.push(["invite", "Invite"]);
  if (isAdmin(c.myRole) || decidesGroups) tabs.push(["requests", c.pendingRequestCount ? `Requests (${c.pendingRequestCount})` : "Requests"]);
  tabs.push(["settings", canManageSettings(c.myRole) ? "Settings" : "Info"]);
  return (
    <div className="mx-auto w-full max-w-3xl space-y-5 px-4 py-6" data-testid="community-page">
      <Link href="/dashboard/communities" className="inline-flex items-center gap-1 text-sm font-semibold text-muted-foreground hover:text-foreground"><ArrowLeft className="size-4" />Communities</Link>
      <header className="flex items-center gap-4">
        <CommunityAvatar engine={engine} conversationId={c.announcementsConversationId} pictureRef={c.pictureRef} name={c.name} size="lg" />
        <div className="min-w-0">
          <h1 className="truncate text-2xl font-bold tracking-tight">{c.name}</h1>
          <p className="text-sm text-muted-foreground">
            Community · {c.memberCount} {c.memberCount === 1 ? "member" : "members"} · {c.groupCount} {c.groupCount === 1 ? "group" : "groups"} · {PRIVACY_LABELS[c.privacy].label}
          </p>
          {c.description ? <p className="mt-1 whitespace-pre-line text-sm">{c.description}</p> : null}
        </div>
      </header>
      <nav className="no-scrollbar flex gap-2 overflow-x-auto" aria-label="Community sections">
        {tabs.map(([t, label]) => (
          <button key={t} type="button" onClick={() => setTab(t)} aria-current={tab === t ? "page" : undefined}
            className={cn("h-9 shrink-0 rounded-full px-4 text-sm font-semibold", tab === t ? "bg-primary text-primary-foreground" : "bg-muted text-muted-foreground hover:text-foreground")}>{label}</button>
        ))}
      </nav>
      {tab === "groups" ? <GroupsTab engine={engine} community={c} groups={groups} /> : null}
      {tab === "members" ? <MembersTab engine={engine} community={c} /> : null}
      {tab === "invite" ? <InviteTab engine={engine} community={c} /> : null}
      {tab === "requests" ? <RequestsTab engine={engine} community={c} /> : null}
      {tab === "settings" ? <SettingsTab engine={engine} community={c} /> : null}
    </div>
  );
}

// ------------------------------------------------------------------------------ groups

function GroupsTab({ engine, community: c, groups }: { engine: MessagingEngine; community: Community; groups: ReturnType<typeof useCommunityGroups> }) {
  const [showAll, setShowAll] = useState(false);
  const [adding, setAdding] = useState(false);
  if (groups.isPending) return <ul className="space-y-2">{[0, 1].map((i) => <li key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />)}</ul>;
  if (groups.error) return <ErrorNote error={messagingErrorMessage(groups.error)} />;
  const all = groups.data ?? [];
  const { joined, joinable } = splitGroups(all);
  return (
    <div className="space-y-5">
      {canAddGroups(c) ? <Button variant="outline" onClick={() => setAdding(true)}><Plus className="size-4" />Add group</Button> : null}
      <Section title="Groups you're in">
        {joined.length ? <ul className="space-y-2">{joined.map((g) => <GroupRow key={g.conversationId} engine={engine} community={c} group={g} all={all} />)}</ul> : <Empty>You haven&apos;t joined any group yet.</Empty>}
      </Section>
      <Section title="Other groups" action={joinable.length ? <button type="button" className="text-sm font-semibold text-primary" onClick={() => setShowAll((v) => !v)}>{showAll ? "Hide" : `View all groups (${joinable.length})`}</button> : null}>
        {!joinable.length ? <Empty>No other groups to join.</Empty>
          : showAll ? <ul className="space-y-2">{joinable.map((g) => <GroupRow key={g.conversationId} engine={engine} community={c} group={g} all={all} />)}</ul>
            : <p className="px-1 text-sm text-muted-foreground">{joinable.length} {joinable.length === 1 ? "group" : "groups"} you can join or ask to join.</p>}
      </Section>
      {adding ? <AddGroupDialog engine={engine} community={c} onClose={() => setAdding(false)} /> : null}
    </div>
  );
}

function GroupPhoto({ engine, group: g }: { engine: MessagingEngine; group: CommunityGroup }) {
  const photo = useGroupPhoto(engine, g.conversationId, g.joined ? g.avatarRef : null);
  return (
    <span className="flex size-11 shrink-0 items-center justify-center overflow-hidden rounded-xl bg-muted text-muted-foreground" aria-hidden>
      {/* eslint-disable-next-line @next/next/no-img-element -- a blob: URL of a group photo */}
      {photo ? <img src={photo} alt="" className="size-full object-cover" /> : g.announcements ? <Megaphone className="size-5" /> : <MessageSquare className="size-5" />}
    </span>
  );
}

function GroupRow({ engine, community: c, group: g, all }: { engine: MessagingEngine; community: Community; group: CommunityGroup; all: CommunityGroup[] }) {
  const refresh = useCommunitiesRefresh();
  const [busy, setBusy] = useState(false);
  const run = async (job: () => Promise<unknown>, done?: string) => {
    setBusy(true);
    try {
      await job();
      await refresh();
      if (done) toast(done);
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const label = groupLabel(g, engine.knownGroupName(g.conversationId));
  const admin = isAdmin(c.myRole);
  const join = () => run(async () => {
    const o = await api.joinGroup(c.communityId, g.conversationId);
    toast(outcomeText(o, "group"));
    if (o.outcome === "JOINED") void engine.refreshConversations().catch(() => {});
  });
  const move = (delta: -1 | 1) => {
    const order = moveGroup(all, g.conversationId, delta);
    if (order) void run(() => api.orderGroups(c.communityId, order));
  };
  return (
    <li className="space-y-2 rounded-2xl border border-border bg-card px-4 py-3" data-testid="community-group">
      <div className="flex items-center gap-3">
        <GroupPhoto engine={engine} group={g} />
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-1.5">
            <span className="truncate font-semibold">{label}</span>
            {g.access === "RESTRICTED" && !g.announcements ? <Lock className="size-3.5 shrink-0 text-muted-foreground" aria-label="Join by request" /> : null}
          </span>
          <span className="block truncate text-xs text-muted-foreground">
            {g.announcements ? "Only admins post · every member is here" : g.description ?? (g.access === "RESTRICTED" ? "Admins approve who joins" : "Any member can join")}
            {g.memberCount !== null ? ` · ${g.memberCount} ${g.memberCount === 1 ? "member" : "members"}` : ""}
          </span>
        </span>
        {g.joined ? (
          <Link href={`/dashboard/messages?conversation=${encodeURIComponent(g.conversationId)}`} className={chip}>Open</Link>
        ) : g.pendingRequest ? (
          <span className="text-xs font-semibold text-muted-foreground">Requested</span>
        ) : (
          <Button size="sm" loading={busy} onClick={() => void join()}>{g.access === "RESTRICTED" ? "Request" : "Join"}</Button>
        )}
      </div>
      {g.canManage && !g.announcements ? (
        <div className="flex flex-wrap items-center gap-2 pl-14">
          <button type="button" className={chip} disabled={busy} onClick={() => void run(() => api.updateGroup(c.communityId, g.conversationId, g.access === "OPEN" ? "RESTRICTED" : "OPEN"), "Group updated.")}>
            {g.access === "OPEN" ? <><Lock className="size-3.5" />Require approval</> : <><Check className="size-3.5" />Let members join</>}
          </button>
          {admin ? <>
            <button type="button" className={chip} disabled={busy} aria-label="Move up" onClick={() => move(-1)}><ArrowUp className="size-3.5" /></button>
            <button type="button" className={chip} disabled={busy} aria-label="Move down" onClick={() => move(1)}><ArrowDown className="size-3.5" /></button>
          </> : null}
          <button type="button" className={chip} disabled={busy} onClick={() => {
            if (window.confirm(`Remove "${label}" from ${c.name}? The group keeps its members and messages.`)) void run(() => api.unlinkGroup(c.communityId, g.conversationId), "Group removed from the community.");
          }}><Unlink className="size-3.5" />Remove from community</button>
        </div>
      ) : null}
    </li>
  );
}

function AddGroupDialog({ engine, community: c, onClose }: { engine: MessagingEngine; community: Community; onClose: () => void }) {
  const s = useEngineSnapshot(engine);
  const refresh = useCommunitiesRefresh();
  const [mode, setMode] = useState<"create" | "link">("create");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [access, setAccess] = useState<GroupAccess>("OPEN");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [key] = useState(() => `web-${crypto.randomUUID()}`);
  // Groups you administer that are not in a community yet (the server checks both again).
  const inCommunity = new Set(c.joinedConversationIds);
  const linkable = s.conversations.filter((x) => x.contextClass === "GROUP" && x.lifecycleState === "ACTIVE" && (x.myRole === "OWNER" || x.myRole === "ADMIN") && !inCommunity.has(x.id));
  const done = async (msg: string) => {
    await refresh();
    void engine.refreshConversations().catch(() => {});
    toast(msg);
    onClose();
  };
  const create = async () => {
    const t = cleanText(title, MAX_GROUP_TITLE);
    const d = cleanText(description, MAX_GROUP_DESCRIPTION, true);
    if (!t || d === null) { setError("Give the group a name (at most 120 characters) and a description of at most 512 characters."); return; }
    setBusy(true); setError(null);
    try {
      await api.createGroup(c.communityId, { title: t, ...(d ? { description: d } : {}), access }, key);
      await done("Group created.");
    } catch (e) {
      setError(messagingErrorMessage(e));
      setBusy(false);
    }
  };
  const link = async (conversationId: string) => {
    setBusy(true); setError(null);
    try {
      await api.linkGroup(c.communityId, conversationId, access);
      await done("Group added. New members only see messages sent after they join.");
    } catch (e) {
      setError(messagingErrorMessage(e));
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Add group</DialogTitle>
          <DialogDescription>Create a new group in {c.name}, or add a group you already administer.</DialogDescription>
        </DialogHeader>
        <div className="flex gap-2">
          {(["create", "link"] as const).map((m) => (
            <button key={m} type="button" onClick={() => setMode(m)} className={cn("h-9 flex-1 rounded-full text-sm font-semibold", mode === m ? "bg-primary text-primary-foreground" : "bg-muted text-muted-foreground")}>{m === "create" ? "New group" : "Existing group"}</button>
          ))}
        </div>
        <fieldset className="flex gap-2 text-sm">
          <legend className="mb-1 text-sm font-medium">Who can join</legend>
          {(["OPEN", "RESTRICTED"] as const).map((a) => (
            <label key={a} className="flex flex-1 cursor-pointer items-center gap-2 rounded-xl px-3 py-2 ring-1 ring-border has-[:checked]:ring-primary">
              <input type="radio" name="access" checked={access === a} onChange={() => setAccess(a)} />{a === "OPEN" ? "Any member" : "Admins approve"}
            </label>
          ))}
        </fieldset>
        {mode === "create" ? (
          <div className="space-y-3">
            <input className={field} value={title} maxLength={MAX_GROUP_TITLE} onChange={(e) => setTitle(e.target.value)} placeholder="Group name" aria-label="Group name" />
            <textarea className={field} rows={2} value={description} maxLength={MAX_GROUP_DESCRIPTION} onChange={(e) => setDescription(e.target.value)} placeholder="Description (optional)" aria-label="Group description" />
            <ErrorNote error={error} />
            <Button className="w-full" loading={busy} disabled={!title.trim()} onClick={() => void create()}>Create group</Button>
          </div>
        ) : (
          <div className="space-y-2">
            <p className="text-xs text-muted-foreground">People who join from the community see only messages sent after they join; earlier messages, media and the member list stay private.</p>
            <ErrorNote error={error} />
            {linkable.length ? (
              <ul className="max-h-72 space-y-1 overflow-y-auto">
                {linkable.map((x) => (
                  <li key={x.id}>
                    <button type="button" disabled={busy} onClick={() => void link(x.id)} className="flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left hover:bg-muted disabled:opacity-50">
                      <MessageSquare className="size-4 text-muted-foreground" /><span className="min-w-0 flex-1 truncate">{x.title}</span><Plus className="size-4" />
                    </button>
                  </li>
                ))}
              </ul>
            ) : <Empty>You don&apos;t administer a group that can be added.</Empty>}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

// ----------------------------------------------------------------------------- members

function MembersTab({ engine, community: c }: { engine: MessagingEngine; community: Community }) {
  const members = useCommunityMembers(c.communityId);
  const admin = isAdmin(c.myRole);
  const bans = useCommunityBans(c.communityId, admin);
  const list = useMemo(() => members.data?.pages.flatMap((p) => p.members) ?? [], [members.data]);
  const name = useNames(engine, [...list.map((m) => m.principalId), ...(bans.data ?? []).map((b) => b.principalId)]);
  const refresh = useCommunitiesRefresh();
  const me = useEngineSnapshot(engine).identity?.principalId ?? null;
  const [busy, setBusy] = useState<string | null>(null);
  const run = async (id: string, job: () => Promise<unknown>, done: string) => {
    setBusy(id);
    try {
      await job();
      await refresh();
      toast(done);
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };
  if (members.isPending) return <p role="status" className="text-sm text-muted-foreground">Loading members…</p>;
  if (members.error) return <ErrorNote error={messagingErrorMessage(members.error)} />;
  const actions = (m: CommunityMember) => {
    const self = m.principalId === me;
    const n = name(m.principalId);
    return (
      <div className="flex flex-wrap gap-1.5">
        {!self && canPromote(c.myRole, m.role) ? <button type="button" className={chip} disabled={busy === m.principalId} onClick={() => void run(m.principalId, () => api.setRole(c.communityId, m.principalId, "ADMIN"), `${n} is now a community admin.`)}><Shield className="size-3.5" />Make admin</button> : null}
        {canDemote(c.myRole, m.role, self) ? <button type="button" className={chip} disabled={busy === m.principalId} onClick={() => void run(m.principalId, () => api.setRole(c.communityId, m.principalId, "MEMBER"), self ? "You're no longer an admin." : `${n} is no longer an admin.`)}><ShieldOff className="size-3.5" />Dismiss as admin</button> : null}
        {!self && canTransfer(c.myRole) ? <button type="button" className={chip} disabled={busy === m.principalId} onClick={() => {
          if (window.confirm(`Make ${n} the owner of ${c.name}? You'll stay as a community admin.`)) void run(m.principalId, () => api.transfer(c.communityId, m.principalId), `${n} now owns the community.`);
        }}><Crown className="size-3.5" />Make owner</button> : null}
        {!self && canRemove(c.myRole, m.role) ? <>
          <button type="button" className={chip} disabled={busy === m.principalId} onClick={() => {
            if (window.confirm(`Remove ${n} from ${c.name} and its groups?`)) void run(m.principalId, () => api.removeMember(c.communityId, m.principalId), `${n} was removed.`);
          }}><UserMinus className="size-3.5" />Remove</button>
          <button type="button" className={cn(chip, "text-destructive")} disabled={busy === m.principalId} onClick={() => {
            if (window.confirm(`Ban ${n}? They're removed from the community and its groups and can't rejoin.`)) void run(m.principalId, () => api.ban(c.communityId, m.principalId), `${n} was banned.`);
          }}><Ban className="size-3.5" />Ban</button>
        </> : null}
      </div>
    );
  };
  return (
    <div className="space-y-5">
      <Section title={`${c.memberCount} ${c.memberCount === 1 ? "member" : "members"}`}>
        <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
          {list.map((m) => (
            <li key={m.principalId} className="space-y-2 px-4 py-3">
              <div className="flex items-center gap-3">
                <span className="flex size-9 items-center justify-center rounded-full bg-muted text-sm font-semibold">{name(m.principalId)[0]?.toLocaleUpperCase()}</span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium">{name(m.principalId)}</span>
                  <span className="text-xs text-muted-foreground">{ROLE_LABELS[m.role]} · joined {listTime(m.joinedAt)}</span>
                </span>
              </div>
              {actions(m)}
            </li>
          ))}
        </ul>
        {members.hasNextPage ? <button type="button" className={chip} disabled={members.isFetchingNextPage} onClick={() => void members.fetchNextPage()}>Show more</button> : null}
      </Section>
      {admin ? (
        <Section title="Banned">
          {bans.data?.length ? (
            <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
              {bans.data.map((b) => (
                <li key={b.principalId} className="flex items-center gap-3 px-4 py-3">
                  <span className="min-w-0 flex-1 truncate">{name(b.principalId)}<span className="block text-xs text-muted-foreground">Banned {listTime(b.bannedAt)}</span></span>
                  <button type="button" className={chip} disabled={busy === b.principalId} onClick={() => void run(b.principalId, () => api.unban(c.communityId, b.principalId), "Ban lifted.")}>Unban</button>
                </li>
              ))}
            </ul>
          ) : <Empty>Nobody is banned.</Empty>}
        </Section>
      ) : null}
    </div>
  );
}

// ------------------------------------------------------------------------------ invite

function InviteTab({ engine, community: c }: { engine: MessagingEngine; community: Community }) {
  const s = useEngineSnapshot(engine);
  const admin = isAdmin(c.myRole);
  const invitations = useCommunityInvitations(c.communityId, admin);
  const links = useCommunityLinks(c.communityId, true);
  const refresh = useCommunitiesRefresh();
  const me = s.identity?.principalId ?? null;
  const members = useCommunityMembers(c.communityId);
  const memberIds = useMemo(() => new Set(members.data?.pages.flatMap((p) => p.members.map((m) => m.principalId)) ?? []), [members.data]);
  // People you chat with (real principals with names); the server checks every invitation.
  const people = useMemo(() => {
    const ids = new Set<string>();
    for (const conv of s.conversations) for (const p of conv.members) if (p !== me) ids.add(p);
    return [...ids].filter((p) => !memberIds.has(p));
  }, [s.conversations, me, memberIds]);
  const name = useNames(engine, [...people, ...(invitations.data ?? []).map((i) => i.principalId)]);
  const [search, setSearch] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<CommunityInviteLink | null>(null);
  const [duration, setDuration] = useState(LINK_DURATIONS[2]!.seconds);
  const [maxUses, setMaxUses] = useState("");
  const needle = search.trim().toLocaleLowerCase();
  const shown = people.filter((p) => !needle || name(p).toLocaleLowerCase().includes(needle)).sort((a, b) => name(a).localeCompare(name(b)));
  const invite = async () => {
    setBusy(true);
    try {
      const r = await api.invite(c.communityId, chosen);
      setChosen([]);
      await refresh();
      toast(r.skipped.length ? `${r.invitations.length} invited; ${r.skipped.length} couldn't be invited.` : `${r.invitations.length} invited.`);
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const createLink = async () => {
    const uses = maxUses.trim() ? Number(maxUses) : undefined;
    if (uses !== undefined && (!Number.isInteger(uses) || uses < 1 || uses > 10_000)) { toast("Uses must be a whole number from 1 to 10,000.", "error"); return; }
    setBusy(true);
    try {
      setCreated(await api.createInviteLink(c.communityId, { expiresInSeconds: duration, ...(uses ? { maxUses: uses } : {}) }));
      await refresh();
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const url = created?.token && typeof window !== "undefined" ? inviteLinkUrl(window.location.origin, created.token) : null;
  return (
    <div className="space-y-6">
      <Section title="Invite people">
        <input className={field} type="search" value={search} onChange={(e) => setSearch(e.target.value)} placeholder="Search people you chat with" aria-label="Search people" />
        {shown.length ? (
          <ul className="max-h-64 space-y-1 overflow-y-auto rounded-2xl border border-border bg-card p-1">
            {shown.slice(0, 200).map((p) => {
              const on = chosen.includes(p);
              return (
                <li key={p}>
                  <button type="button" onClick={() => setChosen((x) => on ? x.filter((y) => y !== p) : x.length < MAX_INVITEES ? [...x, p] : x)} aria-pressed={on}
                    className={cn("flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left hover:bg-muted", on && "bg-primary/10")}>
                    <span className="min-w-0 flex-1 truncate">{name(p)}</span>{on ? <Check className="size-4 text-primary" /> : <UserPlus className="size-4 text-muted-foreground" />}
                  </button>
                </li>
              );
            })}
          </ul>
        ) : <Empty>{needle ? "Nobody matches." : "Start a chat with someone to invite them here, or share an invite link."}</Empty>}
        <Button disabled={!chosen.length} loading={busy} onClick={() => void invite()}>Invite {chosen.length || ""}</Button>
      </Section>

      <Section title="Invite links">
        <div className="flex flex-wrap items-center gap-2 rounded-2xl border border-border bg-card p-3">
          <select className={cn(field, "w-auto")} value={duration} onChange={(e) => setDuration(Number(e.target.value))} aria-label="Link expires after">
            {LINK_DURATIONS.map((d) => <option key={d.seconds} value={d.seconds}>Expires in {d.label}</option>)}
          </select>
          <input className={cn(field, "w-36")} inputMode="numeric" value={maxUses} onChange={(e) => setMaxUses(e.target.value)} placeholder="Unlimited uses" aria-label="Maximum uses" />
          <Button size="sm" loading={busy} onClick={() => void createLink()}><Link2 className="size-4" />Create link</Button>
        </div>
        {url ? (
          <div className="flex flex-col items-center gap-3 rounded-2xl border border-primary/30 bg-primary/5 p-4 text-center" data-testid="new-invite-link">
            <QrCode value={url} label={`QR code to join ${c.name}`} />
            <p className="break-all text-sm">{url}</p>
            <p className="text-xs text-muted-foreground">Copy it now: for safety the link is shown only once.{c.privacy === "APPROVAL" ? " Admins approve people who join with it." : ""}</p>
            <button type="button" className={chip} onClick={() => void navigator.clipboard.writeText(url).then(() => toast("Link copied."))}><Copy className="size-3.5" />Copy link</button>
          </div>
        ) : null}
        {links.data?.length ? (
          <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
            {links.data.map((l) => (
              <li key={l.inviteLinkId} className="flex items-center gap-3 px-4 py-3 text-sm">
                <span className="min-w-0 flex-1">
                  <span className="block font-medium">{l.status === "ACTIVE" ? `Expires ${listTime(l.expiresAt)}` : l.status === "REVOKED" ? "Revoked" : l.status === "EXHAUSTED" ? "Used up" : "Expired"}</span>
                  <span className="text-xs text-muted-foreground">{l.useCount}{l.maxUses ? ` of ${l.maxUses}` : ""} uses · created {listTime(l.createdAt)}</span>
                </span>
                {l.status === "ACTIVE" ? <button type="button" className={chip} onClick={() => void api.revokeInviteLink(c.communityId, l.inviteLinkId).then(() => refresh()).then(() => toast("Link revoked.")).catch((e) => toast(messagingErrorMessage(e), "error"))}>Revoke</button> : null}
              </li>
            ))}
          </ul>
        ) : links.isSuccess ? <Empty>No invite links yet.</Empty> : null}
      </Section>

      {admin ? (
        <Section title="Pending invitations">
          {invitations.data?.length ? (
            <ul className="divide-y divide-border rounded-2xl border border-border bg-card">
              {invitations.data.map((i) => (
                <li key={i.invitationId} className="flex items-center gap-3 px-4 py-3 text-sm">
                  <span className="min-w-0 flex-1 truncate">{name(i.principalId)}<span className="block text-xs text-muted-foreground">Expires {listTime(i.expiresAt)}</span></span>
                  <button type="button" className={chip} onClick={() => void api.revokeInvitation(c.communityId, i.invitationId).then(() => refresh()).then(() => toast("Invitation withdrawn.")).catch((e) => toast(messagingErrorMessage(e), "error"))}><X className="size-3.5" />Withdraw</button>
                </li>
              ))}
            </ul>
          ) : <Empty>No pending invitations.</Empty>}
        </Section>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------- requests

function RequestsTab({ engine, community: c }: { engine: MessagingEngine; community: Community }) {
  const requests = useCommunityRequests(c.communityId, true);
  const refresh = useCommunitiesRefresh();
  const name = useNames(engine, (requests.data ?? []).map((r) => r.principalId));
  const [busy, setBusy] = useState<string | null>(null);
  const decide = async (requestId: string, approve: boolean) => {
    setBusy(requestId);
    try {
      await (approve ? api.approve(c.communityId, requestId) : api.reject(c.communityId, requestId));
      await refresh();
      toast(approve ? "Request approved." : "Request declined.");
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };
  if (requests.isPending) return <p role="status" className="text-sm text-muted-foreground">Loading requests…</p>;
  if (requests.error) return <ErrorNote error={messagingErrorMessage(requests.error)} />;
  const list = requests.data ?? [];
  return list.length ? (
    <ul className="space-y-2">
      {list.map((r) => (
        <li key={r.requestId} className="space-y-2 rounded-2xl border border-border bg-card px-4 py-3">
          <div className="flex items-center gap-3">
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium">{name(r.principalId)}</span>
              <span className="text-xs text-muted-foreground">{r.conversationId ? `Wants to join ${r.groupTitle ?? "a group"}` : "Wants to join the community"} · {listTime(r.createdAt)}</span>
            </span>
            <button type="button" className={chip} disabled={busy === r.requestId} onClick={() => void decide(r.requestId, false)}><X className="size-4" />Decline</button>
            <Button size="sm" loading={busy === r.requestId} onClick={() => void decide(r.requestId, true)}><Check className="size-4" />Approve</Button>
          </div>
          {r.message ? <p className="whitespace-pre-line rounded-xl bg-muted px-3 py-2 text-sm">{r.message}</p> : null}
        </li>
      ))}
    </ul>
  ) : <Empty>No requests waiting.</Empty>;
}

// ---------------------------------------------------------------------------- settings

function SettingsTab({ engine, community: c }: { engine: MessagingEngine; community: Community }) {
  const router = useRouter();
  const refresh = useCommunitiesRefresh();
  const [draft, setDraft] = useState<CommunityDraft>(() => draftOf(c));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const manage = canManageSettings(c.myRole);
  const change = patchBody(c, draft);
  const changed = Object.keys(change).length > 1;
  const save = async () => {
    const problem = draftError(draft);
    if (problem) { setError(problem); return; }
    setBusy(true); setError(null);
    try {
      await api.update(c.communityId, change);
      await refresh();
      void engine.refreshConversations().catch(() => {});
      toast("Community updated.");
    } catch (e) {
      setError(e instanceof MessagingApiError && e.code === "VERSION_CONFLICT" ? "Someone else changed the community. Reload and try again." : messagingErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const picture = async (file: File | null) => {
    if (!c.announcementsConversationId) return;
    setBusy(true);
    try {
      await engine.setGroupPhoto(c.announcementsConversationId, file);
      await refresh();
      toast(file ? "Picture updated." : "Picture removed.");
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const leave = async () => {
    if (!window.confirm(`Leave ${c.name}? You'll also leave its groups.`)) return;
    setBusy(true);
    try {
      await api.leave(c.communityId);
      await refresh();
      void engine.refreshConversations().catch(() => {});
      router.replace("/dashboard/communities");
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
      setBusy(false);
    }
  };
  return (
    <div className="space-y-6">
      {c.rules ? <Section title="Community rules"><p className="whitespace-pre-line rounded-2xl border border-border bg-card px-4 py-3 text-sm">{c.rules}</p></Section> : null}
      {manage ? (
        <Section title="Community details">
          <div className="space-y-4 rounded-2xl border border-border bg-card p-4">
            <div className="flex flex-wrap items-center gap-2">
              <label className={`${chip} cursor-pointer`}>Change picture<input type="file" accept="image/png,image/jpeg,image/webp,image/gif" className="sr-only" disabled={busy} onChange={(e) => { const f = e.target.files?.[0]; if (f) void picture(f); }} /></label>
              {c.pictureRef ? <button type="button" className={chip} disabled={busy} onClick={() => void picture(null)}>Remove picture</button> : null}
            </div>
            <CommunityForm draft={draft} onChange={setDraft} />
            <ErrorNote error={error} />
            <Button disabled={!changed} loading={busy} onClick={() => void save()}>Save changes</Button>
          </div>
        </Section>
      ) : (
        <Section title="About">
          <p className="rounded-2xl border border-border bg-card px-4 py-3 text-sm">{PRIVACY_LABELS[c.privacy].detail} You&apos;re a {ROLE_LABELS[c.myRole].toLocaleLowerCase()}.</p>
        </Section>
      )}
      <Section title="Membership">
        <div className="flex flex-wrap gap-2">
          {canLeave(c.myRole) ? <button type="button" className={cn(chip, "text-destructive")} disabled={busy} onClick={() => void leave()}><LogOut className="size-4" />Leave community</button>
            : <p className="text-sm text-muted-foreground">As the owner, make another member the owner (Members) before you leave.</p>}
          {canDelete(c.myRole) ? <button type="button" className={cn(chip, "text-destructive")} disabled={busy} onClick={() => setConfirmDelete(true)}><Trash2 className="size-4" />Delete community</button> : null}
        </div>
      </Section>
      {confirmDelete ? <DeleteDialog community={c} onClose={() => setConfirmDelete(false)} onDeleted={() => { void refresh(); void engine.refreshConversations().catch(() => {}); router.replace("/dashboard/communities"); }} /> : null}
    </div>
  );
}

function DeleteDialog({ community: c, onClose, onDeleted }: { community: Community; onClose: () => void; onDeleted: () => void }) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>Delete {c.name}?</DialogTitle>
          <DialogDescription>The community and its Announcements group are deleted. Its other groups stay as ordinary groups with their members and messages.</DialogDescription>
        </DialogHeader>
        <input className={field} value={typed} onChange={(e) => setTyped(e.target.value)} placeholder="Type the community name" aria-label="Community name" />
        <ErrorNote error={error} />
        <Button variant="destructive" loading={busy} disabled={typed.trim() !== c.name} onClick={() => {
          setBusy(true);
          void api.remove(c.communityId).then(onDeleted).catch((e) => { setError(messagingErrorMessage(e)); setBusy(false); });
        }}>Delete community</Button>
      </DialogContent>
    </Dialog>
  );
}

"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { CalendarDays, Check, ChevronRight, EllipsisVertical, Link2, Plus, Settings, UsersRound, X } from "lucide-react";
import { DEPLOYED, communitiesMenu } from "@/lib/section-menus";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { toast } from "@/components/ui/toast";
import { listTime } from "@/components/messaging/ChatListRow";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { communitiesService as api } from "@/services/v2/communities.service";
import {
  communityActivity, createBody, draftError, outcomeText, sortCommunities, tokenFrom, PRIVACY_LABELS, MAX_DESCRIPTION, MAX_NAME, MAX_RULES,
  type Community, type CommunityDraft, type CommunityPrivacy, type ChatListMode,
} from "@/lib/communities";
import {
  useCommunities, useCommunitiesRefresh, useCommunityPreferences, useMyCommunityInvitations, useMyCommunityRequests, communityKeys,
} from "@/hooks/v2/useCommunities";
import { useQueryClient } from "@tanstack/react-query";
import type { MessagingEngine } from "@/lib/messaging/engine";
import { CommunityAvatar, Empty, ErrorNote, Section, chip, field, useCommunityEngine, useEngineSnapshot, useNames } from "./bits";

/** The Communities section: your communities with their latest announcement, invitations and requests. */
export default function CommunitiesContent() {
  const engine = useCommunityEngine();
  const s = useEngineSnapshot(engine);
  const communities = useCommunities();
  const invitations = useMyCommunityInvitations();
  const requests = useMyCommunityRequests();
  const [creating, setCreating] = useState(false);
  const [settings, setSettings] = useState(false);
  const [joining, setJoining] = useState(false);
  const [menu, setMenu] = useState(false);
  const list = sortCommunities(communities.data ?? [], s.conversations);
  const pendingRequests = (requests.data ?? []).filter((r) => r.state === "PENDING");

  return (
    <div className="mx-auto w-full max-w-3xl space-y-6 px-4 py-6" data-testid="communities-page">
      <header className="flex items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Communities</h1>
          <p className="text-sm text-muted-foreground">Bring related groups together, with announcements for everyone.</p>
        </div>
        <div className="relative flex items-center gap-1">
          <Button onClick={() => setCreating(true)} data-testid="new-community"><Plus className="size-4" />New community</Button>
          <button type="button" aria-label="Communities menu" aria-expanded={menu} onClick={() => setMenu((v) => !v)} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><EllipsisVertical className="size-5" /></button>
          {menu ? (
            <div role="menu" className="absolute right-0 top-11 z-20 w-52 rounded-2xl border border-border bg-card p-1 shadow-lg" onMouseLeave={() => setMenu(false)}>
              {/* The Communities section menu (ADR 0074): New community, Events, Settings. */}
              {communitiesMenu({ createCommunity: true }, DEPLOYED).map((item) => item.key === "new-community" ? (
                <div key={item.key}>
                  <MenuItem icon={Plus} label={item.label} onClick={() => { setMenu(false); setCreating(true); }} />
                  <MenuItem icon={Link2} label="Join with a link" onClick={() => { setMenu(false); setJoining(true); }} />
                </div>
              ) : item.key === "events" && item.href ? (
                <Link key={item.key} role="menuitem" href={item.href} onClick={() => setMenu(false)} className="flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-left text-sm hover:bg-muted"><CalendarDays className="size-4" />{item.label}</Link>
              ) : (
                // Settings here are the community chat-list preference (together / separate).
                <MenuItem key={item.key} icon={Settings} label={item.label} onClick={() => { setMenu(false); setSettings(true); }} />
              ))}
            </div>
          ) : null}
        </div>
      </header>

      {invitations.data?.length ? <Invitations engine={engine} invitations={invitations.data} /> : null}

      <Section title="Your communities">
        {communities.isPending ? (
          <ul className="space-y-2">{[0, 1, 2].map((i) => <li key={i} className="h-20 animate-pulse rounded-2xl bg-muted" />)}</ul>
        ) : communities.error ? (
          <ErrorNote error={messagingErrorMessage(communities.error)} />
        ) : list.length ? (
          <ul className="space-y-2">{list.map((c) => <CommunityRow key={c.communityId} engine={engine} community={c} />)}</ul>
        ) : (
          <Empty>You&apos;re not in any community yet. Create one, or open an invite link someone shared with you.</Empty>
        )}
      </Section>

      {pendingRequests.length ? (
        <Section title="Your requests">
          <ul className="space-y-2">
            {pendingRequests.map((r) => (
              <li key={r.requestId} className="flex items-center gap-3 rounded-2xl border border-border bg-card px-4 py-3">
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium">{r.conversationId ? `${r.groupTitle ?? "A group"} in ${r.communityName ?? "a community"}` : r.communityName ?? "A community"}</span>
                  <span className="text-xs text-muted-foreground">Waiting for an admin · {listTime(r.createdAt)}</span>
                </span>
                <CancelRequest requestId={r.requestId} />
              </li>
            ))}
          </ul>
        </Section>
      ) : null}

      {creating ? <NewCommunityDialog engine={engine} onClose={() => setCreating(false)} /> : null}
      {settings ? <PreferencesDialog onClose={() => setSettings(false)} /> : null}
      {joining ? <JoinLinkDialog onClose={() => setJoining(false)} /> : null}
    </div>
  );
}

function MenuItem({ icon: Icon, label, onClick }: { icon: typeof Plus; label: string; onClick: () => void }) {
  return (
    <button type="button" role="menuitem" onClick={onClick} className="flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left text-sm hover:bg-muted">
      <Icon className="size-4 text-muted-foreground" />{label}
    </button>
  );
}

function CommunityRow({ engine, community: c }: { engine: MessagingEngine; community: Community }) {
  const s = useEngineSnapshot(engine);
  const announcements = s.conversations.find((x) => x.id === c.announcementsConversationId) ?? null;
  const activity = communityActivity(c, s.conversations);
  const last = announcements?.lastMessage;
  const sender = useNames(engine, last ? [last.senderPrincipalId] : []);
  const preview = last
    ? last.lifecycle !== "ACTIVE" ? "Announcement removed"
      : `${sender(last.senderPrincipalId)}: ${last.previewText ?? (last.attachmentCount ? "Attachment" : "Announcement")}`
    : c.description ?? `${c.memberCount} ${c.memberCount === 1 ? "member" : "members"} · ${c.groupCount} ${c.groupCount === 1 ? "group" : "groups"}`;
  return (
    <li>
      <Link href={`/dashboard/communities/${c.communityId}`} className="flex items-center gap-3 rounded-2xl border border-border bg-card px-4 py-3 hover:bg-muted/50" data-testid="community-row">
        <CommunityAvatar engine={engine} conversationId={c.announcementsConversationId} pictureRef={c.pictureRef} name={c.name} />
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <span className="truncate font-semibold">{c.name}</span>
            {c.myRole !== "MEMBER" ? <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-semibold text-muted-foreground">{c.myRole === "OWNER" ? "Owner" : "Admin"}</span> : null}
          </span>
          <span className="block truncate text-sm text-muted-foreground">{preview}</span>
        </span>
        <span className="flex shrink-0 flex-col items-end gap-1">
          {activity.lastActivityAt ? <span className="text-xs text-muted-foreground">{listTime(activity.lastActivityAt)}</span> : null}
          {activity.unread ? <span className="min-w-5 rounded-full bg-primary px-1.5 text-center text-xs font-semibold text-primary-foreground" aria-label={`${activity.unread} unread`}>{activity.unread}</span> : null}
          {c.pendingRequestCount ? <span className="text-[11px] font-semibold text-primary">{c.pendingRequestCount} to review</span> : null}
        </span>
        <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
      </Link>
    </li>
  );
}

function Invitations({ engine, invitations }: { engine: MessagingEngine; invitations: NonNullable<ReturnType<typeof useMyCommunityInvitations>["data"]> }) {
  const refresh = useCommunitiesRefresh();
  const router = useRouter();
  const name = useNames(engine, invitations.map((i) => i.invitedByPrincipalId).filter((x): x is string => !!x));
  const [busy, setBusy] = useState<string | null>(null);
  const answer = async (id: string, accept: boolean) => {
    setBusy(id);
    try {
      if (accept) {
        const o = await api.acceptInvitation(id);
        toast(outcomeText(o, "community"));
        if (o.community) router.push(`/dashboard/communities/${o.community.communityId}`);
      } else await api.declineInvitation(id);
      await refresh();
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };
  return (
    <Section title="Invitations">
      <ul className="space-y-2">
        {invitations.map((i) => (
          <li key={i.invitationId} className="flex flex-wrap items-center gap-3 rounded-2xl border border-primary/30 bg-primary/5 px-4 py-3">
            <span className="min-w-0 flex-1">
              <span className="block truncate font-semibold">{i.community?.name ?? "A community"}</span>
              <span className="block truncate text-sm text-muted-foreground">
                {i.invitedByPrincipalId ? `${name(i.invitedByPrincipalId)} invited you` : "You're invited"}
                {i.community ? ` · ${i.community.memberCount} ${i.community.memberCount === 1 ? "member" : "members"}` : ""}
              </span>
            </span>
            <button type="button" className={chip} disabled={busy === i.invitationId} onClick={() => void answer(i.invitationId, false)}><X className="size-4" />Decline</button>
            <Button size="sm" loading={busy === i.invitationId} onClick={() => void answer(i.invitationId, true)}><Check className="size-4" />Join</Button>
          </li>
        ))}
      </ul>
    </Section>
  );
}

function CancelRequest({ requestId }: { requestId: string }) {
  const refresh = useCommunitiesRefresh();
  const [busy, setBusy] = useState(false);
  return (
    <button type="button" className={chip} disabled={busy} onClick={() => {
      setBusy(true);
      void api.cancelRequest(requestId).then(() => refresh()).catch((e) => toast(messagingErrorMessage(e), "error")).finally(() => setBusy(false));
    }}>Cancel</button>
  );
}

const PRIVACIES: CommunityPrivacy[] = ["APPROVAL", "OPEN", "INVITE_ONLY"];

export function CommunityForm({ draft, onChange }: { draft: CommunityDraft; onChange: (d: CommunityDraft) => void }) {
  return (
    <div className="space-y-3">
      <label className="block space-y-1 text-sm font-medium">
        <span>Community name</span>
        <input className={field} value={draft.name} maxLength={MAX_NAME} onChange={(e) => onChange({ ...draft, name: e.target.value })} placeholder="For example, Riverside Parents" data-testid="community-name" />
      </label>
      <label className="block space-y-1 text-sm font-medium">
        <span>Description <span className="font-normal text-muted-foreground">(optional)</span></span>
        <textarea className={field} rows={3} value={draft.description} maxLength={MAX_DESCRIPTION} onChange={(e) => onChange({ ...draft, description: e.target.value })} placeholder="What is this community about?" />
      </label>
      <label className="block space-y-1 text-sm font-medium">
        <span>Community rules <span className="font-normal text-muted-foreground">(optional)</span></span>
        <textarea className={field} rows={3} value={draft.rules} maxLength={MAX_RULES} onChange={(e) => onChange({ ...draft, rules: e.target.value })} placeholder="Be kind. Stay on topic." />
      </label>
      <fieldset className="space-y-1.5">
        <legend className="text-sm font-medium">Who can join</legend>
        {PRIVACIES.map((p) => (
          <label key={p} className="flex cursor-pointer items-start gap-3 rounded-xl px-3 py-2 ring-1 ring-border has-[:checked]:ring-primary">
            <input type="radio" name="privacy" className="mt-1" checked={draft.privacy === p} onChange={() => onChange({ ...draft, privacy: p })} />
            <span><span className="block text-sm font-semibold">{PRIVACY_LABELS[p].label}</span><span className="block text-xs text-muted-foreground">{PRIVACY_LABELS[p].detail}</span></span>
          </label>
        ))}
      </fieldset>
      <label className="flex items-center justify-between gap-3 rounded-xl px-3 py-2 ring-1 ring-border">
        <span><span className="block text-sm font-semibold">Members can add groups</span><span className="block text-xs text-muted-foreground">Otherwise only admins create or link groups.</span></span>
        <input type="checkbox" checked={draft.membersCanAddGroups} onChange={(e) => onChange({ ...draft, membersCanAddGroups: e.target.checked })} />
      </label>
    </div>
  );
}

/** New community onboarding: details, then an optional picture; opens the new community. */
function NewCommunityDialog({ engine, onClose }: { engine: MessagingEngine; onClose: () => void }) {
  const router = useRouter();
  const qc = useQueryClient();
  const [draft, setDraft] = useState<CommunityDraft>({ name: "", description: "", rules: "", privacy: "APPROVAL", membersCanAddGroups: false });
  const [created, setCreated] = useState<Community | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // One key per dialog: a retry after a lost response never creates a second community.
  const [key] = useState(() => `web-${crypto.randomUUID()}`);
  const problem = draftError(draft);
  const create = async () => {
    setBusy(true); setError(null);
    try {
      const c = await api.create(createBody(draft), key);
      await qc.invalidateQueries({ queryKey: communityKeys.all });
      void engine.refreshConversations().catch(() => {});
      setCreated(c);
    } catch (e) {
      setError(messagingErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const picture = async (file: File) => {
    if (!created?.announcementsConversationId) return;
    setBusy(true); setError(null);
    try {
      await engine.refreshConversations();
      await engine.setGroupPhoto(created.announcementsConversationId, file);
      router.push(`/dashboard/communities/${created.communityId}`);
    } catch (e) {
      setError(messagingErrorMessage(e));
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{created ? "Add a community picture" : "New community"}</DialogTitle>
          <DialogDescription>
            {created ? "Members see it on the community and its Announcements group." : "Communities bring related groups together. An Announcements group for every member is created with it."}
          </DialogDescription>
        </DialogHeader>
        {created ? (
          <div className="space-y-4 text-center">
            <span className="mx-auto flex size-20 items-center justify-center rounded-3xl bg-primary/10 text-primary"><UsersRound className="size-9" /></span>
            <label className={`${chip} cursor-pointer`}>
              Choose a picture
              <input type="file" accept="image/png,image/jpeg,image/webp,image/gif" className="sr-only" disabled={busy || !created.announcementsConversationId} onChange={(e) => { const f = e.target.files?.[0]; if (f) void picture(f); }} />
            </label>
            <ErrorNote error={error} />
            <Button variant="outline" className="w-full" disabled={busy} onClick={() => router.push(`/dashboard/communities/${created.communityId}`)}>{busy ? "Uploading…" : "Skip for now"}</Button>
          </div>
        ) : (
          <div className="space-y-4">
            <CommunityForm draft={draft} onChange={setDraft} />
            <ErrorNote error={error} />
            <Button className="w-full" loading={busy} disabled={!!problem} onClick={() => void create()} data-testid="create-community">Create community</Button>
            {problem && draft.name ? <p className="text-xs text-muted-foreground">{problem}</p> : null}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function PreferencesDialog({ onClose }: { onClose: () => void }) {
  const prefs = useCommunityPreferences();
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const mode: ChatListMode = prefs.data?.chatListMode ?? "TOGETHER";
  const set = async (m: ChatListMode) => {
    setBusy(true);
    try {
      const r = await api.setPreferences(m);
      qc.setQueryData(communityKeys.preferences, r);
    } catch (e) {
      toast(messagingErrorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>Community settings</DialogTitle>
          <DialogDescription>Where your community groups appear in Chats. Synced to all your devices.</DialogDescription>
        </DialogHeader>
        <fieldset className="space-y-2" disabled={busy || prefs.isPending}>
          {([["TOGETHER", "With your other chats", "Community groups are listed in All, and under Communities."], ["SEPARATE", "Separately", "Community groups are listed only under the Communities filter."]] as const).map(([m, label, detail]) => (
            <label key={m} className="flex cursor-pointer items-start gap-3 rounded-xl px-3 py-2 ring-1 ring-border has-[:checked]:ring-primary">
              <input type="radio" name="chat-list-mode" className="mt-1" checked={mode === m} onChange={() => void set(m)} />
              <span><span className="block text-sm font-semibold">{label}</span><span className="block text-xs text-muted-foreground">{detail}</span></span>
            </label>
          ))}
        </fieldset>
      </DialogContent>
    </Dialog>
  );
}

function JoinLinkDialog({ onClose }: { onClose: () => void }) {
  const router = useRouter();
  const [value, setValue] = useState("");
  const token = tokenFrom(value);
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>Join with a link</DialogTitle>
          <DialogDescription>Paste the community invite link you were sent.</DialogDescription>
        </DialogHeader>
        <input className={field} value={value} onChange={(e) => setValue(e.target.value)} placeholder="https://…/dashboard/communities/join?token=…" aria-label="Invite link" />
        <Button disabled={!token} onClick={() => router.push(`/dashboard/communities/join?token=${encodeURIComponent(token!)}`)}>Continue</Button>
      </DialogContent>
    </Dialog>
  );
}

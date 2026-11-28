"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ArrowLeft, Copy, Crown, Link2, Shield, Trash2, UserPlus } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import QrCode from "@/components/communications/QrCode";
import { inviteLink, VERIFICATION_BASES } from "@/lib/channels";
import { channelsService, type Admin, type Channel, type Invite } from "@/services/v2/channels.service";
import { socialService, type PersonView } from "@/services/v2/social.service";
import { errorText, whenText } from "@/components/updates/shared";
import {
  useChannelAdmins, useChannelAudit, useChannelInvites, useChannelModeration, useChannelsRefresh, useChannelV2,
  useChannelVerification, useScheduledPosts,
} from "@/hooks/v2/useChannels";
import ChannelForm from "./ChannelForm";
import ChannelAnalytics from "./ChannelAnalytics";
import ChannelPostCard from "./ChannelPostCard";
import { Sheet, Toggle, VerifiedBadge } from "./bits";

type Tab = "details" | "admins" | "invites" | "scheduled" | "analytics" | "verification" | "audit";

/** The owner's and admins' tools for one channel, each tab showing only what the caller may do. */
export default function ChannelAdminTools({ channelId, initialTab }: { channelId: string; initialTab?: Tab }) {
  const channel = useChannelV2(channelId);
  const c = channel.data;
  const [tab, setTab] = useState<Tab>(initialTab ?? "details");
  if (channel.isError || (c && c.myRole !== "OWNER" && c.myRole !== "ADMIN")) {
    return (
      <div className="mx-auto max-w-md px-6 py-16 text-center">
        <p className="text-lg font-semibold">Only the channel&apos;s owner and admins can open this</p>
        <Link href={`/dashboard/updates/channels/${channelId}`} className="mt-4 inline-block font-semibold text-primary">Back to the channel</Link>
      </div>
    );
  }
  const tabs: [Tab, string, boolean][] = c ? [
    ["details", "Details", c.permissions.edit],
    ["admins", "Admins", true],
    ["invites", "Invite links", c.permissions.edit],
    ["scheduled", "Scheduled", true],
    ["analytics", "Analytics", true],
    ["verification", "Verification & safety", true],
    ["audit", "Activity log", true],
  ] : [];
  return (
    <div className="min-h-dvh" data-testid="channel-admin-tools">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border bg-background px-2 py-2">
        <Link href={`/dashboard/updates/channels/${channelId}`} aria-label="Back" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        <p className="min-w-0 flex-1 truncate font-semibold">{c ? <>{c.name} {c.verified ? <VerifiedBadge /> : null}</> : "Channel"}</p>
      </header>
      <nav className="flex gap-1 overflow-x-auto border-b border-border px-2 py-2">
        {tabs.filter(([, , ok]) => ok).map(([t, label]) => (
          <button key={t} type="button" onClick={() => setTab(t)} className={cn("h-9 shrink-0 rounded-full px-4 text-sm font-semibold", tab === t ? "bg-primary text-primary-foreground" : "bg-muted")}>{label}</button>
        ))}
      </nav>
      {!c ? <div className="mx-auto mt-8 h-40 max-w-lg animate-pulse rounded-2xl bg-muted" /> : (
        <div className="mx-auto max-w-3xl">
          {tab === "details" ? <ChannelForm channel={c} /> : null}
          {tab === "admins" ? <Admins channel={c} /> : null}
          {tab === "invites" ? <Invites channel={c} /> : null}
          {tab === "scheduled" ? <Scheduled channel={c} /> : null}
          {tab === "analytics" ? <ChannelAnalytics channelId={c.channelId} /> : null}
          {tab === "verification" ? <Verification channel={c} /> : null}
          {tab === "audit" ? <Audit channel={c} /> : null}
        </div>
      )}
    </div>
  );
}

const PERMS: [keyof Pick<Admin, "canPost" | "canEdit" | "canManageAdmins">, string][] = [
  ["canPost", "Post, schedule and pin"], ["canEdit", "Edit the channel, invite links and every post"], ["canManageAdmins", "Add and remove admins"],
];

function Admins({ channel }: { channel: Channel }) {
  const router = useRouter();
  const admins = useChannelAdmins(channel.channelId, true);
  const refresh = useChannelsRefresh();
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<Admin | null>(null);
  const owner = channel.myRole === "OWNER";
  const mine = channel.permissions;
  const reload = () => { void admins.refetch(); void refresh(channel.channelId); };
  const remove = async (a: Admin) => {
    if (!window.confirm(`Remove ${a.name ?? "this admin"} as admin?`)) return;
    try { await channelsService.removeAdmin(channel.channelId, a.principalId); reload(); } catch (e) { toast(errorText(e), "error"); }
  };
  const transfer = async (a: Admin) => {
    if (!window.confirm(`Make ${a.name ?? "this admin"} the owner of “${channel.name}”? You'll stay on as an admin.`)) return;
    try { await channelsService.transfer(channel.channelId, a.principalId); toast("Ownership transferred"); await refresh(channel.channelId); router.refresh(); reload(); } catch (e) { toast(errorText(e), "error"); }
  };
  return (
    <section className="space-y-3 px-4 py-4">
      <p className="text-sm text-muted-foreground">Admins help run the channel. Followers are never listed, to anyone.</p>
      <div className="divide-y divide-border rounded-2xl border border-border">
        <div className="flex items-center gap-3 px-3 py-3"><Crown className="size-5 text-amber-500" /><span className="flex-1 text-sm font-medium">{admins.data?.ownerName ?? "Owner"}</span><span className="text-xs text-muted-foreground">Owner</span></div>
        {admins.data?.admins.map((a) => (
          <div key={a.principalId} className="flex items-center gap-3 px-3 py-3" data-testid="channel-admin-row">
            <Shield className="size-5 text-primary" />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium">{a.name}</span>
              <span className="block text-xs text-muted-foreground">{PERMS.filter(([k]) => a[k]).map(([, l]) => l.split(",")[0]).join(" · ")}</span>
            </span>
            {owner || (mine.manageAdmins && !a.canManageAdmins) ? <button type="button" onClick={() => setEditing(a)} className="rounded-full px-3 py-1 text-xs font-semibold hover:bg-muted">Edit</button> : null}
            {owner ? <button type="button" onClick={() => void transfer(a)} className="rounded-full px-3 py-1 text-xs font-semibold hover:bg-muted">Make owner</button> : null}
            {owner || (mine.manageAdmins && !a.canManageAdmins) ? <button type="button" aria-label="Remove admin" onClick={() => void remove(a)} className="flex size-8 items-center justify-center rounded-full text-destructive hover:bg-muted"><Trash2 className="size-4" /></button> : null}
          </div>
        ))}
      </div>
      {mine.manageAdmins ? <button type="button" onClick={() => setAdding(true)} className="flex h-11 items-center gap-2 rounded-xl bg-primary px-4 font-semibold text-primary-foreground"><UserPlus className="size-4" />Add admin</button> : null}
      {adding ? <AddAdmin channel={channel} existing={admins.data?.admins.map((a) => a.principalId) ?? []} onClose={() => setAdding(false)} onDone={reload} /> : null}
      {editing ? <EditAdmin channel={channel} admin={editing} onClose={() => setEditing(null)} onDone={reload} /> : null}
    </section>
  );
}

function PermissionToggles({ value, onChange, channel }: { value: Record<string, boolean>; onChange: (v: Record<string, boolean>) => void; channel: Channel }) {
  const owner = channel.myRole === "OWNER";
  const held = { canPost: channel.permissions.post, canEdit: channel.permissions.edit, canManageAdmins: owner };
  return (
    <div className="divide-y divide-border rounded-2xl border border-border px-3">
      {PERMS.map(([k, label]) => <Toggle key={k} label={label} checked={!!value[k]} disabled={!held[k]} onChange={(v) => onChange({ ...value, [k]: v })} />)}
    </div>
  );
}

function AddAdmin({ channel, existing, onClose, onDone }: { channel: Channel; existing: string[]; onClose: () => void; onDone: () => void }) {
  const [people, setPeople] = useState<PersonView[] | null>(null);
  const [who, setWho] = useState<string>("");
  const [q, setQ] = useState("");
  const [perms, setPerms] = useState<Record<string, boolean>>({ canPost: true, canEdit: false, canManageAdmins: false });
  const [busy, setBusy] = useState(false);
  useEffect(() => { socialService.eligibleContacts().then(setPeople, () => setPeople([])); }, []);
  const shown = useMemo(() => (people ?? []).filter((p) => !existing.includes(p.userId) && p.name.toLowerCase().includes(q.trim().toLowerCase())), [people, existing, q]);
  const save = async () => {
    setBusy(true);
    try {
      await channelsService.putAdmin(channel.channelId, who, { canPost: !!perms.canPost, canEdit: !!perms.canEdit, canManageAdmins: !!perms.canManageAdmins });
      toast("Admin added"); onDone(); onClose();
    } catch (e) { toast(errorText(e), "error"); } finally { setBusy(false); }
  };
  return (
    <Sheet title="Add admin" onClose={onClose}>
      <p className="mb-2 text-xs text-muted-foreground">You can make your contacts admins. They&apos;ll follow the channel.</p>
      <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search contacts" className="mb-2 h-10 w-full rounded-xl border border-border bg-background px-3 text-sm" />
      <div className="max-h-56 space-y-1 overflow-y-auto">
        {people === null ? <p className="py-4 text-center text-sm text-muted-foreground">Loading contacts…</p> : null}
        {people && shown.length === 0 ? <p className="py-4 text-center text-sm text-muted-foreground">No contacts to add.</p> : null}
        {shown.map((p) => (
          <label key={p.userId} className={cn("flex items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted", who === p.userId && "bg-primary/10")}>
            <input type="radio" name="admin" className="accent-primary" checked={who === p.userId} onChange={() => setWho(p.userId)} />
            <span className="text-sm">{p.name}</span>
          </label>
        ))}
      </div>
      <div className="mt-3"><PermissionToggles value={perms} onChange={setPerms} channel={channel} /></div>
      <button type="button" disabled={!who || busy || !(perms.canPost || perms.canEdit || perms.canManageAdmins)} onClick={() => void save()} className="mt-3 h-11 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-50">{busy ? "Adding…" : "Add admin"}</button>
    </Sheet>
  );
}

function EditAdmin({ channel, admin, onClose, onDone }: { channel: Channel; admin: Admin; onClose: () => void; onDone: () => void }) {
  const [perms, setPerms] = useState<Record<string, boolean>>({ canPost: admin.canPost, canEdit: admin.canEdit, canManageAdmins: admin.canManageAdmins });
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      await channelsService.putAdmin(channel.channelId, admin.principalId, { canPost: !!perms.canPost, canEdit: !!perms.canEdit, canManageAdmins: !!perms.canManageAdmins });
      onDone(); onClose();
    } catch (e) { toast(errorText(e), "error"); } finally { setBusy(false); }
  };
  return (
    <Sheet title={`${admin.name ?? "Admin"}'s permissions`} onClose={onClose}>
      <PermissionToggles value={perms} onChange={setPerms} channel={channel} />
      <button type="button" disabled={busy || !(perms.canPost || perms.canEdit || perms.canManageAdmins)} onClick={() => void save()} className="mt-3 h-11 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-50">{busy ? "Saving…" : "Save"}</button>
    </Sheet>
  );
}

function Invites({ channel }: { channel: Channel }) {
  const invites = useChannelInvites(channel.channelId, true);
  const [fresh, setFresh] = useState<Invite | null>(null);
  const [days, setDays] = useState(30);
  const [maxUses, setMaxUses] = useState("");
  const [busy, setBusy] = useState(false);
  const create = async () => {
    setBusy(true);
    try {
      const r = await channelsService.createInvite(channel.channelId, { expiresInSeconds: days * 86_400, maxUses: maxUses ? Number(maxUses) : undefined });
      setFresh(r); void invites.refetch();
    } catch (e) { toast(errorText(e), "error"); } finally { setBusy(false); }
  };
  const revoke = async (i: Invite) => {
    if (!window.confirm("Revoke this invite link? People can no longer join with it.")) return;
    try { await channelsService.revokeInvite(channel.channelId, i.inviteId); void invites.refetch(); if (fresh?.inviteId === i.inviteId) setFresh(null); } catch (e) { toast(errorText(e), "error"); }
  };
  const link = fresh?.token && typeof window !== "undefined" ? inviteLink(window.location.origin, fresh.token) : null;
  return (
    <section className="space-y-4 px-4 py-4" data-testid="channel-invites">
      <p className="text-sm text-muted-foreground">{channel.visibility === "PRIVATE" ? "This channel is private: people join only with an invite link." : "Anyone can follow this public channel; invite links make sharing easier."} A link is shown once — copy it or save its QR code now. You can revoke it any time.</p>
      <div className="flex flex-wrap items-end gap-3 rounded-2xl border border-border p-3">
        <label className="text-sm">Expires after
          <select value={days} onChange={(e) => setDays(Number(e.target.value))} className="mt-1 block h-10 rounded-xl border border-border bg-background px-2">
            {[1, 7, 30, 90, 365].map((d) => <option key={d} value={d}>{d === 1 ? "1 day" : `${d} days`}</option>)}
          </select>
        </label>
        <label className="text-sm">Uses (optional)
          <input type="number" min={1} max={100000} value={maxUses} onChange={(e) => setMaxUses(e.target.value)} className="mt-1 block h-10 w-28 rounded-xl border border-border bg-background px-2" placeholder="No limit" />
        </label>
        <button type="button" disabled={busy} onClick={() => void create()} className="flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground disabled:opacity-50"><Link2 className="size-4" />Create link</button>
      </div>
      {link ? (
        <div className="flex flex-col items-center gap-3 rounded-2xl border border-primary/40 bg-primary/5 p-4" data-testid="channel-invite-fresh">
          <QrCode value={link} size={192} label={`Invite QR code for ${channel.name}`} />
          <p className="w-full break-all rounded-xl bg-background px-3 py-2 text-xs">{link}</p>
          <button type="button" onClick={() => void navigator.clipboard.writeText(link).then(() => toast("Link copied"), () => toast("Couldn't copy", "error"))} className="flex h-10 items-center gap-2 rounded-full bg-muted px-4 text-sm font-semibold"><Copy className="size-4" />Copy link</button>
        </div>
      ) : null}
      <div className="divide-y divide-border rounded-2xl border border-border">
        {(invites.data ?? []).length === 0 ? <p className="px-3 py-4 text-sm text-muted-foreground">No invite links yet.</p> : null}
        {invites.data?.map((i) => (
          <div key={i.inviteId} className="flex items-center gap-3 px-3 py-3 text-sm">
            <span className="min-w-0 flex-1">
              <span className="block">{i.active ? "Active" : i.revoked ? "Revoked" : "Expired or used up"}</span>
              <span className="block text-xs text-muted-foreground">Created {whenText(i.createdAt)} · expires {whenText(i.expiresAt)} · {i.useCount}{i.maxUses ? `/${i.maxUses}` : ""} joined</span>
            </span>
            {!i.revoked ? <button type="button" onClick={() => void revoke(i)} className="rounded-full px-3 py-1 text-xs font-semibold text-destructive hover:bg-muted">Revoke</button> : null}
          </div>
        ))}
      </div>
    </section>
  );
}

function Scheduled({ channel }: { channel: Channel }) {
  const posts = useScheduledPosts(channel.channelId, true);
  return (
    <section className="space-y-3 px-4 py-4">
      {posts.isPending ? <div className="h-24 animate-pulse rounded-2xl bg-muted" /> : null}
      {posts.data?.length === 0 ? <p className="py-8 text-center text-sm text-muted-foreground">Nothing scheduled. Use “Schedule” in the composer to publish later.</p> : null}
      {posts.data?.map((p) => <ChannelPostCard key={p.postId} post={p} channel={channel} onRemoved={() => void posts.refetch()} />)}
    </section>
  );
}

function Verification({ channel }: { channel: Channel }) {
  const verification = useChannelVerification(channel.channelId, true);
  const moderation = useChannelModeration(channel.channelId, true);
  const [basis, setBasis] = useState("");
  const [busy, setBusy] = useState(false);
  const pending = verification.data?.applications.some((a) => a.state === "PENDING");
  const apply = async () => {
    setBusy(true);
    try { await channelsService.applyVerification(channel.channelId, basis); toast("Application sent to CF Meet"); void verification.refetch(); } catch (e) { toast(errorText(e), "error"); } finally { setBusy(false); }
  };
  return (
    <section className="space-y-4 px-4 py-4" data-testid="channel-verification">
      <div className="rounded-2xl border border-border p-4">
        <p className="flex items-center gap-2 font-semibold">{verification.data?.verified ? <><VerifiedBadge className="size-5" />Verified</> : "Not verified"}</p>
        <p className="mt-1 text-sm text-muted-foreground">CF Meet reviews every application. Channel owners can&apos;t verify themselves.</p>
        {!verification.data?.verified && channel.myRole === "OWNER" && !pending ? (
          <form className="mt-3 space-y-2" onSubmit={(e) => { e.preventDefault(); void apply(); }}>
            <select value={basis} onChange={(e) => setBasis(e.target.value)} className="h-11 w-full rounded-xl border border-border bg-background px-3 text-sm" aria-label="Why should this channel be verified?">
              <option value="">Why should this channel be verified?</option>
              {VERIFICATION_BASES.map((b) => <option key={b.code} value={b.code}>{b.label}</option>)}
            </select>
            <p className="text-xs text-muted-foreground">The channel must be public, active and have a username.</p>
            <button type="submit" disabled={!basis || busy} className="h-11 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-50">{busy ? "Sending…" : "Apply for verification"}</button>
          </form>
        ) : null}
        {pending ? <p className="mt-3 rounded-xl bg-muted px-3 py-2 text-sm">Your application is being reviewed.</p> : null}
        {verification.data?.applications.length ? (
          <ul className="mt-3 divide-y divide-border text-sm">
            {verification.data.applications.map((a) => (
              <li key={a.applicationId} className="flex justify-between gap-2 py-2"><span>{VERIFICATION_BASES.find((b) => b.code === a.basis)?.label ?? a.basis}</span><span className="text-muted-foreground">{a.state.toLowerCase()} · {whenText(a.decidedAt ?? a.createdAt)}</span></li>
            ))}
          </ul>
        ) : null}
      </div>
      <div className="rounded-2xl border border-border p-4">
        <p className="font-semibold">Channel status: {moderation.data?.moderationState.toLowerCase() ?? channel.moderationState.toLowerCase()}</p>
        {moderation.data?.actions.length ? (
          <ul className="mt-2 divide-y divide-border text-sm">
            {moderation.data.actions.map((a) => (
              <li key={a.actionId} className="py-2"><span className="font-medium">{a.action.replaceAll("_", " ").toLowerCase()}</span> <span className="text-muted-foreground">· {a.reason.replaceAll("_", " ").toLowerCase()} · {whenText(a.createdAt)}</span></li>
            ))}
          </ul>
        ) : <p className="mt-1 text-sm text-muted-foreground">CF Meet hasn&apos;t taken any action on this channel.</p>}
      </div>
    </section>
  );
}

const ACTIONS: Record<string, string> = {
  CHANNEL_CREATED: "created the channel", CHANNEL_UPDATED: "edited the channel", ADMIN_ADDED: "added an admin", ADMIN_CHANGED: "changed an admin's permissions",
  ADMIN_REMOVED: "removed an admin", OWNERSHIP_TRANSFERRED: "transferred ownership", INVITE_CREATED: "created an invite link", INVITE_REVOKED: "revoked an invite link",
  POST_DELETED: "deleted a post", POST_EDITED: "edited a post", POST_PINNED: "pinned a post", POST_UNPINNED: "unpinned a post", POST_SCHEDULED: "scheduled a post",
  SCHEDULED_POST_CANCELLED: "cancelled a scheduled post", VERIFICATION_REQUESTED: "applied for verification",
};

function Audit({ channel }: { channel: Channel }) {
  const audit = useChannelAudit(channel.channelId, true);
  const admins = useChannelAdmins(channel.channelId, true);
  const names = new Map<string, string>([[admins.data?.ownerPrincipalId ?? "", admins.data?.ownerName ?? "Owner"], ...(admins.data?.admins ?? []).map((a) => [a.principalId, a.name ?? "Admin"] as [string, string])]);
  const items = audit.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <section className="px-4 py-4">
      <ul className="divide-y divide-border rounded-2xl border border-border text-sm">
        {items.length === 0 && !audit.isPending ? <li className="px-3 py-4 text-muted-foreground">No activity yet.</li> : null}
        {items.map((i) => (
          <li key={i.entryId} className="px-3 py-2.5"><span className="font-medium">{names.get(i.actorPrincipalId) ?? "A former admin"}</span> {ACTIONS[i.action] ?? i.action.toLowerCase()}<span className="block text-xs text-muted-foreground">{whenText(i.occurredAt)}</span></li>
        ))}
      </ul>
      {audit.hasNextPage ? <button type="button" onClick={() => void audit.fetchNextPage()} className="mt-3 h-9 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10">Load more</button> : null}
    </section>
  );
}

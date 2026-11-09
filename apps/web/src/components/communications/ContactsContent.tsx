"use client";

/**
 * Contacts (Blueprint Section 17, MSG-006 / ACC-009) over the Communications Hub relay: your
 * CF Meet ID and QR, invite links, find by ID, contact requests, connections, blocks, privacy
 * and the unified activity timeline (calls, meetings and chats). Contacts are PERSONAL: in an
 * organization workspace the organization's colleague directory is shown separately — a
 * coworker is not automatically a contact.
 *
 * Calls are launched by the web Calling client: this screen only links to it (`callHref`).
 */
import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Ban, Copy, History, Link2, MessageSquare, Phone, QrCode as QrIcon, RefreshCw, Search, Shield, Star, UserCheck, UserPlus, Users, Video } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import QrCode from "./QrCode";
import {

  useHubActions,
  useHubActivity,
  useHubBlocks,
  useHubCallHistory,
  useHubCapabilities,
  useHubContacts,
  useHubInvites,
  useHubPrivacy,
  useHubProfile,
  useHubRequests,
  useHubSync,
} from "@/hooks/v2/useCommunications";
import { useMessagingDirectory } from "@/hooks/v2/useMessaging";
import { useWorkspace } from "@/context/WorkspaceContext";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { organizationsService } from "@/services/v2/organizations.service";
import { relay } from "@/services/v2/messaging.service";
import { communicationsErrorMessage, isUnavailable } from "@/lib/communications/errors";
import { openDirectChatWithColleague, openDirectChatWithContact } from "@/lib/communications/direct-chat";
import {
  callHref,
  chatHref,
  colleaguesOfOrganization,
  communicationsAreas,
  connectedContacts,
  contactLabel,
  formatDuration,
  meetingJoinHref,
  normalizeCfMeetId,
  type CommunicationsAreas,
} from "@/lib/communications/comms-core";
import type { Contact, Privacy, PublicProfile, TimelineCallCard, TimelineMeetingCard } from "@/types/communications";

type Tab = "connections" | "requests" | "find" | "blocked" | "privacy" | "activity" | "colleagues";

function ErrorText({ error, retry }: { error: unknown; retry?: () => void }) {
  if (!error) return null;
  return (
    <p role="alert" className="text-sm text-destructive" data-testid="contacts-error">
      {communicationsErrorMessage(error)} {retry ? <button type="button" className="underline" onClick={retry}>Retry</button> : null}
    </p>
  );
}

function copy(text: string) {
  void navigator.clipboard?.writeText(text).catch(() => {});
}

function MyIdCard({ areas }: { areas: CommunicationsAreas }) {
  const profile = useHubProfile(areas.contacts);
  const actions = useHubActions();
  const qr = useMutation({ mutationFn: () => hub.qr(), retry: false });
  const [confirmRotate, setConfirmRotate] = useState(false);
  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-4" aria-label="My CF Meet ID" data-testid="my-cfmeet-id">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <p className="text-xs uppercase tracking-wide text-muted-foreground">My CF Meet ID</p>
          {profile.isPending ? <Spinner /> : profile.data ? (
            <p className="font-mono text-base" data-testid="cfmeet-id">{profile.data.cfMeetId}</p>
          ) : null}
          {profile.data ? <p className="text-sm text-muted-foreground">{profile.data.displayName}</p> : null}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="outline" disabled={!profile.data} onClick={() => profile.data && copy(profile.data.cfMeetId)}><Copy className="h-4 w-4" /> Copy ID</Button>
          <Button size="sm" variant="outline" data-testid="show-qr" disabled={qr.isPending} onClick={() => qr.mutate()}><QrIcon className="h-4 w-4" /> {qr.data ? "New QR code" : "Show QR code"}</Button>
          <Button size="sm" variant="ghost" onClick={() => setConfirmRotate(true)}><RefreshCw className="h-4 w-4" /> Change ID</Button>
        </div>
      </div>
      <ErrorText error={profile.error} retry={() => void profile.refetch()} />
      {qr.error ? <ErrorText error={qr.error} /> : null}
      {qr.data?.payload ? (
        <div className="flex flex-wrap items-center gap-4" data-testid="qr-panel">
          <QrCode value={qr.data.payload} label="QR code to connect with you on CF Meet" />
          <div className="space-y-1 text-sm">
            <p>Anyone who scans this can send you a contact request. It works once and expires {new Date(qr.data.expiresAt).toLocaleString()}.</p>
            <Button size="sm" variant="outline" onClick={() => copy(qr.data!.payload!)}><Link2 className="h-4 w-4" /> Copy link</Button>
          </div>
        </div>
      ) : null}
      {confirmRotate ? (
        <div className="space-y-2 rounded-xl border border-destructive/40 p-3 text-sm" role="alertdialog" aria-label="Change your CF Meet ID">
          <p>Changing your CF Meet ID revokes your invite links and QR codes. Existing connections stay.</p>
          <div className="flex gap-2">
            <Button size="sm" variant="destructive" disabled={actions.rotateId.isPending} onClick={async () => {
              const id = await hub.publicId().catch(() => null);
              if (id) actions.rotateId.mutate(id.version, { onSuccess: () => { setConfirmRotate(false); void profile.refetch(); qr.reset(); } });
            }}>Change ID</Button>
            <Button size="sm" variant="ghost" onClick={() => setConfirmRotate(false)}>Cancel</Button>
          </div>
          <ErrorText error={actions.rotateId.error} />
        </div>
      ) : null}
    </section>
  );
}

function InviteLinks({ areas }: { areas: CommunicationsAreas }) {
  const invites = useHubInvites(areas.contacts);
  const actions = useHubActions();
  const [maxUses, setMaxUses] = useState(1);
  const created = actions.createInvite.data;
  return (
    <section className="space-y-3" aria-label="Invite links" data-testid="invite-links">
      <h3 className="text-sm font-semibold">Invite links</h3>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <label className="flex items-center gap-2">Uses
          <select className="rounded-md border border-input bg-background px-2 py-1" value={maxUses} onChange={(e) => setMaxUses(Number(e.target.value))}>
            {[1, 5, 10].map((n) => <option key={n} value={n}>{n}</option>)}
          </select>
        </label>
        <Button size="sm" data-testid="create-invite" disabled={actions.createInvite.isPending} onClick={() => actions.createInvite.mutate({ expiresInSeconds: 7 * 24 * 3600, maxUses })}>
          <Link2 className="h-4 w-4" /> Create link (7 days)
        </Button>
      </div>
      <ErrorText error={actions.createInvite.error} />
      {created?.payload ? (
        <div className="rounded-xl border border-border p-3 text-sm" data-testid="invite-created">
          <p className="mb-1">Share this link now — it is shown only once:</p>
          <div className="flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded bg-muted px-2 py-1">{created.payload}</code>
            <Button size="sm" variant="outline" onClick={() => copy(created.payload!)}><Copy className="h-4 w-4" /> Copy</Button>
          </div>
        </div>
      ) : null}
      {invites.isPending ? <Spinner /> : <ErrorText error={invites.error} retry={() => void invites.refetch()} />}
      <ul className="space-y-1 text-sm">
        {(invites.data?.data ?? []).map((i) => (
          <li key={i.inviteId} className="flex items-center justify-between gap-2 rounded-lg px-2 py-1 hover:bg-muted/50" data-testid="invite">
            <span>
              {i.revoked ? "Revoked" : new Date(i.expiresAt) < new Date() ? "Expired" : `Active until ${new Date(i.expiresAt).toLocaleDateString()}`} · {i.uses}/{i.maxUses} used
            </span>
            {!i.revoked ? (
              <Button size="sm" variant="ghost" data-testid="revoke-invite" disabled={actions.revokeInvite.isPending} onClick={() => actions.revokeInvite.mutate(i.inviteId)}>Revoke</Button>
            ) : null}
          </li>
        ))}
      </ul>
      <ErrorText error={actions.revokeInvite.error} />
    </section>
  );
}

function ProfileCard({ profile, onDone }: { profile: PublicProfile; onDone?: () => void }) {
  const actions = useHubActions();
  const [reported, setReported] = useState(false);
  const canRequest = profile.permittedActions?.requestConnection ?? profile.relationship === "NONE";
  return (
    <div className="space-y-2 rounded-xl border border-border p-3" data-testid="public-profile">
      <p className="font-medium">{profile.displayName}</p>
      <p className="font-mono text-xs text-muted-foreground">{profile.cfMeetId}</p>
      <p className="text-xs text-muted-foreground">Relationship: {profile.relationship ?? "NONE"}</p>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" data-testid="send-request" disabled={!canRequest || actions.requestContact.isPending || actions.requestContact.isSuccess}
          onClick={() => actions.requestContact.mutate(profile.cfMeetId, { onSuccess: () => onDone?.() })}>
          <UserPlus className="h-4 w-4" /> {actions.requestContact.isSuccess ? "Request sent" : "Send contact request"}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => actions.block.mutate(profile.cfMeetId)} disabled={actions.block.isPending}><Ban className="h-4 w-4" /> Block</Button>
        <Button size="sm" variant="ghost" disabled={reported || actions.report.isPending} onClick={() => actions.report.mutate({ cfMeetId: profile.cfMeetId, reason: "SPAM" }, { onSuccess: () => setReported(true) })}>
          {reported ? "Reported" : "Report spam"}
        </Button>
      </div>
      <ErrorText error={actions.requestContact.error ?? actions.block.error ?? actions.report.error} />
    </div>
  );
}

function FindTab({ areas }: { areas: CommunicationsAreas }) {
  const [value, setValue] = useState("");
  const resolve = useMutation({ mutationFn: (id: string) => hub.resolvePublicId(id), retry: false });
  const normalized = normalizeCfMeetId(value);
  return (
    <div className="space-y-6">
      <MyIdCard areas={areas} />
      <section className="space-y-2" aria-label="Find someone by CF Meet ID">
        <h3 className="text-sm font-semibold">Find someone by CF Meet ID</h3>
        <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); if (normalized) resolve.mutate(normalized); }}>
          <Input aria-label="CF Meet ID" placeholder="CF-…" value={value} maxLength={40} onChange={(e) => setValue(e.target.value)} data-testid="find-id" />
          <Button type="submit" size="sm" disabled={!normalized || resolve.isPending} data-testid="find-submit"><Search className="h-4 w-4" /> Find</Button>
        </form>
        {value && !normalized ? <p className="text-xs text-muted-foreground">A CF Meet ID looks like CF- followed by 26 letters and digits.</p> : null}
        <ErrorText error={resolve.error} />
        {resolve.data ? <ProfileCard profile={resolve.data} /> : null}
      </section>
      <InviteLinks areas={areas} />
    </div>
  );
}

function RequestsTab({ areas }: { areas: CommunicationsAreas }) {
  const { incoming, outgoing } = useHubRequests(areas.contacts);
  const actions = useHubActions();
  const pendingIn = (incoming.data?.data ?? []).filter((r) => r.state === "PENDING");
  const pendingOut = (outgoing.data?.data ?? []).filter((r) => r.state === "PENDING");
  return (
    <div className="space-y-6">
      <section aria-label="Incoming requests" className="space-y-2">
        <h3 className="text-sm font-semibold">Incoming</h3>
        {incoming.isPending ? <Spinner /> : <ErrorText error={incoming.error} retry={() => void incoming.refetch()} />}
        {incoming.isSuccess && !pendingIn.length ? <p className="text-sm text-muted-foreground">No pending requests.</p> : null}
        <ul className="space-y-2">
          {pendingIn.map((r) => (
            <li key={r.requestId} className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-border p-3" data-testid="incoming-request">
              <span className="font-mono text-sm">{r.cfMeetId}</span>
              <span className="flex gap-2">
                <Button size="sm" data-testid="accept-request" disabled={actions.accept.isPending} onClick={() => actions.accept.mutate({ requestId: r.requestId, version: r.version })}><UserCheck className="h-4 w-4" /> Accept</Button>
                <Button size="sm" variant="outline" data-testid="decline-request" disabled={actions.decline.isPending} onClick={() => actions.decline.mutate({ requestId: r.requestId, version: r.version })}>Decline</Button>
                <Button size="sm" variant="ghost" onClick={() => actions.block.mutate(r.cfMeetId)}><Ban className="h-4 w-4" /> Block</Button>
              </span>
            </li>
          ))}
        </ul>
        <ErrorText error={actions.accept.error ?? actions.decline.error} />
      </section>
      <section aria-label="Sent requests" className="space-y-2">
        <h3 className="text-sm font-semibold">Sent</h3>
        {outgoing.isPending ? <Spinner /> : <ErrorText error={outgoing.error} retry={() => void outgoing.refetch()} />}
        {outgoing.isSuccess && !pendingOut.length ? <p className="text-sm text-muted-foreground">No requests waiting for an answer.</p> : null}
        <ul className="space-y-2">
          {pendingOut.map((r) => (
            <li key={r.requestId} className="flex items-center justify-between gap-2 rounded-xl border border-border p-3" data-testid="outgoing-request">
              <span className="font-mono text-sm">{r.cfMeetId}</span>
              <Button size="sm" variant="ghost" disabled={actions.cancel.isPending} onClick={() => actions.cancel.mutate({ requestId: r.requestId, version: r.version })}>Cancel</Button>
            </li>
          ))}
        </ul>
        <ErrorText error={actions.cancel.error} />
      </section>
    </div>
  );
}

function ContactRow({ contact, areas }: { contact: Contact; areas: CommunicationsAreas }) {
  const router = useRouter();
  const actions = useHubActions();
  const [editing, setEditing] = useState(false);
  const [alias, setAlias] = useState(contact.alias ?? "");
  const open = useMutation({ mutationFn: () => openDirectChatWithContact(contact.cfMeetId), retry: false, onSuccess: (id) => router.push(chatHref(id)) });
  const connected = contact.relationship === "CONNECTED";
  return (
    <li className="space-y-2 rounded-xl border border-border p-3" data-testid="contact" data-relationship={contact.relationship}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <p className="truncate font-medium">{contactLabel(contact)} {contact.favorite ? <Star className="inline h-3.5 w-3.5 fill-current text-amber-500" aria-label="Favorite" /> : null}</p>
          <p className="truncate font-mono text-xs text-muted-foreground">{contact.alias ? `${contact.displayName} · ` : ""}{contact.cfMeetId}</p>
        </div>
        <div className="flex flex-wrap gap-1">
          {areas.managedMessaging && connected ? (
            <Button size="sm" variant="outline" data-testid="contact-message" disabled={open.isPending} onClick={() => open.mutate()} title="Managed chat: readable by CF Meet servers">
              <MessageSquare className="h-4 w-4" /> Message
            </Button>
          ) : null}
          {areas.calling && connected ? (
            <>
              <Button asChild size="sm" variant="ghost"><Link href={callHref({ contactId: contact.contactId }, "VOICE")} data-testid="contact-call" aria-label={`Call ${contactLabel(contact)}`}><Phone className="h-4 w-4" /></Link></Button>
              <Button asChild size="sm" variant="ghost"><Link href={callHref({ contactId: contact.contactId }, "VIDEO")} aria-label={`Video call ${contactLabel(contact)}`}><Video className="h-4 w-4" /></Link></Button>
            </>
          ) : null}
          <Button size="sm" variant="ghost" aria-pressed={contact.favorite} onClick={() => actions.updateContact.mutate({ contactId: contact.contactId, expectedVersion: contact.version, favorite: !contact.favorite })}>
            <Star className="h-4 w-4" />
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setEditing(!editing)}>More</Button>
        </div>
      </div>
      {areas.managedMessaging && connected ? <p className="text-[11px] text-muted-foreground">Messages are Managed: readable by CF Meet servers.</p> : null}
      {editing ? (
        <div className="flex flex-wrap items-center gap-2 border-t border-border pt-2">
          <Input aria-label="Alias (only you see it)" placeholder="Alias (only you see it)" maxLength={64} value={alias} onChange={(e) => setAlias(e.target.value)} className="h-9 max-w-xs" />
          <Button size="sm" variant="outline" disabled={actions.updateContact.isPending} onClick={() => actions.updateContact.mutate({ contactId: contact.contactId, expectedVersion: contact.version, alias: alias.trim() || null })}>Save alias</Button>
          <select aria-label="Notifications" className="rounded-md border border-input bg-background px-2 py-1 text-sm" value={contact.notifications}
            onChange={(e) => actions.updateContact.mutate({ contactId: contact.contactId, expectedVersion: contact.version, notifications: e.target.value as Contact["notifications"] })}>
            <option value="ALL">All notifications</option>
            <option value="MENTIONS">Mentions only</option>
            <option value="MUTED">Muted</option>
          </select>
          <Button size="sm" variant="ghost" data-testid="contact-block" onClick={() => actions.block.mutate(contact.cfMeetId)}><Ban className="h-4 w-4" /> Block</Button>
          <Button size="sm" variant="ghost" data-testid="contact-remove" onClick={() => actions.removeContact.mutate(contact.contactId)}>Remove contact</Button>
        </div>
      ) : null}
      <ErrorText error={open.error ?? actions.updateContact.error ?? actions.block.error ?? actions.removeContact.error} />
    </li>
  );
}

function ConnectionsTab({ areas }: { areas: CommunicationsAreas }) {
  const contacts = useHubContacts(areas.contacts);
  const [filter, setFilter] = useState("");
  const list = useMemo(() => {
    const needle = filter.trim().toLocaleLowerCase();
    return connectedContacts(contacts.data ?? [])
      .filter((c) => !needle || contactLabel(c).toLocaleLowerCase().includes(needle) || c.cfMeetId.toLowerCase().includes(needle))
      .sort((a, b) => Number(b.favorite) - Number(a.favorite) || contactLabel(a).localeCompare(contactLabel(b)));
  }, [contacts.data, filter]);
  return (
    <div className="space-y-3">
      <Input aria-label="Search contacts" placeholder="Search contacts" value={filter} onChange={(e) => setFilter(e.target.value)} className="max-w-sm" />
      {contacts.isPending ? <Spinner /> : <ErrorText error={contacts.error} retry={() => void contacts.refetch()} />}
      {contacts.isSuccess && !list.length ? <p className="text-sm text-muted-foreground">{filter ? "No matching contacts." : "No connections yet. Share your CF Meet ID or an invite link to connect."}</p> : null}
      <ul className="space-y-2" data-testid="contact-list">
        {list.map((c) => <ContactRow key={c.contactId} contact={c} areas={areas} />)}
      </ul>
    </div>
  );
}

function BlockedTab({ areas }: { areas: CommunicationsAreas }) {
  const blocks = useHubBlocks(areas.contacts);
  const actions = useHubActions();
  return (
    <div className="space-y-2">
      <p className="text-sm text-muted-foreground">Blocked people cannot call you, message you or send requests. They are not told.</p>
      {blocks.isPending ? <Spinner /> : <ErrorText error={blocks.error} retry={() => void blocks.refetch()} />}
      {blocks.isSuccess && !blocks.data.data.length ? <p className="text-sm text-muted-foreground">Nobody is blocked.</p> : null}
      <ul className="space-y-2">
        {(blocks.data?.data ?? []).map((b) => (
          <li key={b.cfMeetId} className="flex items-center justify-between rounded-xl border border-border p-3" data-testid="blocked">
            <span className="font-mono text-sm">{b.cfMeetId}</span>
            <Button size="sm" variant="outline" data-testid="unblock" disabled={actions.unblock.isPending} onClick={() => actions.unblock.mutate(b.cfMeetId)}>Unblock</Button>
          </li>
        ))}
      </ul>
      <ErrorText error={actions.unblock.error} />
    </div>
  );
}

const PRIVACY_FIELDS: { key: keyof Privacy; label: string; options: [string, string][] }[] = [
  { key: "contactRequestPermissions", label: "Who can send me contact requests", options: [["EVERYONE", "Everyone"], ["NOBODY", "Nobody"]] },
  { key: "invitationPermissions", label: "Invite links and QR codes", options: [["ENABLED", "Enabled"], ["DISABLED", "Disabled"]] },
  { key: "incomingCallPolicy", label: "Who can call me", options: [["CONNECTIONS", "My connections"], ["NOBODY", "Nobody"]] },
  { key: "directChatPolicy", label: "Who can start a chat with me", options: [["CONNECTIONS", "My connections"], ["NOBODY", "Nobody"]] },
  { key: "publicProfileVisibility", label: "Who can see my profile", options: [["PUBLIC", "Anyone with my ID"], ["CONNECTIONS", "My connections"], ["HIDDEN", "Nobody"]] },
  { key: "presenceVisibility", label: "Who can see when I'm online", options: [["CONNECTIONS", "My connections"], ["NOBODY", "Nobody"]] },
];

function PrivacyTab({ areas }: { areas: CommunicationsAreas }) {
  const privacy = useHubPrivacy(areas.contacts);
  const actions = useHubActions();
  const [draft, setDraft] = useState<Privacy | null>(null);
  const value = draft ?? privacy.data?.privacy ?? null;
  if (privacy.isPending) return <Spinner />;
  if (!value) return <ErrorText error={privacy.error} retry={() => void privacy.refetch()} />;
  return (
    <form className="max-w-lg space-y-3" data-testid="privacy-form" onSubmit={(e) => {
      e.preventDefault();
      if (privacy.data && draft) actions.updatePrivacy.mutate({ version: privacy.data.version, privacy: draft }, { onSuccess: () => setDraft(null) });
    }}>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={value.publicIdDiscoverable} onChange={(e) => setDraft({ ...value, publicIdDiscoverable: e.target.checked })} />
        People can find me by my CF Meet ID
      </label>
      {PRIVACY_FIELDS.map((f) => (
        <label key={f.key} className="flex items-center justify-between gap-3 text-sm">
          {f.label}
          <select className="rounded-md border border-input bg-background px-2 py-1" value={String(value[f.key])} onChange={(e) => setDraft({ ...value, [f.key]: e.target.value } as Privacy)}>
            {f.options.map(([v, l]) => <option key={v} value={v}>{l}</option>)}
          </select>
        </label>
      ))}
      <div className="flex gap-2">
        <Button type="submit" size="sm" disabled={!draft || actions.updatePrivacy.isPending} data-testid="save-privacy"><Shield className="h-4 w-4" /> Save</Button>
        {draft ? <Button type="button" size="sm" variant="ghost" onClick={() => setDraft(null)}>Discard</Button> : null}
      </div>
      <ErrorText error={actions.updatePrivacy.error} />
    </form>
  );
}

type RecentChat = { conversationId: string; title: string | null; securityProfile: string; inbox?: { lastActivityAt: string; unreadCount: number; lastMessage: { previewText: string | null; attachmentCount: number } | null } };

function ActivityTab({ areas }: { areas: CommunicationsAreas }) {
  const activity = useHubActivity(areas.contacts);
  const [missedOnly, setMissedOnly] = useState(false);
  const calls = useHubCallHistory(missedOnly, areas.contacts);
  // Chats join the unified timeline from Messaging's inbox order (MANAGED previews only).
  const chats = useQuery({
    queryKey: ["communications", "timeline", "chats"],
    queryFn: () => relay<{ conversations: RecentChat[] }>("GET", "v1/conversations?limit=20&orderBy=lastActivity"),
    enabled: areas.managedMessaging,
    retry: false,
    refetchInterval: 60_000,
  });
  type Item = { key: string; at: string; node: React.ReactNode };
  const items: Item[] = [];
  for (const e of activity.data?.data ?? []) {
    const call = (e.call ?? null) as TimelineCallCard | null;
    const meeting = (e.meeting ?? null) as TimelineMeetingCard | null;
    items.push({
      key: `a:${e.entryId}`,
      at: e.occurredAt,
      node: (
        <span className="flex flex-wrap items-center gap-2">
          {e.kind === "CALL" ? <Phone className="h-4 w-4" /> : <Video className="h-4 w-4" />}
          <span>{e.kind === "CALL" ? `${call?.missed ? "Missed " : ""}${call?.kind === "VIDEO" ? "video" : "voice"} call${call?.outcome ? ` · ${call.outcome.toLowerCase()}` : ""}${call?.durationMs ? ` · ${formatDuration(call.durationMs)}` : ""}` : `Meeting ${meeting?.origin === "INSTANT" ? "started" : "shared"}${meeting?.meetingCode ? ` · ${meeting.meetingCode}` : ""}`}</span>
          {meeting?.meetingCode ? <Link className="text-primary underline" href={meetingJoinHref(meeting.meetingCode)}>Join</Link> : null}
          {e.conversationId ? <Link className="text-primary underline" href={chatHref(e.conversationId)}>Open chat</Link> : null}
        </span>
      ),
    });
  }
  for (const c of chats.data?.conversations ?? []) {
    if (!c.inbox) continue;
    const preview = c.securityProfile === "MANAGED" ? (c.inbox.lastMessage?.previewText ?? (c.inbox.lastMessage?.attachmentCount ? "Attachment" : "")) : "End-to-end encrypted message";
    items.push({
      key: `c:${c.conversationId}`,
      at: c.inbox.lastActivityAt,
      node: (
        <span className="flex flex-wrap items-center gap-2">
          <MessageSquare className="h-4 w-4" />
          <span className="truncate">{c.title ?? "Chat"}{preview ? `: ${preview}` : ""}{c.inbox.unreadCount ? ` · ${c.inbox.unreadCount} unread` : ""}</span>
          <Link className="text-primary underline" href={chatHref(c.conversationId)}>Open</Link>
        </span>
      ),
    });
  }
  items.sort((a, b) => b.at.localeCompare(a.at));
  return (
    <div className="space-y-6">
      <section className="space-y-2" aria-label="Recent activity">
        <h3 className="text-sm font-semibold">Recent activity</h3>
        {activity.isPending ? <Spinner /> : <ErrorText error={activity.error ?? chats.error} retry={() => { void activity.refetch(); void chats.refetch(); }} />}
        {activity.isSuccess && !items.length ? <p className="text-sm text-muted-foreground">No recent calls, meetings or chats.</p> : null}
        <ul className="space-y-1 text-sm" data-testid="activity">
          {items.slice(0, 50).map((i) => (
            <li key={i.key} className="flex items-center justify-between gap-2 rounded-lg px-2 py-1.5 hover:bg-muted/50" data-testid="activity-item">
              {i.node}
              <time className="shrink-0 text-xs text-muted-foreground">{new Date(i.at).toLocaleString()}</time>
            </li>
          ))}
        </ul>
      </section>
      <section className="space-y-2" aria-label="Calls">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold">Calls</h3>
          <label className="flex items-center gap-2 text-xs"><input type="checkbox" checked={missedOnly} onChange={(e) => setMissedOnly(e.target.checked)} /> Missed only</label>
        </div>
        {calls.isPending ? <Spinner /> : <ErrorText error={calls.error} retry={() => void calls.refetch()} />}
        <ul className="space-y-1 text-sm">
          {(calls.data?.data ?? []).map((c) => {
            const who = c.counterparts.filter((p) => !p.self).map((p) => p.alias || p.displayName || p.cfMeetId || "CF Meet user").join(", ") || "Call";
            return (
              <li key={c.callId} className="flex items-center justify-between gap-2 rounded-lg px-2 py-1.5 hover:bg-muted/50" data-testid="call-entry">
                <span className={cn("flex items-center gap-2", c.missed && "text-destructive")}>
                  <Phone className="h-4 w-4" /> {who} · {c.direction === "INCOMING" ? "incoming" : "outgoing"}{c.missed ? " · missed" : ""}{c.durationMs ? ` · ${formatDuration(c.durationMs)}` : ""}
                </span>
                <span className="flex items-center gap-2">
                  {areas.calling && c.conversationId ? <Link className="text-primary underline" href={callHref({ conversationId: c.conversationId }, c.kind === "VIDEO" ? "VIDEO" : "VOICE")}>Call back</Link> : null}
                  {c.createdAt ? <time className="text-xs text-muted-foreground">{new Date(c.createdAt).toLocaleString()}</time> : null}
                </span>
              </li>
            );
          })}
        </ul>
      </section>
    </div>
  );
}

/** Organization workspace only: the selected organization's colleague directory (not contacts). */
function ColleaguesTab({ organizationId, organizationName }: { organizationId: string; organizationName: string }) {
  const router = useRouter();
  const [search, setSearch] = useState("");
  const directory = useMessagingDirectory(search, true);
  const members = useQuery({ queryKey: ["workspace", organizationId, "members"], queryFn: () => organizationsService.listMembers(organizationId), retry: false });
  const people = colleaguesOfOrganization(directory.data?.people ?? [], members.data);
  const open = useMutation({ mutationFn: (userId: string) => openDirectChatWithColleague(userId), retry: false, onSuccess: (id) => router.push(chatHref(id)) });
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">Colleagues in {organizationName}. Colleagues are not your personal contacts; chats with them are Managed (readable by CF Meet servers).</p>
      <Input aria-label="Search colleagues" placeholder="Search colleagues" maxLength={64} value={search} onChange={(e) => setSearch(e.target.value)} className="max-w-sm" />
      {directory.isPending || members.isPending ? <Spinner /> : <ErrorText error={directory.error ?? members.error} />}
      <ul className="space-y-2" data-testid="colleague-list">
        {people.map((p) => (
          <li key={p.userId} className="flex items-center justify-between rounded-xl border border-border p-3" data-testid="org-colleague">
            <span>{p.displayName}</span>
            <Button size="sm" variant="outline" disabled={open.isPending} onClick={() => open.mutate(p.userId)}><MessageSquare className="h-4 w-4" /> Message</Button>
          </li>
        ))}
      </ul>
      <ErrorText error={open.error} />
    </div>
  );
}

export default function ContactsContent() {
  const workspace = useWorkspace();
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  useHubSync(areas.contacts);
  const { incoming } = useHubRequests(areas.contacts);
  const pendingCount = (incoming.data?.data ?? []).filter((r) => r.state === "PENDING").length;
  const organization = workspace.kind === "ORGANIZATION" && workspace.organizationId ? { id: workspace.organizationId, name: workspace.workspace?.displayName ?? "this organization" } : null;
  const [tab, setTab] = useState<Tab>("connections");
  const tabs: [Tab, string, React.ReactNode][] = [
    ["connections", "Connections", <Users key="i" className="h-4 w-4" />],
    ["requests", pendingCount ? `Requests (${pendingCount})` : "Requests", <UserPlus key="i" className="h-4 w-4" />],
    ["find", "Find & invite", <QrIcon key="i" className="h-4 w-4" />],
    ["activity", "Activity", <History key="i" className="h-4 w-4" />],
    ["blocked", "Blocked", <Ban key="i" className="h-4 w-4" />],
    ["privacy", "Privacy", <Shield key="i" className="h-4 w-4" />],
    ...(organization ? ([["colleagues", "Colleagues", <Users key="i" className="h-4 w-4" />]] as [Tab, string, React.ReactNode][]) : []),
  ];
  return (
    <div className="mx-auto max-w-4xl space-y-4 p-3 sm:p-5" data-testid="contacts-page" data-workspace={workspace.kind}>
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Contacts</h1>
        <p className="mt-0.5 text-sm text-muted-foreground">Your personal connections on CF Meet{organization ? `, and colleagues in ${organization.name}` : ""}.</p>
      </header>
      {capabilities.isPending ? (
        <p className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner /> Loading…</p>
      ) : capabilities.error ? (
        <div className="rounded-xl border border-border p-4 text-sm" data-testid="contacts-unavailable">
          {isUnavailable(capabilities.error) ? "Personal contacts are not available in this environment." : communicationsErrorMessage(capabilities.error)}
          {organization ? <div className="mt-4"><ColleaguesTab organizationId={organization.id} organizationName={organization.name} /></div> : null}
        </div>
      ) : !areas.contacts ? (
        <div className="rounded-xl border border-border p-4 text-sm" data-testid="contacts-unavailable">
          Personal contacts are not available for this account{areas.reason ? ` (${areas.reason.toLowerCase().replace(/_/g, " ")})` : ""}.
          {organization ? <div className="mt-4"><ColleaguesTab organizationId={organization.id} organizationName={organization.name} /></div> : null}
        </div>
      ) : (
        <>
          <nav className="flex flex-wrap gap-1" role="tablist" aria-label="Contacts sections">
            {tabs.map(([value, label, icon]) => (
              <button key={value} type="button" role="tab" aria-selected={tab === value} data-testid={`tab-${value}`} onClick={() => setTab(value)}
                className={cn("inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-sm font-medium transition-colors", tab === value ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-muted")}>
                {icon} {label}
              </button>
            ))}
          </nav>
          <div role="tabpanel">
            {tab === "connections" ? <ConnectionsTab areas={areas} /> : null}
            {tab === "requests" ? <RequestsTab areas={areas} /> : null}
            {tab === "find" ? <FindTab areas={areas} /> : null}
            {tab === "activity" ? <ActivityTab areas={areas} /> : null}
            {tab === "blocked" ? <BlockedTab areas={areas} /> : null}
            {tab === "privacy" ? <PrivacyTab areas={areas} /> : null}
            {tab === "colleagues" && organization ? <ColleaguesTab organizationId={organization.id} organizationName={organization.name} /> : null}
          </div>
        </>
      )}
    </div>
  );
}

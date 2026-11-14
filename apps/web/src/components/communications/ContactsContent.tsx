"use client";

/**
 * Contacts (Blueprint Section 17, MSG-006 / ACC-009) over the Communications Hub relay: your
 * CF Meet ID and QR, invite links, find by ID, contact requests, connections, blocks and
 * privacy. Contacts are PERSONAL: in an organization workspace the organization's colleague
 * directory is shown separately — a coworker is not automatically a contact.
 *
 * Layout (Google Contacts pattern): one list with favorites first and A–Z headers, quick
 * actions on each row, "Add contact" for everything about connecting, privacy behind a gear.
 * Calls and chats are launched elsewhere (Calls / Messages); the old Activity tab repeated
 * those screens and is gone.
 */
import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Ban, Bell, BellOff, Check, Copy, Link2, MessageSquare, MoreHorizontal, Pencil, Phone, QrCode as QrIcon, RefreshCw, Search, Settings2, Star, UserMinus, UserPlus, Users, Video, X } from "lucide-react";
import { Popover } from "radix-ui";

import { Spinner } from "@/components/ui/spinner";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import QrCode from "./QrCode";
import {
  useHubActions,
  useHubBlocks,
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
import { communicationsErrorMessage, isUnavailable } from "@/lib/communications/errors";
import { openDirectChatWithColleague, openDirectChatWithContact } from "@/lib/communications/direct-chat";
import {
  callHref,
  chatHref,
  colleaguesOfOrganization,
  communicationsAreas,
  connectedContacts,
  contactLabel,
  normalizeCfMeetId,
  type CommunicationsAreas,
} from "@/lib/communications/comms-core";
import type { Contact, Privacy, PublicProfile } from "@/types/communications";

type Tab = "connections" | "requests" | "blocked" | "colleagues";

const AVATAR_COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (key: string) => AVATAR_COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.replace(/^CF-/, "").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
function Avatar({ name, seed, size = "size-10", className }: { name: string; seed?: string; size?: string; className?: string }) {
  return <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white", size, className)} style={{ backgroundColor: colorFor(seed ?? name) }}>{initials(name)}</span>;
}
const shortId = (id: string) => (id.length > 14 ? `${id.slice(0, 9)}…${id.slice(-4)}` : id);

function ErrorText({ error, retry }: { error: unknown; retry?: () => void }) {
  if (!error) return null;
  return (
    <p role="alert" className="text-sm text-destructive" data-testid="contacts-error">
      {communicationsErrorMessage(error)} {retry ? <button type="button" className="font-semibold underline" onClick={retry}>Retry</button> : null}
    </p>
  );
}

function useCopy() {
  const [copied, setCopied] = useState<string | null>(null);
  return {
    copied,
    copy: (text: string, key = text) => {
      void navigator.clipboard?.writeText(text).then(() => { setCopied(key); setTimeout(() => setCopied(null), 2000); }).catch(() => {});
    },
  };
}

const pill = "inline-flex h-9 items-center justify-center gap-1.5 rounded-full px-4 text-sm font-semibold transition-colors disabled:opacity-50";
const iconBtn = "flex size-9 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-40";

/* ─── Add contact: your ID & QR, find by ID, invite links ─────────────── */

function MyIdCard({ areas }: { areas: CommunicationsAreas }) {
  const profile = useHubProfile(areas.contacts);
  const actions = useHubActions();
  const qr = useMutation({ mutationFn: () => hub.qr(), retry: false });
  const [confirmRotate, setConfirmRotate] = useState(false);
  const { copied, copy } = useCopy();
  return (
    <section className="rounded-2xl bg-muted/50 p-4" aria-label="My CF Meet ID" data-testid="my-cfmeet-id">
      <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Your CF Meet ID</p>
      <div className="mt-2 flex items-center gap-3">
        {profile.isPending ? <Spinner /> : profile.data ? <Avatar name={profile.data.displayName} seed={profile.data.cfMeetId} size="size-11" /> : null}
        <div className="min-w-0 flex-1">
          {profile.data ? <p className="truncate font-semibold">{profile.data.displayName}</p> : null}
          {profile.data ? <p className="truncate font-mono text-sm text-muted-foreground" data-testid="cfmeet-id">{profile.data.cfMeetId}</p> : null}
        </div>
        <button type="button" disabled={!profile.data} onClick={() => profile.data && copy(profile.data.cfMeetId, "id")} className={cn(pill, "bg-background text-foreground ring-1 ring-border hover:bg-muted")}>{copied === "id" ? <Check className="size-4 text-primary" /> : <Copy className="size-4" />}{copied === "id" ? "Copied" : "Copy"}</button>
      </div>
      <ErrorText error={profile.error} retry={() => void profile.refetch()} />
      <div className="mt-3 flex flex-wrap gap-2">
        <button type="button" data-testid="show-qr" disabled={qr.isPending} onClick={() => qr.mutate()} className={cn(pill, "bg-background text-foreground ring-1 ring-border hover:bg-muted")}><QrIcon className="size-4" />{qr.data ? "New QR code" : "Show QR code"}</button>
        <button type="button" onClick={() => setConfirmRotate(true)} className={cn(pill, "text-muted-foreground hover:bg-background hover:text-foreground")}><RefreshCw className="size-4" />Change ID</button>
      </div>
      {qr.error ? <div className="mt-2"><ErrorText error={qr.error} /></div> : null}
      {qr.data?.payload ? (
        <div className="mt-4 flex flex-wrap items-center gap-4 rounded-xl bg-background p-3" data-testid="qr-panel">
          <div className="rounded-xl bg-white p-2"><QrCode value={qr.data.payload} label="QR code to connect with you on CF Meet" /></div>
          <div className="min-w-0 flex-1 space-y-2 text-sm">
            <p>Anyone who scans this can send you a request. It works once and expires {new Date(qr.data.expiresAt).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}.</p>
            <button type="button" onClick={() => copy(qr.data!.payload!, "qr")} className={cn(pill, "h-8 px-3 text-primary hover:bg-primary/10")}>{copied === "qr" ? <Check className="size-4" /> : <Link2 className="size-4" />}{copied === "qr" ? "Copied" : "Copy as link"}</button>
          </div>
        </div>
      ) : null}
      {confirmRotate ? (
        <div className="mt-3 space-y-3 rounded-xl bg-destructive/5 p-3 text-sm ring-1 ring-destructive/30" role="alertdialog" aria-label="Change your CF Meet ID">
          <p>You&apos;ll get a new ID. Old invite links and QR codes stop working. Your contacts stay.</p>
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => setConfirmRotate(false)} className={cn(pill, "text-foreground hover:bg-muted")}>Cancel</button>
            <button type="button" disabled={actions.rotateId.isPending} onClick={async () => {
              const id = await hub.publicId().catch(() => null);
              if (id) actions.rotateId.mutate(id.version, { onSuccess: () => { setConfirmRotate(false); void profile.refetch(); qr.reset(); } });
            }} className={cn(pill, "bg-destructive text-white hover:bg-destructive/90")}>Change ID</button>
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
  const { copied, copy } = useCopy();
  const list = (invites.data?.data ?? []).filter((i) => !i.revoked && new Date(i.expiresAt) > new Date());
  return (
    <section className="space-y-3" aria-label="Invite links" data-testid="invite-links">
      <div>
        <h3 className="font-semibold">Invite link</h3>
        <p className="text-sm text-muted-foreground">Send a link anywhere. Whoever opens it can connect with you. Lasts 7 days.</p>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm text-muted-foreground">Can be used</span>
        <div className="flex rounded-full bg-muted p-1">
          {[1, 5, 10].map((n) => <button key={n} type="button" aria-pressed={maxUses === n} onClick={() => setMaxUses(n)} className={cn("h-7 rounded-full px-3 text-sm font-medium", maxUses === n ? "bg-background text-foreground shadow-sm" : "text-muted-foreground")}>{n === 1 ? "Once" : `${n} times`}</button>)}
        </div>
        <button type="button" data-testid="create-invite" disabled={actions.createInvite.isPending} onClick={() => actions.createInvite.mutate({ expiresInSeconds: 7 * 24 * 3600, maxUses })} className={cn(pill, "ml-auto bg-primary text-primary-foreground hover:bg-primary/90")}>
          {actions.createInvite.isPending ? <Spinner /> : <Link2 className="size-4" />}Create link
        </button>
      </div>
      <ErrorText error={actions.createInvite.error} />
      {created?.payload ? (
        <div className="rounded-xl bg-primary/5 p-3 ring-1 ring-primary/20" data-testid="invite-created">
          <p className="text-xs font-semibold text-primary">Copy it now. For your safety it&apos;s only shown once.</p>
          <div className="mt-2 flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded-lg bg-background px-3 py-2 text-sm">{created.payload}</code>
            <button type="button" onClick={() => copy(created.payload!, "invite")} className={cn(pill, "bg-primary text-primary-foreground hover:bg-primary/90")}>{copied === "invite" ? <Check className="size-4" /> : <Copy className="size-4" />}{copied === "invite" ? "Copied" : "Copy"}</button>
          </div>
        </div>
      ) : null}
      {invites.isPending ? null : <ErrorText error={invites.error} retry={() => void invites.refetch()} />}
      {list.length > 0 && (
        <ul className="divide-y divide-border rounded-xl border border-border">
          {list.map((i) => (
            <li key={i.inviteId} className="flex items-center gap-3 px-3 py-2.5 text-sm" data-testid="invite">
              <Link2 className="size-4 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1">Active until {new Date(i.expiresAt).toLocaleDateString([], { day: "numeric", month: "short" })} · <span className="text-muted-foreground">{i.uses} of {i.maxUses} used</span></span>
              <button type="button" data-testid="revoke-invite" disabled={actions.revokeInvite.isPending} onClick={() => actions.revokeInvite.mutate(i.inviteId)} className="h-8 rounded-full px-3 text-sm font-semibold text-destructive hover:bg-destructive/10">Turn off</button>
            </li>
          ))}
        </ul>
      )}
      <ErrorText error={actions.revokeInvite.error} />
    </section>
  );
}

const RELATIONSHIP: Record<string, string> = { NONE: "Not connected", CONNECTED: "Already a contact", PENDING_OUTGOING: "Request sent", PENDING_INCOMING: "Sent you a request", BLOCKED: "Blocked" };

function ProfileCard({ profile }: { profile: PublicProfile }) {
  const actions = useHubActions();
  const [reported, setReported] = useState(false);
  const canRequest = profile.permittedActions?.requestConnection ?? profile.relationship === "NONE";
  return (
    <div className="rounded-xl border border-border p-3" data-testid="public-profile">
      <div className="flex items-center gap-3">
        <Avatar name={profile.displayName} seed={profile.cfMeetId} size="size-11" />
        <div className="min-w-0 flex-1">
          <p className="truncate font-semibold">{profile.displayName}</p>
          <p className="truncate text-xs text-muted-foreground">{RELATIONSHIP[profile.relationship ?? "NONE"] ?? profile.relationship}</p>
        </div>
        <button type="button" data-testid="send-request" disabled={!canRequest || actions.requestContact.isPending || actions.requestContact.isSuccess}
          onClick={() => actions.requestContact.mutate(profile.cfMeetId)} className={cn(pill, "bg-primary text-primary-foreground hover:bg-primary/90")}>
          {actions.requestContact.isSuccess ? <Check className="size-4" /> : <UserPlus className="size-4" />}{actions.requestContact.isSuccess ? "Request sent" : "Add"}
        </button>
      </div>
      <div className="mt-2 flex justify-end gap-1">
        <button type="button" onClick={() => actions.block.mutate(profile.cfMeetId)} disabled={actions.block.isPending} className="h-8 rounded-full px-3 text-xs font-semibold text-muted-foreground hover:bg-muted">{actions.block.isSuccess ? "Blocked" : "Block"}</button>
        <button type="button" disabled={reported || actions.report.isPending} onClick={() => actions.report.mutate({ cfMeetId: profile.cfMeetId, reason: "SPAM" }, { onSuccess: () => setReported(true) })} className="h-8 rounded-full px-3 text-xs font-semibold text-muted-foreground hover:bg-muted">{reported ? "Reported" : "Report spam"}</button>
      </div>
      <ErrorText error={actions.requestContact.error ?? actions.block.error ?? actions.report.error} />
    </div>
  );
}

function AddContactDialog({ open, onOpenChange, areas }: { open: boolean; onOpenChange: (v: boolean) => void; areas: CommunicationsAreas }) {
  const [value, setValue] = useState("");
  const resolve = useMutation({ mutationFn: (id: string) => hub.resolvePublicId(id), retry: false });
  const normalized = normalizeCfMeetId(value);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90dvh] gap-0 overflow-y-auto p-0 sm:max-w-lg">
        <DialogHeader className="px-5 pb-2 pt-5 text-left">
          <DialogTitle>Add contact</DialogTitle>
          <DialogDescription>Find someone by their CF Meet ID, or share yours.</DialogDescription>
        </DialogHeader>
        <div className="space-y-6 px-5 pb-6 pt-3">
          <section className="space-y-2" aria-label="Find someone by CF Meet ID">
            <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); if (normalized) resolve.mutate(normalized); }}>
              <label className="flex h-11 min-w-0 flex-1 items-center gap-2 rounded-full bg-muted px-4 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
                <Search className="size-4 shrink-0" />
                <input autoFocus aria-label="CF Meet ID" placeholder="Their CF Meet ID, e.g. CF-7K2…" value={value} maxLength={40} onChange={(e) => setValue(e.target.value)} data-testid="find-id" className="min-w-0 flex-1 bg-transparent font-mono text-sm text-foreground outline-none placeholder:font-sans placeholder:text-muted-foreground" />
              </label>
              <button type="submit" disabled={!normalized || resolve.isPending} data-testid="find-submit" className={cn(pill, "h-11 bg-primary text-primary-foreground hover:bg-primary/90")}>{resolve.isPending ? <Spinner /> : "Find"}</button>
            </form>
            {value && !normalized ? <p className="px-2 text-xs text-muted-foreground">A CF Meet ID starts with CF- followed by 26 letters and numbers.</p> : null}
            <ErrorText error={resolve.error} />
            {resolve.data ? <ProfileCard profile={resolve.data} /> : null}
          </section>
          <MyIdCard areas={areas} />
          <InviteLinks areas={areas} />
        </div>
      </DialogContent>
    </Dialog>
  );
}

/* ─── Contacts list ───────────────────────────────────────────────────── */

const NOTIFY: { value: Contact["notifications"]; label: string }[] = [
  { value: "ALL", label: "All notifications" },
  { value: "MENTIONS", label: "Mentions only" },
  { value: "MUTED", label: "Muted" },
];

function ContactRow({ contact, areas }: { contact: Contact; areas: CommunicationsAreas }) {
  const router = useRouter();
  const actions = useHubActions();
  const [renaming, setRenaming] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [alias, setAlias] = useState(contact.alias ?? "");
  const open = useMutation({ mutationFn: () => openDirectChatWithContact(contact.cfMeetId), retry: false, onSuccess: (id) => router.push(chatHref(id)) });
  const connected = contact.relationship === "CONNECTED";
  const name = contactLabel(contact);
  const update = (patch: Partial<Pick<Contact, "favorite" | "notifications">> & { alias?: string | null }) => actions.updateContact.mutate({ contactId: contact.contactId, expectedVersion: contact.version, ...patch });
  const menu = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted";
  return (
    <li className="group" data-testid="contact" data-relationship={contact.relationship}>
      <div className="flex items-center gap-3 rounded-2xl px-2 py-2 transition-colors hover:bg-muted/60">
        <Avatar name={name} seed={contact.cfMeetId} size="size-11" />
        <div className="min-w-0 flex-1">
          <p className="flex items-center gap-1.5 truncate font-semibold">
            <span className="truncate">{name}</span>
            {contact.notifications === "MUTED" ? <BellOff className="size-3.5 shrink-0 text-muted-foreground" aria-label="Muted" /> : null}
          </p>
          <p className="truncate text-sm text-muted-foreground">{contact.alias ? contact.displayName : shortId(contact.cfMeetId)}</p>
        </div>
        <div className="flex shrink-0 items-center">
          {areas.managedMessaging && connected ? <button type="button" data-testid="contact-message" disabled={open.isPending} onClick={() => open.mutate()} aria-label={`Message ${name}`} title="Message" className={iconBtn}><MessageSquare className="size-[18px]" /></button> : null}
          {areas.calling && connected ? (
            <>
              <Link href={callHref({ contactId: contact.contactId }, "VOICE")} data-testid="contact-call" aria-label={`Call ${name}`} title="Voice call" className={cn(iconBtn, "max-sm:hidden")}><Phone className="size-[18px]" /></Link>
              <Link href={callHref({ contactId: contact.contactId }, "VIDEO")} aria-label={`Video call ${name}`} title="Video call" className={cn(iconBtn, "max-sm:hidden")}><Video className="size-[18px]" /></Link>
            </>
          ) : null}
          <button type="button" aria-pressed={contact.favorite} aria-label={contact.favorite ? "Remove from favorites" : "Add to favorites"} title={contact.favorite ? "Favorite" : "Add to favorites"} disabled={actions.updateContact.isPending} onClick={() => update({ favorite: !contact.favorite })} className={cn(iconBtn, contact.favorite && "text-amber-500 hover:text-amber-500")}>
            <Star className={cn("size-[18px]", contact.favorite && "fill-current")} />
          </button>
          <Popover.Root>
            <Popover.Trigger asChild><button type="button" aria-label={`More for ${name}`} className={iconBtn}><MoreHorizontal className="size-[18px]" /></button></Popover.Trigger>
            <Popover.Portal>
              <Popover.Content align="end" sideOffset={4} collisionPadding={12} className="z-50 w-60 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                {areas.calling && connected ? (
                  <div className="sm:hidden">
                    <Link href={callHref({ contactId: contact.contactId }, "VOICE")} className={menu}><Phone className="size-4 text-muted-foreground" />Voice call</Link>
                    <Link href={callHref({ contactId: contact.contactId }, "VIDEO")} className={menu}><Video className="size-4 text-muted-foreground" />Video call</Link>
                  </div>
                ) : null}
                <Popover.Close asChild><button type="button" className={menu} onClick={() => setRenaming(true)}><Pencil className="size-4 text-muted-foreground" />Edit nickname</button></Popover.Close>
                <p className="px-3 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Notifications</p>
                {NOTIFY.map((n) => (
                  <Popover.Close asChild key={n.value}>
                    <button type="button" className={menu} onClick={() => update({ notifications: n.value })}>
                      {n.value === "MUTED" ? <BellOff className="size-4 text-muted-foreground" /> : <Bell className="size-4 text-muted-foreground" />}<span className="flex-1">{n.label}</span>{contact.notifications === n.value ? <Check className="size-4 text-primary" /> : null}
                    </button>
                  </Popover.Close>
                ))}
                <div className="my-1 border-t border-border" />
                <Popover.Close asChild><button type="button" className={menu} data-testid="contact-block" onClick={() => actions.block.mutate(contact.cfMeetId)}><Ban className="size-4 text-muted-foreground" />Block</button></Popover.Close>
                <Popover.Close asChild><button type="button" className={cn(menu, "text-destructive")} onClick={() => setRemoving(true)}><UserMinus className="size-4" />Remove contact</button></Popover.Close>
              </Popover.Content>
            </Popover.Portal>
          </Popover.Root>
        </div>
      </div>
      <div className="px-2"><ErrorText error={open.error ?? actions.updateContact.error ?? actions.block.error ?? actions.removeContact.error} /></div>

      <Dialog open={renaming} onOpenChange={setRenaming}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader className="text-left"><DialogTitle>Nickname</DialogTitle><DialogDescription>Only you see this. Their name stays {contact.displayName}.</DialogDescription></DialogHeader>
          <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); update({ alias: alias.trim() || null }); setRenaming(false); }}>
            <input autoFocus aria-label="Nickname (only you see it)" placeholder={contact.displayName} maxLength={64} value={alias} onChange={(e) => setAlias(e.target.value)} className="h-11 w-full rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary" />
            <div className="flex justify-end gap-2">
              <button type="button" onClick={() => setRenaming(false)} className={cn(pill, "text-foreground hover:bg-muted")}>Cancel</button>
              <button type="submit" disabled={actions.updateContact.isPending} className={cn(pill, "bg-primary text-primary-foreground hover:bg-primary/90")}>Save</button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
      <Dialog open={removing} onOpenChange={setRemoving}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader className="text-left"><DialogTitle>Remove {name}?</DialogTitle><DialogDescription>They won&apos;t be told. You can connect again later.</DialogDescription></DialogHeader>
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => setRemoving(false)} className={cn(pill, "text-foreground hover:bg-muted")}>Cancel</button>
            <button type="button" data-testid="contact-remove" disabled={actions.removeContact.isPending} onClick={() => actions.removeContact.mutate(contact.contactId, { onSuccess: () => setRemoving(false) })} className={cn(pill, "bg-destructive text-white hover:bg-destructive/90")}>Remove</button>
          </div>
        </DialogContent>
      </Dialog>
    </li>
  );
}

function ConnectionsTab({ areas, onAdd }: { areas: CommunicationsAreas; onAdd: () => void }) {
  const contacts = useHubContacts(areas.contacts);
  const [filter, setFilter] = useState("");
  const list = useMemo(() => {
    const needle = filter.trim().toLocaleLowerCase();
    return connectedContacts(contacts.data ?? [])
      .filter((c) => !needle || contactLabel(c).toLocaleLowerCase().includes(needle) || c.cfMeetId.toLowerCase().includes(needle))
      .sort((a, b) => contactLabel(a).localeCompare(contactLabel(b)));
  }, [contacts.data, filter]);
  const favorites = list.filter((c) => c.favorite);
  const groups = useMemo(() => {
    const map = new Map<string, Contact[]>();
    for (const c of list) {
      if (c.favorite && !filter.trim()) continue;
      const letter = (contactLabel(c).trim()[0] ?? "#").toUpperCase();
      const key = /[A-Z]/.test(letter) ? letter : "#";
      map.set(key, [...(map.get(key) ?? []), c]);
    }
    return [...map.entries()];
  }, [list, filter]);
  return (
    <div className="space-y-4">
      <label className="flex h-11 max-w-md items-center gap-2 rounded-full bg-muted px-4 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
        <Search className="size-4 shrink-0" />
        <input aria-label="Search contacts" placeholder="Search contacts" value={filter} onChange={(e) => setFilter(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
      </label>
      {contacts.isPending ? <div className="animate-pulse space-y-1">{[0, 1, 2, 3].map((i) => <div key={i} className="flex items-center gap-3 px-2 py-2.5"><span className="size-11 shrink-0 rounded-full bg-muted" /><span className="flex-1 space-y-2"><span className="block h-3.5 rounded-full bg-muted" style={{ width: `${40 - i * 5}%` }} /><span className="block h-3 w-1/4 rounded-full bg-muted/70" /></span></div>)}</div> : <ErrorText error={contacts.error} retry={() => void contacts.refetch()} />}
      {contacts.isSuccess && !list.length ? (
        filter ? <p className="py-10 text-center text-sm text-muted-foreground">No contacts match.</p> : (
          <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-14 text-center">
            <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Users className="size-6" /></span>
            <p className="mt-4 font-semibold">No contacts yet</p>
            <p className="mt-1 max-w-sm text-sm text-muted-foreground">Share your CF Meet ID or an invite link, or find someone by their ID.</p>
            <button type="button" onClick={onAdd} className={cn(pill, "mt-5 h-10 bg-primary px-5 text-primary-foreground hover:bg-primary/90")}><UserPlus className="size-4" />Add contact</button>
          </div>
        )
      ) : null}
      <div data-testid="contact-list" className="space-y-5">
        {favorites.length > 0 && !filter && (
          <section>
            <p className="mb-1 flex items-center gap-1.5 px-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground"><Star className="size-3.5 fill-amber-500 text-amber-500" />Favorites</p>
            <ul>{favorites.map((c) => <ContactRow key={`f-${c.contactId}`} contact={c} areas={areas} />)}</ul>
          </section>
        )}
        {groups.map(([letter, people]) => (
          <section key={letter}>
            <p className="mb-1 px-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{letter}</p>
            <ul>{people.map((c) => <ContactRow key={c.contactId} contact={c} areas={areas} />)}</ul>
          </section>
        ))}
      </div>
    </div>
  );
}

function RequestsTab({ areas }: { areas: CommunicationsAreas }) {
  const { incoming, outgoing } = useHubRequests(areas.contacts);
  const actions = useHubActions();
  const pendingIn = (incoming.data?.data ?? []).filter((r) => r.state === "PENDING");
  const pendingOut = (outgoing.data?.data ?? []).filter((r) => r.state === "PENDING");
  return (
    <div className="space-y-8">
      <section aria-label="Incoming requests">
        <h3 className="mb-2 px-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Waiting for you · {pendingIn.length}</h3>
        {incoming.isPending ? <Spinner /> : <ErrorText error={incoming.error} retry={() => void incoming.refetch()} />}
        {incoming.isSuccess && !pendingIn.length ? <p className="px-2 py-4 text-sm text-muted-foreground">No new requests.</p> : null}
        <ul className="space-y-1">
          {pendingIn.map((r) => (
            <li key={r.requestId} className="flex flex-wrap items-center gap-3 rounded-2xl px-2 py-2 hover:bg-muted/60" data-testid="incoming-request">
              <Avatar name={r.cfMeetId} seed={r.cfMeetId} size="size-11" />
              <span className="min-w-0 flex-1"><span className="block truncate font-mono text-sm font-semibold">{r.cfMeetId}</span><span className="block text-xs text-muted-foreground">Wants to connect</span></span>
              <span className="flex items-center gap-1.5">
                <button type="button" data-testid="decline-request" disabled={actions.decline.isPending} onClick={() => actions.decline.mutate({ requestId: r.requestId, version: r.version })} className={cn(pill, "text-foreground ring-1 ring-border hover:bg-muted")}>Decline</button>
                <button type="button" data-testid="accept-request" disabled={actions.accept.isPending} onClick={() => actions.accept.mutate({ requestId: r.requestId, version: r.version })} className={cn(pill, "bg-primary text-primary-foreground hover:bg-primary/90")}>Accept</button>
                <button type="button" aria-label="Block" title="Block" onClick={() => actions.block.mutate(r.cfMeetId)} className={iconBtn}><Ban className="size-4" /></button>
              </span>
            </li>
          ))}
        </ul>
        <ErrorText error={actions.accept.error ?? actions.decline.error} />
      </section>
      <section aria-label="Sent requests">
        <h3 className="mb-2 px-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">You sent · {pendingOut.length}</h3>
        {outgoing.isPending ? <Spinner /> : <ErrorText error={outgoing.error} retry={() => void outgoing.refetch()} />}
        {outgoing.isSuccess && !pendingOut.length ? <p className="px-2 py-4 text-sm text-muted-foreground">Nothing waiting for an answer.</p> : null}
        <ul className="space-y-1">
          {pendingOut.map((r) => (
            <li key={r.requestId} className="flex items-center gap-3 rounded-2xl px-2 py-2 hover:bg-muted/60" data-testid="outgoing-request">
              <Avatar name={r.cfMeetId} seed={r.cfMeetId} size="size-11" />
              <span className="min-w-0 flex-1"><span className="block truncate font-mono text-sm font-semibold">{r.cfMeetId}</span><span className="block text-xs text-muted-foreground">Waiting for them to accept</span></span>
              <button type="button" disabled={actions.cancel.isPending} onClick={() => actions.cancel.mutate({ requestId: r.requestId, version: r.version })} className={cn(pill, "text-muted-foreground hover:bg-muted hover:text-foreground")}>Withdraw</button>
            </li>
          ))}
        </ul>
        <ErrorText error={actions.cancel.error} />
      </section>
    </div>
  );
}

function BlockedTab({ areas }: { areas: CommunicationsAreas }) {
  const blocks = useHubBlocks(areas.contacts);
  const actions = useHubActions();
  return (
    <div className="space-y-3">
      <p className="px-2 text-sm text-muted-foreground">Blocked people can&apos;t call you, message you or send requests. They aren&apos;t told.</p>
      {blocks.isPending ? <Spinner /> : <ErrorText error={blocks.error} retry={() => void blocks.refetch()} />}
      {blocks.isSuccess && !blocks.data.data.length ? (
        <div className="flex flex-col items-center px-6 py-14 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Ban className="size-6" /></span>
          <p className="mt-4 font-semibold">Nobody is blocked</p>
        </div>
      ) : null}
      <ul className="space-y-1">
        {(blocks.data?.data ?? []).map((b) => (
          <li key={b.cfMeetId} className="flex items-center gap-3 rounded-2xl px-2 py-2 hover:bg-muted/60" data-testid="blocked">
            <span className="flex size-11 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"><Ban className="size-5" /></span>
            <span className="min-w-0 flex-1 truncate font-mono text-sm">{b.cfMeetId}</span>
            <button type="button" data-testid="unblock" disabled={actions.unblock.isPending} onClick={() => actions.unblock.mutate(b.cfMeetId)} className={cn(pill, "text-foreground ring-1 ring-border hover:bg-muted")}>Unblock</button>
          </li>
        ))}
      </ul>
      <ErrorText error={actions.unblock.error} />
    </div>
  );
}

const PRIVACY_FIELDS: { key: keyof Privacy; label: string; options: { value: string; label: string }[] }[] = [
  { key: "contactRequestPermissions", label: "Who can send you requests", options: [{ value: "EVERYONE", label: "Everyone" }, { value: "NOBODY", label: "Nobody" }] },
  { key: "incomingCallPolicy", label: "Who can call you", options: [{ value: "CONNECTIONS", label: "Your contacts" }, { value: "NOBODY", label: "Nobody" }] },
  { key: "directChatPolicy", label: "Who can message you", options: [{ value: "CONNECTIONS", label: "Your contacts" }, { value: "NOBODY", label: "Nobody" }] },
  { key: "publicProfileVisibility", label: "Who can see your profile", options: [{ value: "PUBLIC", label: "Anyone with your ID" }, { value: "CONNECTIONS", label: "Your contacts" }, { value: "HIDDEN", label: "Nobody" }] },
  { key: "presenceVisibility", label: "Who can see when you're online", options: [{ value: "CONNECTIONS", label: "Your contacts" }, { value: "NOBODY", label: "Nobody" }] },
];

function Switch({ checked, onChange, label, hint }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string }) {
  return (
    <button type="button" role="switch" aria-checked={checked} onClick={() => onChange(!checked)} className="flex w-full items-center gap-3 py-1 text-left">
      <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{label}</span>{hint ? <span className="block text-xs text-muted-foreground">{hint}</span> : null}</span>
      <span className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", checked ? "bg-primary" : "bg-muted-foreground/30")}><span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-[left]", checked ? "left-[18px]" : "left-0.5")} /></span>
    </button>
  );
}

function PrivacyDialog({ open, onOpenChange, areas }: { open: boolean; onOpenChange: (v: boolean) => void; areas: CommunicationsAreas }) {
  const privacy = useHubPrivacy(areas.contacts);
  const actions = useHubActions();
  const [draft, setDraft] = useState<Privacy | null>(null);
  const value = draft ?? privacy.data?.privacy ?? null;
  const close = (v: boolean) => { if (!v) setDraft(null); onOpenChange(v); };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-md">
        <DialogHeader className="text-left"><DialogTitle>Privacy</DialogTitle><DialogDescription>Choose who can find and reach you.</DialogDescription></DialogHeader>
        {privacy.isPending ? <div className="flex justify-center py-8"><Spinner /></div> : !value ? <ErrorText error={privacy.error} retry={() => void privacy.refetch()} /> : (
          <form className="space-y-4" data-testid="privacy-form" onSubmit={(e) => {
            e.preventDefault();
            if (privacy.data && draft) actions.updatePrivacy.mutate({ version: privacy.data.version, privacy: draft }, { onSuccess: () => { setDraft(null); onOpenChange(false); } });
          }}>
            <div className="space-y-2 rounded-xl bg-muted/50 p-3">
              <Switch checked={value.publicIdDiscoverable} onChange={(v) => setDraft({ ...value, publicIdDiscoverable: v })} label="People can find me by my ID" hint="Turn off to stay hidden from ID search" />
              <Switch checked={value.invitationPermissions === "ENABLED"} onChange={(v) => setDraft({ ...value, invitationPermissions: v ? "ENABLED" : "DISABLED" } as Privacy)} label="Invite links and QR codes" hint="Let people connect with a link or scan" />
            </div>
            {PRIVACY_FIELDS.map((f) => (
              <NiceSelect key={f.key} label={f.label} value={String(value[f.key])} onChange={(v) => setDraft({ ...value, [f.key]: v } as Privacy)} options={f.options} />
            ))}
            <ErrorText error={actions.updatePrivacy.error} />
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => close(false)} className={cn(pill, "text-foreground hover:bg-muted")}>Cancel</button>
              <button type="submit" disabled={!draft || actions.updatePrivacy.isPending} data-testid="save-privacy" className={cn(pill, "bg-primary text-primary-foreground hover:bg-primary/90")}>{actions.updatePrivacy.isPending ? <Spinner /> : null}Save</button>
            </div>
          </form>
        )}
      </DialogContent>
    </Dialog>
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
    <div className="space-y-4">
      <p className="px-2 text-sm text-muted-foreground">People in {organizationName}. They aren&apos;t personal contacts, and work chats with them can be read by CF Meet.</p>
      <label className="flex h-11 max-w-md items-center gap-2 rounded-full bg-muted px-4 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
        <Search className="size-4 shrink-0" />
        <input aria-label="Search colleagues" placeholder="Search colleagues" maxLength={64} value={search} onChange={(e) => setSearch(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
      </label>
      {directory.isPending || members.isPending ? <Spinner /> : <ErrorText error={directory.error ?? members.error} />}
      <ul className="space-y-1" data-testid="colleague-list">
        {people.map((p) => (
          <li key={p.userId} className="flex items-center gap-3 rounded-2xl px-2 py-2 hover:bg-muted/60" data-testid="org-colleague">
            <Avatar name={p.displayName} seed={p.userId} size="size-11" />
            <span className="min-w-0 flex-1 truncate font-semibold">{p.displayName}</span>
            <button type="button" disabled={open.isPending} onClick={() => open.mutate(p.userId)} aria-label={`Message ${p.displayName}`} title="Message" className={iconBtn}><MessageSquare className="size-[18px]" /></button>
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
  const contacts = useHubContacts(areas.contacts);
  const pendingCount = (incoming.data?.data ?? []).filter((r) => r.state === "PENDING").length;
  const contactCount = connectedContacts(contacts.data ?? []).length;
  const organization = workspace.kind === "ORGANIZATION" && workspace.organizationId ? { id: workspace.organizationId, name: workspace.workspace?.displayName ?? "this organization" } : null;
  const [tab, setTab] = useState<Tab>("connections");
  const [adding, setAdding] = useState(false);
  const [privacyOpen, setPrivacyOpen] = useState(false);
  const tabs: [Tab, string, number | null][] = [
    ["connections", "Contacts", contacts.isSuccess ? contactCount : null],
    ["requests", "Requests", pendingCount || null],
    ["blocked", "Blocked", null],
    ...(organization ? ([["colleagues", "Colleagues", null]] as [Tab, string, number | null][]) : []),
  ];
  const unavailable = capabilities.error
    ? (isUnavailable(capabilities.error) ? "Personal contacts aren't available here yet." : communicationsErrorMessage(capabilities.error))
    : !capabilities.isPending && !areas.contacts ? `Personal contacts aren't available for this account${areas.reason ? ` (${areas.reason.toLowerCase().replace(/_/g, " ")})` : ""}.` : null;

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8" data-testid="contacts-page" data-workspace={workspace.kind}>
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Contacts</h1>
          <p className="mt-0.5 text-sm text-muted-foreground">People you&apos;re connected with on CF Meet{organization ? `, plus colleagues in ${organization.name}` : ""}.</p>
        </div>
        {!unavailable && !capabilities.isPending ? (
          <div className="flex items-center gap-1">
            <button type="button" aria-label="Privacy settings" title="Privacy" onClick={() => setPrivacyOpen(true)} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><Settings2 className="size-5" /></button>
            <button type="button" data-testid="tab-find" onClick={() => setAdding(true)} className={cn(pill, "h-10 bg-primary pl-3.5 pr-4 text-primary-foreground shadow-sm hover:bg-primary/90")}><UserPlus className="size-4" />Add contact</button>
          </div>
        ) : null}
      </header>

      {capabilities.isPending ? (
        <div role="status" aria-label="Loading contacts" className="mt-5 animate-pulse">
          <div className="flex gap-4 border-b border-border pb-3">{[20, 24, 16].map((w, i) => <span key={i} className="h-4 rounded-full bg-muted" style={{ width: `${w * 4}px` }} />)}</div>
          <div className="mt-5 h-11 max-w-md rounded-full bg-muted" />
          <ul className="mt-5 space-y-1">
            {[0, 1, 2, 3, 4].map((i) => (
              <li key={i} className="flex items-center gap-3 px-2 py-2.5">
                <span className="size-11 shrink-0 rounded-full bg-muted" />
                <span className="flex-1 space-y-2"><span className="block h-3.5 rounded-full bg-muted" style={{ width: `${40 - i * 4}%` }} /><span className="block h-3 w-1/4 rounded-full bg-muted/70" /></span>
              </li>
            ))}
          </ul>
        </div>
      ) : unavailable ? (
        <div className="mt-6 space-y-6" data-testid="contacts-unavailable">
          <div className="flex items-center gap-3 rounded-2xl bg-muted/60 p-4 text-sm"><Users className="size-5 shrink-0 text-muted-foreground" />{unavailable}</div>
          {organization ? <ColleaguesTab organizationId={organization.id} organizationName={organization.name} /> : null}
        </div>
      ) : (
        <>
          <nav className="no-scrollbar mt-5 flex gap-1 overflow-x-auto border-b border-border" role="tablist" aria-label="Contacts sections">
            {tabs.map(([value, label, count]) => (
              <button key={value} type="button" role="tab" aria-selected={tab === value} data-testid={`tab-${value}`} onClick={() => setTab(value)}
                className={cn("-mb-px inline-flex h-11 shrink-0 items-center gap-2 border-b-2 px-3 text-sm font-medium transition-colors", tab === value ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>
                {label}
                {count !== null ? <span className={cn("rounded-full px-1.5 text-xs tabular-nums", value === "requests" ? "bg-primary text-primary-foreground" : "bg-muted text-muted-foreground")}>{count}</span> : null}
              </button>
            ))}
          </nav>
          <div role="tabpanel" className="pt-5">
            {tab === "connections" ? <ConnectionsTab areas={areas} onAdd={() => setAdding(true)} /> : null}
            {tab === "requests" ? <RequestsTab areas={areas} /> : null}
            {tab === "blocked" ? <BlockedTab areas={areas} /> : null}
            {tab === "colleagues" && organization ? <ColleaguesTab organizationId={organization.id} organizationName={organization.name} /> : null}
          </div>
          <AddContactDialog open={adding} onOpenChange={setAdding} areas={areas} />
          <PrivacyDialog open={privacyOpen} onOpenChange={setPrivacyOpen} areas={areas} />
        </>
      )}
    </div>
  );
}

"use client";

/**
 * CF Meet Messages (Part V Milestone B): conversations of the signed-in account, with the
 * truth about each one's protection. PRIVATE_E2EE conversations are encrypted on this
 * device (the server cannot read them); MANAGED conversations are readable by CF Meet's
 * servers and say so. Message text is rendered only as text (React escapes it).
 */
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, BellOff, Check, Lock, LockOpen, LogOut, MoreHorizontal, Paperclip, Phone, Pin, Search, Send, ShieldCheck, ShieldAlert, Video, Wifi, WifiOff, Smartphone, History, UserPlus } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { MessagingEngine, type ConversationView, type MessageView, type SearchHit, type SecurityProfile } from "@/lib/messaging/engine";
import { mergeTimeline, type TimelineCard } from "@/lib/messaging/managed-store";
import { useMessagingDirectory, useMessagingDevices, useRegisterMessagingDevice, useRemoveCurrentMessagingDevice } from "@/hooks/v2/useMessaging";
import { hubKeys, useConversationContext, useConversationTimeline, useHubCapabilities, useHubContacts, useHubInbox } from "@/hooks/v2/useCommunications";
import { useWorkspace } from "@/context/WorkspaceContext";
import { organizationsService } from "@/services/v2/organizations.service";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { communicationsErrorMessage } from "@/lib/communications/errors";
import {
  callHref,
  colleaguesOfOrganization,
  communicationsAreas,
  connectedContacts,
  contactLabel,
  conversationPeopleSource,
  formatDuration,
  meetingJoinHref,
  meetingShareLine,
  normalizeCfMeetId,
} from "@/lib/communications/comms-core";
import type { TimelineCallCard, TimelineMeetingCard } from "@/types/communications";
import MessagingDevicesPanel from "./MessagingDevicesPanel";

function useEngine(): MessagingEngine {
  // One engine per mounted page (lazy state initializer, never re-created on render).
  const [engine] = useState(() => new MessagingEngine());
  useEffect(() => {
    void engine.start();
    return () => engine.stop();
  }, [engine]);
  return engine;
}

function ProfileBadge({ profile }: { profile: SecurityProfile }) {
  return profile === "PRIVATE_E2EE" ? (
    <span data-testid="profile-badge" data-profile={profile} className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:text-emerald-400">
      <Lock className="h-3 w-3" /> End-to-end encrypted
    </span>
  ) : (
    <span data-testid="profile-badge" data-profile={profile} className="inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-xs font-medium text-amber-700 dark:text-amber-400">
      <LockOpen className="h-3 w-3" /> Managed — readable by CF Meet servers
    </span>
  );
}

function StatusBar({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const connected = s.connection === "connected";
  return (
    <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground" data-testid="messaging-status">
      <span className="inline-flex items-center gap-1" data-testid="device-state" data-phase={s.phase}>
        <Smartphone className="h-3.5 w-3.5" />
        {s.phase === "ready" ? "This browser is a secure messaging device" : s.phase === "needs-device" ? "No secure device in this browser" : s.phase === "registering" ? "Setting up this device…" : s.phase}
      </span>
      <span className="inline-flex items-center gap-1" data-testid="connection-state" data-connection={s.connection}>
        {connected ? <Wifi className="h-3.5 w-3.5" /> : <WifiOff className="h-3.5 w-3.5" />}
        {s.sessionsPaused ? "Live updates paused" : connected ? "Live" : s.connection === "connecting" ? "Connecting…" : s.connection === "offline" ? "Live updates unavailable (refreshing periodically)" : "Reconnecting…"}
      </span>
      <span className="inline-flex items-center gap-1" data-testid="kt-state">
        {s.ktPinned ? <ShieldCheck className="h-3.5 w-3.5" /> : <ShieldAlert className="h-3.5 w-3.5" />}
        {s.ktPinned ? "Contact keys verified against the transparency log" : "Contact keys not verified against a pinned transparency log"}
      </span>
    </div>
  );
}

// A history request from another device must be seen where the user is, not behind the
// collapsed settings toggle: the comparison code is the only check against a rogue device.
function IncomingTransfers({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [error, setError] = useState<string | null>(null);
  if (s.phase !== "ready" || s.incomingTransfers.length === 0) return null;
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (cause) { setError(messagingErrorMessage(cause)); }
  };
  return (
    <section className="space-y-2" aria-label="History requests from your other devices">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {s.incomingTransfers.map((t) => (
        <div key={t.transferId} className="space-y-2 rounded-xl border border-border bg-card p-3" data-testid="incoming-transfer">
          <p className="text-sm">Another of your devices asks for your message history. Continue only if it shows this code:</p>
          <p className="font-mono text-lg tracking-widest" data-testid="incoming-code">{t.comparisonCode}</p>
          <div className="flex gap-2">
            <Button data-testid="approve-transfer" onClick={() => void run(() => engine.approveTransfer(t.transferId))}>Codes match — send history</Button>
            <Button variant="outline" data-testid="decline-transfer" onClick={() => void run(() => engine.declineTransfer(t.transferId))}>Decline</Button>
          </div>
        </div>
      ))}
    </section>
  );
}

function DevicePanel({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [recovering, setRecovering] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const devices = useMessagingDevices(recovering ? s.identity?.principalId ?? "" : "");
  const registration = useRegisterMessagingDevice(engine);
  const removal = useRemoveCurrentMessagingDevice(engine);
  const busy = registration.isPending || removal.isPending;
  const sources = devices.data?.devices.filter((d) => d.status === "ACTIVE" && d.deviceId !== s.deviceId) ?? [];
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (cause) { setError(messagingErrorMessage(cause)); }
  };
  const [transfer, setTransfer] = useState<{ code: string; state: string } | null>(null);
  if (s.phase === "loading") return null;
  if (s.phase === "needs-device" || s.phase === "revoked" || s.phase === "registering") {
    return (
      <div className="rounded-2xl border border-border bg-card p-4 space-y-2" data-testid="device-setup">
        {s.detail ? <p className="text-sm text-muted-foreground">{s.detail}</p> : null}
        <p className="text-sm">Set up this browser to read and send end-to-end encrypted messages. Its keys are created here and never leave this browser.</p>
        <Button
          data-testid="register-device"
          disabled={busy || s.phase === "registering"}
          onClick={() => void run(() => registration.mutateAsync())}
        >
          {s.phase === "registering" ? <Spinner /> : null} Set up secure messaging
        </Button>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      </div>
    );
  }
  if (s.phase !== "ready") {
    return (
      <div className="rounded-2xl border border-border bg-card p-4 text-sm" data-testid="messaging-unavailable">
        {s.detail ?? "Messaging is unavailable."}
      </div>
    );
  }
  return (
    <div className="rounded-2xl border border-border bg-card p-4 space-y-3">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          data-testid="recover-history"
          onClick={() => setRecovering(!recovering)}
          aria-expanded={recovering}
        >
          <History className="h-4 w-4" /> Recover history from another device
        </Button>
        <Button variant="outline" size="sm" disabled={busy} data-testid="remove-device" onClick={() => void run(() => removal.mutateAsync())}>
          Remove this device
        </Button>
      </div>
      {recovering && devices.isPending ? <p className="text-sm text-muted-foreground">Loading your other devices…</p> : null}
      {recovering && devices.error ? <p role="alert" className="text-sm text-destructive">{messagingErrorMessage(devices.error)} <button className="underline" onClick={() => void devices.refetch()}>Retry</button></p> : null}
      {recovering && devices.isSuccess && sources.length === 0 ? <p className="text-sm text-muted-foreground">No other active devices to recover history from.</p> : null}
      {recovering && sources.length > 0 && !transfer ? (
        <ul className="space-y-1">
          {sources.map((d) => (
            <li key={d.deviceId}>
              <Button
                variant="ghost"
                size="sm"
                data-testid="transfer-source"
                onClick={() => void run(async () => {
                  const r = await engine.requestHistory(d.deviceId);
                  setTransfer({ code: r.comparisonCode, state: "waiting" });
                  try {
                    const n = await engine.receiveHistory(r.transferId);
                    setTransfer({ code: r.comparisonCode, state: `received ${n} messages` });
                  } catch (e) {
                    setTransfer({ code: r.comparisonCode, state: `failed: ${e instanceof Error ? e.message : "error"}` });
                  }
                })}
              >
                {d.displayLabel ?? "Device"} · {d.deviceId.slice(0, 8)}
              </Button>
            </li>
          ))}
        </ul>
      ) : null}
      {transfer ? (
        <div className="rounded-xl border border-border p-3 text-sm" data-testid="outgoing-transfer" data-state={transfer.state}>
          <p>Confirm on your other device that it shows:</p>
          <p className="font-mono text-lg tracking-widest" data-testid="outgoing-code">{transfer.code}</p>
          <p className="text-muted-foreground">{transfer.state}</p>
        </div>
      ) : null}
    </div>
  );
}

const QUICK_REACTIONS = ["👍", "❤️", "😂", "🎉", "😮", "🙏"];

/**
 * People a new conversation can include, per the selected workspace: in an organization, that
 * organization's colleagues; in Personal, your connected contacts (a coworker is not
 * automatically a contact). When the Hub is not available in Personal, the colleague
 * directory is offered instead, labelled as such.
 */
function usePeoplePicker(enabled: boolean, search: string) {
  const workspace = useWorkspace();
  const source = conversationPeopleSource(workspace.kind);
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const contactsUsable = source === "contacts" && areas.contacts;
  const contacts = useHubContacts(enabled && contactsUsable);
  const colleaguesWanted = enabled && (source === "colleagues" || (!capabilities.isPending && !contactsUsable));
  const directory = useMessagingDirectory(search, colleaguesWanted);
  const orgId = workspace.kind === "ORGANIZATION" ? workspace.organizationId : null;
  const members = useQuery({
    queryKey: ["workspace", orgId ?? "personal", "members"],
    queryFn: () => organizationsService.listMembers(orgId!),
    enabled: colleaguesWanted && !!orgId,
    retry: false,
  });
  const colleagues = orgId ? colleaguesOfOrganization(directory.data?.people ?? [], members.data) : (directory.data?.people ?? []);
  const needle = search.trim().toLocaleLowerCase();
  const people = contactsUsable
    ? connectedContacts(contacts.data ?? [])
        .filter((c) => !needle || contactLabel(c).toLocaleLowerCase().includes(needle))
        .map((c) => ({ id: `cf:${c.cfMeetId}`, name: contactLabel(c), kind: "contact" as const, value: c.cfMeetId }))
    : colleagues.map((p) => ({ id: `u:${p.userId}`, name: p.displayName, kind: "colleague" as const, value: p.userId }));
  const pending = contactsUsable ? contacts.isPending : directory.isPending || (!!orgId && members.isPending);
  const error = contactsUsable ? contacts.error : (directory.error ?? members.error);
  const heading = contactsUsable
    ? "Your contacts"
    : orgId
      ? `Colleagues in ${workspace.workspace?.displayName ?? "this organization"}`
      : "Colleagues in your organizations";
  return { people, pending, error, heading, retry: () => void (contactsUsable ? contacts.refetch() : directory.refetch()) };
}

function NewConversation({ engine, onCreated }: { engine: MessagingEngine; onCreated: (id: string) => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const picker = usePeoplePicker(open && !!s.identity, search);
  const [chosen, setChosen] = useState<string[]>([]);
  const [title, setTitle] = useState("");
  const [profile, setProfile] = useState<SecurityProfile>("PRIVATE_E2EE");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (!open) {
    return (
      <Button size="sm" data-testid="new-conversation" disabled={!s.identity} onClick={() => setOpen(true)}>
        <UserPlus className="h-4 w-4" /> New conversation
      </Button>
    );
  }
  const group = chosen.length > 1 || !!title.trim();
  return (
    <div className="space-y-2 rounded-xl border border-border p-3" data-testid="new-conversation-form">
      <p className="text-sm font-medium" data-testid="people-source">{picker.heading}</p>
      <Input aria-label="Search people" placeholder="Search" maxLength={64} value={search} onChange={(event) => setSearch(event.target.value)} />
      {picker.pending ? <p className="text-xs text-muted-foreground">Loading…</p> : picker.error ? <p role="alert" className="text-xs text-destructive">{messagingErrorMessage(picker.error)} <button className="underline" onClick={picker.retry}>Retry</button></p> : picker.people.length === 0 ? <p className="text-xs text-muted-foreground">Nobody found.</p> : null}
      <ul className="max-h-40 space-y-1 overflow-auto">
        {picker.people.map((p) => (
          <li key={p.id}>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                data-testid="colleague"
                data-kind={p.kind}
                data-name={p.name}
                checked={chosen.includes(p.id)}
                onChange={(e) => setChosen(e.target.checked ? [...chosen, p.id] : chosen.filter((x) => x !== p.id))}
              />
              {p.name}
            </label>
          </li>
        ))}
      </ul>
      {profile === "MANAGED" ? (
        <Input aria-label="Group name (optional)" placeholder="Group name (optional)" maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} data-testid="group-title" />
      ) : null}
      <fieldset className="space-y-1 text-sm">
        <label className="flex items-center gap-2">
          <input type="radio" name="profile" data-testid="profile-e2ee" checked={profile === "PRIVATE_E2EE"} onChange={() => setProfile("PRIVATE_E2EE")} disabled={s.phase !== "ready"} />
          End-to-end encrypted (only the participants&apos; devices can read it)
        </label>
        <label className="flex items-center gap-2">
          <input type="radio" name="profile" data-testid="profile-managed" checked={profile === "MANAGED"} onChange={() => setProfile("MANAGED")} />
          Managed (CF Meet servers can read it, e.g. for compliance)
        </label>
      </fieldset>
      {error ? <p className="text-xs text-destructive" data-testid="create-error">{error}</p> : null}
      <div className="flex gap-2">
        <Button
          size="sm"
          data-testid="create-conversation"
          disabled={busy || chosen.length === 0 || (profile === "PRIVATE_E2EE" && s.phase !== "ready")}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              const userIds = chosen.filter((x) => x.startsWith("u:")).map((x) => x.slice(2));
              const cfMeetIds = chosen.filter((x) => x.startsWith("cf:")).map((x) => x.slice(3));
              onCreated(await engine.createConversation(userIds, profile, { cfMeetIds, title: group && profile === "MANAGED" ? title : undefined }));
              setOpen(false);
              setChosen([]);
              setTitle("");
            } catch (e) {
              setError(messagingErrorMessage(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          Create
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

type BubbleActions = {
  reply: () => void;
  react: (reaction: string, add: boolean) => void;
  edit: (text: string) => Promise<void>;
  remove: (scope: "EVERYONE" | "ME") => void;
  pin: (pinned: boolean) => void;
  retry: () => void;
  discard: () => void;
};

function MessageBubble({ m, name, names, onDownload, actions, managed }: { m: MessageView; name: string; names: Record<string, string>; onDownload: () => void; actions: BubbleActions; managed: boolean }) {
  const [menu, setMenu] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const live = managed && !!m.messageId && m.state === "delivered";
  return (
    <li
      data-testid="message"
      data-state={m.state}
      data-mine={m.mine ? "true" : "false"}
      data-message-id={m.messageId ?? undefined}
      className={cn("group relative max-w-[75%] rounded-2xl px-3 py-2 text-sm", m.mine ? "ml-auto bg-primary text-primary-foreground" : "bg-muted", m.state === "failed" && "border border-destructive", m.announcement && "border border-amber-500")}
    >
      {!m.mine ? <p className="text-xs font-medium opacity-70">{name}</p> : null}
      {m.pinned ? <p className="text-[10px] opacity-70" data-testid="message-pinned"><Pin className="inline h-3 w-3" /> Pinned</p> : null}
      {m.replyTo ? (
        <p className="mb-1 border-l-2 border-current pl-2 text-xs opacity-75" data-testid="message-reply">
          {m.replyTo.removed ? "Original message deleted" : `${m.replyTo.senderPrincipalId ? (names[m.replyTo.senderPrincipalId] ?? "CF Meet user") : "Message"}: ${m.replyTo.text ?? "…"}`}
        </p>
      ) : null}
      {editing !== null ? (
        <form className="flex gap-1" onSubmit={(e) => { e.preventDefault(); void actions.edit(editing).then(() => setEditing(null)); }}>
          <input className="min-w-0 flex-1 rounded bg-background px-2 text-foreground" aria-label="Edit message" value={editing} maxLength={16384} onChange={(e) => setEditing(e.target.value)} data-testid="edit-input" />
          <button type="submit" className="underline" data-testid="edit-save">Save</button>
          <button type="button" className="underline" onClick={() => setEditing(null)}>Cancel</button>
        </form>
      ) : m.text !== null ? (
        <p className="whitespace-pre-wrap break-words" data-testid="message-text">{m.text}</p>
      ) : null}
      {m.file ? (
        <button type="button" className="mt-1 inline-flex items-center gap-1 underline" data-testid="attachment" onClick={onDownload}>
          <Paperclip className="h-3.5 w-3.5" /> {m.file.name}{m.file.size ? ` (${Math.ceil(m.file.size / 1024)} KB)` : ""}
        </button>
      ) : null}
      {m.note ? <p className="text-xs italic opacity-80" data-testid="message-note">{m.note}</p> : null}
      {m.reactions?.length ? (
        <div className="mt-1 flex flex-wrap gap-1" data-testid="reactions">
          {m.reactions.map((r) => (
            <button key={r.reaction} type="button" aria-pressed={r.reactedByMe} data-testid="reaction" onClick={() => actions.react(r.reaction, !r.reactedByMe)}
              className={cn("rounded-full border px-1.5 text-xs", r.reactedByMe ? "border-current" : "border-transparent bg-background/40")}>
              {r.reaction} {r.count}
            </button>
          ))}
        </div>
      ) : null}
      <p className="mt-0.5 text-[10px] opacity-60">
        {m.state === "queued" ? "Waiting to send…" : m.state === "sending" ? "Sending…" : m.state === "failed" ? "Failed" : m.createdAt ? new Date(m.createdAt).toLocaleTimeString() : ""}
        {m.editCount ? " · edited" : ""}
        {m.transferred ? " · recovered" : ""}
        {m.seenBy?.length ? <span data-testid="seen-by"> · Seen by {m.seenBy.map((p) => names[p] ?? "CF Meet user").join(", ")}</span> : null}
      </p>
      {m.outboxKey && m.state === "failed" ? (
        <span className="mt-1 flex gap-2 text-xs">
          <button type="button" className="underline" data-testid="retry-send" onClick={actions.retry}>Retry</button>
          <button type="button" className="underline" onClick={actions.discard}>Discard</button>
        </span>
      ) : null}
      {live ? (
        <div className="mt-1 flex flex-wrap items-center gap-1 text-xs">
          <button type="button" className="opacity-70 hover:opacity-100" aria-expanded={menu} data-testid="message-actions" onClick={() => setMenu(!menu)}>
            <MoreHorizontal className="h-3.5 w-3.5" aria-label="Message actions" />
          </button>
          {menu ? (
            <span className="flex flex-wrap items-center gap-2" data-testid="message-menu">
              {QUICK_REACTIONS.map((r) => (
                <button key={r} type="button" aria-label={`React ${r}`} onClick={() => { actions.react(r, !(m.reactions ?? []).some((x) => x.reaction === r && x.reactedByMe)); setMenu(false); }}>{r}</button>
              ))}
              <button type="button" className="underline" data-testid="reply" onClick={() => { actions.reply(); setMenu(false); }}>Reply</button>
              <button type="button" className="underline" data-testid="pin" onClick={() => { actions.pin(!m.pinned); setMenu(false); }}>{m.pinned ? "Unpin" : "Pin"}</button>
              {m.mine && m.text !== null ? <button type="button" className="underline" data-testid="edit" onClick={() => { setEditing(m.text ?? ""); setMenu(false); }}>Edit</button> : null}
              {m.mine ? <button type="button" className="underline" data-testid="delete" onClick={() => { actions.remove("EVERYONE"); setMenu(false); }}>Delete for everyone</button> : null}
              <button type="button" className="underline" data-testid="hide" onClick={() => { actions.remove("ME"); setMenu(false); }}>Hide for me</button>
            </span>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

function TimelineCardView({ card }: { card: TimelineCard }) {
  if (card.kind === "MEETING_INVITATION") {
    const meeting = (card.meeting ?? {}) as TimelineMeetingCard;
    let href: string | null = null;
    try {
      href = meeting.meetingCode ? meetingJoinHref(meeting.meetingCode) : null;
    } catch {
      href = null;
    }
    return (
      <li className="mx-auto flex max-w-sm items-center gap-3 rounded-xl border border-border bg-card px-3 py-2 text-sm" data-testid="meeting-card">
        <Video className="h-4 w-4 shrink-0 text-primary" />
        <span className="min-w-0 flex-1">
          <span className="block font-medium">{meeting.origin === "INSTANT" ? "Meeting started" : "Meeting shared"}</span>
          <span className="block font-mono text-xs text-muted-foreground">{meeting.meetingCode}</span>
        </span>
        {href ? <Link className="text-primary underline" href={href} data-testid="meeting-join">Join</Link> : null}
      </li>
    );
  }
  const call = (card.call ?? {}) as TimelineCallCard;
  return (
    <li className={cn("mx-auto flex max-w-sm items-center gap-2 rounded-xl border border-border bg-card px-3 py-2 text-xs", call.missed && "text-destructive")} data-testid="call-card">
      <Phone className="h-3.5 w-3.5" />
      {call.missed ? "Missed " : ""}{call.kind === "VIDEO" ? "video" : "voice"} call{call.outcome ? ` · ${call.outcome.toLowerCase()}` : ""}{call.durationMs ? ` · ${formatDuration(call.durationMs)}` : ""}
      <time className="ml-auto opacity-70">{new Date(card.occurredAt).toLocaleTimeString()}</time>
    </li>
  );
}

function MembersPanel({ engine, conversation }: { engine: MessagingEngine; conversation: ConversationView }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [error, setError] = useState<string | null>(null);
  const [title, setTitle] = useState(conversation.serverTitle ?? "");
  const [cfMeetId, setCfMeetId] = useState("");
  const me = s.identity?.principalId;
  const manager = conversation.myRole === "OWNER" || conversation.myRole === "ADMIN";
  const run = (p: Promise<unknown>) => {
    setError(null);
    void p.catch((cause) => setError(messagingErrorMessage(cause)));
  };
  const managed = conversation.securityProfile === "MANAGED";
  const normalized = normalizeCfMeetId(cfMeetId);
  return (
    <div className="space-y-2 border-b border-border p-3 text-sm" data-testid="members">
      {managed && conversation.contextClass === "GROUP" ? (
        <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); run(engine.renameConversation(conversation.id, title)); }}>
          <Input aria-label="Group name" placeholder="Group name" maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} className="h-9" />
          <Button type="submit" size="sm" variant="outline" data-testid="rename-group">Rename</Button>
        </form>
      ) : null}
      <ul className="space-y-1">
        {conversation.members.map((p) => (
          <li key={p} className="flex items-center justify-between gap-2">
            <span>{p === me ? "You" : s.names[p] ?? "CF Meet user"} {conversation.memberRoles[p] && conversation.memberRoles[p] !== "MEMBER" ? <span className="text-xs text-muted-foreground">({conversation.memberRoles[p].toLowerCase()})</span> : null}</span>
            {p !== me ? (
              <span className="flex items-center gap-1">
                {manager && conversation.contextClass === "GROUP" ? (
                  <select aria-label="Role" className="rounded border border-input bg-background px-1 text-xs" value={conversation.memberRoles[p] ?? "MEMBER"} onChange={(e) => run(engine.setMemberRole(conversation.id, p, e.target.value))}>
                    {["ADMIN", "MODERATOR", "MEMBER", "READ_ONLY"].map((r) => <option key={r} value={r}>{r.toLowerCase().replace("_", " ")}</option>)}
                  </select>
                ) : null}
                {conversation.contextClass === "GROUP" ? (
                  <Button variant="ghost" size="sm" data-testid="remove-member" onClick={() => run(engine.removeMember(conversation.id, p))}>Remove</Button>
                ) : null}
              </span>
            ) : null}
          </li>
        ))}
      </ul>
      {conversation.contextClass === "GROUP" ? (
        <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); if (normalized) run(engine.addMemberByCfMeetId(conversation.id, normalized).then(() => setCfMeetId(""))); }}>
          <Input aria-label="Add a contact by CF Meet ID" placeholder="Add a contact by CF Meet ID" value={cfMeetId} maxLength={40} onChange={(e) => setCfMeetId(e.target.value)} className="h-9" data-testid="add-member-id" />
          <Button type="submit" size="sm" variant="outline" disabled={!normalized}>Add</Button>
        </form>
      ) : null}
      {conversation.contextClass === "GROUP" ? (
        <Button variant="ghost" size="sm" data-testid="leave-conversation" onClick={() => run(engine.leaveConversation(conversation.id))}><LogOut className="h-4 w-4" /> Leave group</Button>
      ) : null}
      {error ? <p role="alert" className="text-destructive">{error}</p> : null}
    </div>
  );
}

function MeetingActions({ engine, conversation, enabled }: { engine: MessagingEngine; conversation: ConversationView; enabled: { instant: boolean; share: boolean } }) {
  const client = useQueryClient();
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const managed = conversation.securityProfile === "MANAGED";
  const run = async (fn: () => Promise<{ meetingCode: string; origin: "INSTANT" | "SHARED" }>) => {
    setBusy(true);
    setError(null);
    try {
      const link = await fn();
      // The Hub card is the server record; a MANAGED chat also gets a client-composed line (MSG-009).
      if (managed) await engine.send(conversation.id, meetingShareLine(link.meetingCode, link.origin));
      void client.invalidateQueries({ queryKey: hubKeys.timeline(conversation.id) });
      setCode("");
    } catch (cause) {
      setError(communicationsErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };
  if (!enabled.instant && !enabled.share) return null;
  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-2 text-sm" data-testid="meeting-actions">
      {enabled.instant ? <Button size="sm" variant="outline" disabled={busy} data-testid="instant-meeting" onClick={() => void run(() => hub.instantMeeting(conversation.id))}><Video className="h-4 w-4" /> Start meeting</Button> : null}
      {enabled.share ? (
        <form className="flex items-center gap-1" onSubmit={(e) => { e.preventDefault(); if (code.trim()) void run(() => hub.shareMeeting(conversation.id, code.trim())); }}>
          <Input aria-label="Meeting code to share" placeholder="Meeting code" value={code} maxLength={64} onChange={(e) => setCode(e.target.value)} className="h-9 w-36" />
          <Button type="submit" size="sm" variant="ghost" disabled={busy || !code.trim()} data-testid="share-meeting">Share</Button>
        </form>
      ) : null}
      {error ? <p role="alert" className="text-destructive">{error}</p> : null}
    </div>
  );
}

function ConversationSearch({ engine, conversation, onClose }: { engine: MessagingEngine; conversation: ConversationView; onClose: () => void }) {
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="space-y-2 border-b border-border p-3 text-sm" data-testid="conversation-message-search">
      <form className="flex gap-2" onSubmit={(e) => {
        e.preventDefault();
        setError(null);
        void engine.search(conversation.id, q).then(setHits, (cause) => setError(messagingErrorMessage(cause)));
      }}>
        <Input aria-label="Search messages" placeholder={conversation.securityProfile === "PRIVATE_E2EE" ? "Search messages on this device" : "Search messages"} value={q} maxLength={200} onChange={(e) => setQ(e.target.value)} className="h-9" />
        <Button type="submit" size="sm" variant="outline" disabled={!q.trim()}>Search</Button>
        <Button type="button" size="sm" variant="ghost" onClick={onClose}>Close</Button>
      </form>
      {error ? <p role="alert" className="text-destructive">{error}</p> : null}
      {hits ? (
        hits.length ? (
          <ul className="max-h-40 space-y-1 overflow-auto" data-testid="search-results">
            {hits.map((h) => <li key={h.messageId} className="truncate"><time className="mr-2 text-xs text-muted-foreground">{h.createdAt ? new Date(h.createdAt).toLocaleString() : ""}</time>{h.text}</li>)}
          </ul>
        ) : <p className="text-muted-foreground">No matches.</p>
      ) : null}
    </div>
  );
}

function ConversationPane({ engine, conversation, onBack }: { engine: MessagingEngine; conversation: ConversationView; onBack?: () => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [text, setText] = useState("");
  const [members, setMembers] = useState(false);
  const [searching, setSearching] = useState(false);
  const [replyTo, setReplyTo] = useState<MessageView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const file = useRef<HTMLInputElement>(null);
  const messages = useMemo(() => s.messages[conversation.id] ?? [], [s.messages, conversation.id]);
  const e2ee = conversation.securityProfile === "PRIVATE_E2EE";
  const managed = conversation.securityProfile === "MANAGED";
  const canSend = !!s.identity && (!e2ee || (s.phase === "ready" && conversation.groupReady));
  const context = useConversationContext(conversation.id);
  const timeline = useConversationTimeline(conversation.id);
  const cleared = context.data?.clearedThroughSequence ?? timeline.data?.clearedThroughSequence ?? 0;
  const items = useMemo(
    () => mergeTimeline(messages.filter((m) => m.sequence === null || m.sequence > cleared), (timeline.data?.data ?? []) as TimelineCard[], cleared),
    [messages, timeline.data, cleared],
  );
  const counterpart = context.data?.counterpart;
  const title = conversation.contextClass === "DIRECT" && conversation.title === "CF Meet user" && counterpart ? counterpart.alias || counterpart.displayName || conversation.title : conversation.title;
  const typing = (s.typing[conversation.id] ?? []).map((p) => s.names[p] ?? (counterpart?.displayName || "Someone"));
  const pins = s.pins[conversation.id] ?? [];
  const actions = context.data?.actions ?? {};
  useEffect(() => {
    let active = true;
    void engine.openConversation(conversation.id).then(() => engine.markRead(conversation.id)).catch((cause) => {
      if (active) setError(messagingErrorMessage(cause));
    });
    return () => { active = false; };
  }, [engine, conversation.id]);
  // New messages while the conversation is open are read.
  const lastSequence = messages.at(-1)?.sequence ?? null;
  useEffect(() => {
    if (lastSequence !== null && typeof document !== "undefined" && document.visibilityState === "visible") void engine.markRead(conversation.id);
  }, [engine, conversation.id, lastSequence]);
  const download = useCallback(
    async (m: MessageView) => {
      const { blob, name } = await engine.download(conversation.id, m);
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = name;
      a.rel = "noopener";
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
    },
    [engine, conversation.id],
  );
  const fail = (cause: unknown) => setError(messagingErrorMessage(cause));
  const bubbleActions = (m: MessageView): BubbleActions => ({
    reply: () => setReplyTo(m),
    react: (reaction, add) => { if (m.messageId) void engine.react(conversation.id, m.messageId, reaction, add).catch(fail); },
    edit: async (t) => { if (m.messageId && t.trim()) await engine.editMessage(conversation.id, m.messageId, t).catch(fail); },
    remove: (scope) => { if (m.messageId) void engine.deleteMessage(conversation.id, m.messageId, scope).catch(fail); },
    pin: (pinned) => { if (m.messageId) void engine.pin(conversation.id, m.messageId, pinned).catch(fail); },
    retry: () => { if (m.outboxKey) engine.retrySend(m.outboxKey); },
    discard: () => { if (m.outboxKey) engine.discardSend(m.outboxKey); },
  });
  const mentions = (t: string) =>
    conversation.members.filter((p) => p !== s.identity?.principalId && s.names[p] && t.includes(`@${s.names[p]}`));
  return (
    <section className="flex h-full flex-col" data-testid="conversation" data-conversation-id={conversation.id} data-profile={conversation.securityProfile}>
      <header className="flex min-h-[4.5rem] items-center justify-between gap-3 border-b border-border px-4 py-3">
        <div className="flex min-w-0 items-center gap-3">
          {onBack ? (
            <Button type="button" variant="ghost" size="icon" className="shrink-0 md:hidden" onClick={onBack} aria-label="Back to conversations" data-testid="back-to-conversations">
              <ArrowLeft className="h-4 w-4" />
            </Button>
          ) : null}
          <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary" aria-hidden="true">
            {title.trim().slice(0, 1).toUpperCase() || "?"}
          </div>
          <div className="min-w-0">
            <h2 className="truncate text-sm font-semibold" data-testid="conversation-title">{title}</h2>
            <div className="mt-1"><ProfileBadge profile={conversation.securityProfile} /></div>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {actions.voiceCall ? (
            <Button asChild variant="ghost" size="sm"><Link href={callHref({ conversationId: conversation.id }, "VOICE")} aria-label="Voice call" data-testid="conversation-call"><Phone className="h-4 w-4" /></Link></Button>
          ) : null}
          {actions.videoCall ? (
            <Button asChild variant="ghost" size="sm"><Link href={callHref({ conversationId: conversation.id }, "VIDEO")} aria-label="Video call" data-testid="conversation-video-call"><Video className="h-4 w-4" /></Link></Button>
          ) : null}
          <Button variant="ghost" size="sm" onClick={() => setSearching(!searching)} aria-expanded={searching} aria-label="Search messages" data-testid="conversation-search-toggle"><Search className="h-4 w-4" /></Button>
          <Button variant="outline" size="sm" onClick={() => setMembers(!members)} aria-expanded={members} data-testid="conversation-members-toggle">
            Members
          </Button>
        </div>
      </header>
      {members ? <MembersPanel engine={engine} conversation={conversation} /> : null}
      {searching ? <ConversationSearch engine={engine} conversation={conversation} onClose={() => setSearching(false)} /> : null}
      <MeetingActions engine={engine} conversation={conversation} enabled={{ instant: !!actions.instantMeeting, share: !!actions.shareMeeting }} />
      {pins.length ? (
        <div className="border-b border-border px-3 py-1.5 text-xs text-muted-foreground" data-testid="pinned-bar">
          <Pin className="mr-1 inline h-3 w-3" /> {pins.length} pinned: {pins.slice(0, 3).map((p) => messages.find((m) => m.messageId === p.messageId)?.text ?? "message").join(" · ")}
        </div>
      ) : null}
      {e2ee && !conversation.groupReady ? (
        <p className="m-3 rounded-xl bg-muted p-3 text-sm" data-testid="e2ee-unavailable">
          {s.phase === "ready" ? "Waiting for this device to be added to the encrypted conversation." : "Set up secure messaging in this browser to read this end-to-end encrypted conversation."}
        </p>
      ) : null}
      <ol className="flex-1 space-y-2 overflow-y-auto p-3" data-testid="messages">
        {managed && s.hasOlder[conversation.id] ? (
          <li className="text-center">
            <Button variant="ghost" size="sm" data-testid="load-older" onClick={() => void engine.loadOlder(conversation.id).catch(fail)}>Load older messages</Button>
          </li>
        ) : null}
        {items.map((item) =>
          item.type === "card" ? (
            <TimelineCardView key={item.key} card={item.card} />
          ) : (
            <MessageBubble
              key={item.key}
              m={item.message}
              managed={managed}
              names={s.names}
              name={(item.message.senderPrincipalId && s.names[item.message.senderPrincipalId]) || (counterpart && !item.message.mine ? counterpart.alias || counterpart.displayName || "CF Meet user" : "CF Meet user")}
              actions={bubbleActions(item.message)}
              onDownload={() => {
                setError(null);
                void download(item.message).catch(fail);
              }}
            />
          ),
        )}
      </ol>
      {typing.length ? <p className="px-3 pb-1 text-xs italic text-muted-foreground" data-testid="typing">{typing.join(", ")} {typing.length === 1 ? "is" : "are"} typing…</p> : null}
      {error ? <p role="alert" className="px-3 pb-2 text-sm text-destructive">{error}</p> : null}
      {replyTo ? (
        <div className="flex items-center justify-between gap-2 border-t border-border px-3 py-1.5 text-xs" data-testid="reply-banner">
          <span className="truncate">Replying to {replyTo.mine ? "yourself" : (replyTo.senderPrincipalId && s.names[replyTo.senderPrincipalId]) || "message"}: {replyTo.text ?? ""}</span>
          <button type="button" className="underline" onClick={() => setReplyTo(null)}>Cancel</button>
        </div>
      ) : null}
      <form
        className="flex items-center gap-2 border-t border-border p-3"
        onSubmit={(e) => {
          e.preventDefault();
          const t = text;
          const reply = managed ? replyTo?.messageId ?? null : null;
          setText("");
          setReplyTo(null);
          setError(null);
          engine.typing(conversation.id, false);
          void engine.send(conversation.id, t, managed ? { replyToMessageId: reply, mentionedPrincipalIds: mentions(t) } : {}).catch((cause) => {
            setText((draft) => draft || t);
            setError(messagingErrorMessage(cause));
          });
        }}
      >
        <input
          ref={file}
          type="file"
          className="hidden"
          data-testid="file-input"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) {
              setError(null);
              void engine.sendFile(conversation.id, f).catch(fail);
            }
            e.target.value = "";
          }}
        />
        <Button type="button" variant="ghost" size="icon" disabled={!canSend} onClick={() => file.current?.click()} aria-label="Attach a file">
          <Paperclip className="h-4 w-4" />
        </Button>
        <Input data-testid="composer" value={text} maxLength={16384} disabled={!canSend} placeholder={canSend ? (e2ee ? "Encrypted message" : "Message (readable by CF Meet servers)") : "Unavailable on this device"} onChange={(e) => { setText(e.target.value); if (managed) engine.typing(conversation.id, e.target.value.length > 0); }} />
        <Button type="submit" size="icon" data-testid="send" disabled={!canSend || !text.trim()} aria-label="Send">
          <Send className="h-4 w-4" />
        </Button>
      </form>
    </section>
  );
}

export default function MessagesContent() {
  const engine = useEngine();
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const params = useSearchParams();
  const requested = params.get("conversation");
  const [selected, setSelected] = useState<string | null>(requested);
  const [search, setSearch] = useState("");
  const [profileFilter, setProfileFilter] = useState<"ALL" | SecurityProfile>("ALL");
  const [securityOpen, setSecurityOpen] = useState(false);
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const inbox = useHubInbox(areas.contacts || areas.managedMessaging);
  const muted = useMemo(() => new Set((inbox.data?.data ?? []).filter((e) => e.notifications === "MUTED").map((e) => e.conversationId)), [inbox.data]);
  useEffect(() => {
    // Contact -> conversation navigation: a chat created elsewhere appears after a refresh.
    if (requested && !s.conversations.some((c) => c.id === requested) && s.phase !== "loading") void engine.refreshConversations().catch(() => {});
  }, [engine, requested, s.conversations, s.phase]);
  const current = s.conversations.find((c) => c.id === selected) ?? null;
  const conversations = s.conversations.filter((conversation) => {
    const matchesSearch = conversation.title.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase());
    return matchesSearch && (profileFilter === "ALL" || conversation.securityProfile === profileFilter);
  });
  return (
    <div className="mx-auto flex h-[calc(100dvh-2rem)] min-h-[32rem] max-w-[88rem] flex-col gap-3 p-3 sm:p-5" data-testid="messages-page">
      <header className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Messages</h1>
          <p className="mt-0.5 text-sm text-muted-foreground">Your conversations, all in one place.</p>
        </div>
        <StatusBar engine={engine} />
      </header>
      <IncomingTransfers engine={engine} />
      {s.phase !== "loading" && s.phase !== "ready" ? (
        <section className="rounded-xl border border-border bg-card px-4 py-3" aria-label="Secure messaging setup">
          <p className="mb-2 text-sm font-medium">Finish setting up secure messaging</p>
          <DevicePanel engine={engine} />
        </section>
      ) : null}
      <div className="flex min-h-0 flex-1 overflow-hidden rounded-2xl border border-border bg-card shadow-sm">
        <aside className={cn("flex w-full min-h-0 flex-col border-r border-border md:w-[21rem] md:shrink-0", current && "hidden md:flex")} aria-label="Conversations">
          <div className="space-y-3 border-b border-border p-4">
            <div className="flex items-center justify-between gap-2">
              <div>
                <h2 className="text-base font-semibold">Chats</h2>
                <p className="text-xs text-muted-foreground">{s.conversations.length} {s.conversations.length === 1 ? "conversation" : "conversations"}</p>
              </div>
              <NewConversation engine={engine} onCreated={setSelected} />
            </div>
            <label className="flex h-10 items-center gap-2 rounded-lg border border-input bg-background px-3 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
              <Search className="h-4 w-4 shrink-0" />
              <input
                type="search"
                aria-label="Search conversations"
                data-testid="conversation-search"
                placeholder="Search conversations"
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
              />
            </label>
            <div className="flex gap-1" role="group" aria-label="Filter conversations">
              {([
                ["ALL", "All"],
                ["PRIVATE_E2EE", "Private"],
                ["MANAGED", "Managed"],
              ] as const).map(([value, label]) => (
                <button
                  type="button"
                  key={value}
                  aria-pressed={profileFilter === value}
                  onClick={() => setProfileFilter(value)}
                  className={cn(
                    "rounded-full px-3 py-1.5 text-xs font-medium transition-colors",
                    profileFilter === value ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-muted",
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <ul className="min-h-0 flex-1 overflow-y-auto p-2" data-testid="conversation-list">
            {s.phase === "loading" ? (
              <li className="flex items-center gap-2 px-3 py-5 text-sm text-muted-foreground"><Spinner /> Loading conversations…</li>
            ) : conversations.map((c) => {
              const latest = s.messages[c.id]?.at(-1);
              const summary = c.lastMessage;
              const preview = latest
                ? (latest.text ?? latest.file?.name ?? latest.note ?? "")
                : summary
                  ? summary.lifecycle !== "ACTIVE" ? "Message deleted" : c.securityProfile === "PRIVATE_E2EE" ? "Encrypted message" : (summary.previewText ?? (summary.attachmentCount ? "Attachment" : ""))
                  : "No messages yet";
              const at = latest?.createdAt ?? summary?.createdAt ?? null;
              const mine = latest ? latest.mine : summary?.senderPrincipalId === s.identity?.principalId;
              const unread = c.id === selected ? 0 : c.unreadCount;
              return (
                <li key={c.id}>
                  <button
                    type="button"
                    data-testid="conversation-item"
                    data-profile={c.securityProfile}
                    data-unread={unread || undefined}
                    aria-current={c.id === selected ? "true" : undefined}
                    onClick={() => setSelected(c.id)}
                    className={cn(
                      "flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left transition-colors hover:bg-muted/70",
                      c.id === selected && "bg-muted",
                    )}
                  >
                    <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary" aria-hidden="true">
                      {c.title.trim().slice(0, 1).toUpperCase() || "?"}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center justify-between gap-2">
                        <span className="flex min-w-0 items-center gap-1.5 font-medium">
                          {c.securityProfile === "PRIVATE_E2EE" ? <Lock className="h-3.5 w-3.5 shrink-0 text-emerald-600" /> : <LockOpen className="h-3.5 w-3.5 shrink-0 text-amber-600" />}
                          <span className="truncate">{c.title}</span>
                          {muted.has(c.id) ? <BellOff className="h-3 w-3 shrink-0 text-muted-foreground" aria-label="Muted" /> : null}
                        </span>
                        {at ? <time className="shrink-0 text-[10px] text-muted-foreground">{new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</time> : null}
                      </span>
                      <span className="mt-1 flex items-center justify-between gap-2">
                        <span className="truncate text-xs text-muted-foreground">{mine && preview ? "You: " : ""}{preview}</span>
                        {unread ? (
                          <span className="flex shrink-0 items-center gap-1">
                            {c.mentionCount ? <span className="rounded-full bg-amber-500 px-1.5 text-[10px] font-bold text-white" data-testid="mention-badge">@</span> : null}
                            <span className="rounded-full bg-primary px-1.5 text-[10px] font-bold text-primary-foreground" data-testid="unread-badge">{unread > 99 ? "99+" : unread}</span>
                          </span>
                        ) : latest?.mine && latest.state !== "failed" && latest.state !== "queued" ? <Check className="h-3 w-3 shrink-0 text-muted-foreground" aria-label="Sent" /> : null}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
            {s.phase !== "loading" && conversations.length === 0 ? (
              <li className="px-4 py-10 text-center">
                <p className="text-sm font-medium">{search ? "No matching conversations" : "No conversations yet"}</p>
                <p className="mt-1 text-xs text-muted-foreground">{search ? "Try another name or search term." : "Start a new chat to get the conversation going."}</p>
              </li>
            ) : null}
          </ul>
          {s.phase === "ready" ? (
            <div className="border-t border-border p-3">
              <button type="button" className="w-full text-left text-xs text-muted-foreground hover:text-foreground" onClick={() => setSecurityOpen(!securityOpen)} aria-expanded={securityOpen} aria-controls="device-security-settings" data-testid="device-settings-toggle">
                {securityOpen ? "Hide device & security settings" : "Device & security settings"}
              </button>
              {/* Stays mounted while collapsed so a history transfer in progress keeps its state. */}
              <div id="device-security-settings" className="mt-3 space-y-3" hidden={!securityOpen}><DevicePanel engine={engine} /><MessagingDevicesPanel engine={engine} /></div>
            </div>
          ) : null}
        </aside>
        <main className={cn("min-w-0 min-h-0 flex-1", !current && "hidden md:flex md:items-center md:justify-center")} data-testid="conversation-pane">
          {current ? (
            <ConversationPane key={current.id} engine={engine} conversation={current} onBack={() => setSelected(null)} />
          ) : (
            <div className="max-w-sm px-6 text-center">
              <div className="mx-auto flex h-16 w-16 items-center justify-center rounded-full bg-primary/10 text-primary"><Send className="h-7 w-7" /></div>
              <h2 className="mt-5 text-lg font-semibold">Your messages</h2>
              <p className="mt-2 text-sm text-muted-foreground">Choose a conversation to pick up where you left off, or start a new chat.</p>
            </div>
          )}
        </main>
      </div>
      {s.phase === "error" ? (
        <p className="text-sm text-destructive" data-testid="messaging-error">Messaging error: {s.detail}</p>
      ) : null}
    </div>
  );
}

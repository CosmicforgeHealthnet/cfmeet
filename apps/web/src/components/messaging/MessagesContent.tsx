"use client";

/**
 * CF Meet Messages (Part V Milestone B): conversations of the signed-in account, with the
 * truth about each one's protection. PRIVATE_E2EE conversations are encrypted on this
 * device (the server cannot read them); MANAGED conversations are readable by CF Meet's
 * servers and say so. Message text is rendered only as text (React escapes it).
 *
 * Layout: one full-height surface (WhatsApp Web / Teams): chat list on the left, the open
 * chat on the right; on phones the list and the chat are separate screens.
 */
import SavedMessages, { SAVED_KEY } from "./SavedMessages";
import ChatInfoExtras from "./ChatInfoExtras";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft, BellOff, Check, Star, CheckCheck, ChevronDown, CornerUpLeft, FileText, History, Info, Lock, LockOpen, LogOut, MessageSquarePlus,
  MoreHorizontal, Paperclip, Pencil, Phone, Pin, PinOff, Search, SendHorizontal, Shield, ShieldCheck, ShieldAlert, SmilePlus, Smartphone,
  Trash2, EyeOff, Video, WifiOff, X,
  UserPlus,
} from "lucide-react";
import { Popover } from "radix-ui";

import { Spinner } from "@/components/ui/spinner";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { MessagingEngine, type ConversationView, type MessageView, type SearchHit, type SecurityProfile } from "@/lib/messaging/engine";
import { mergeTimeline, type TimelineCard } from "@/lib/messaging/managed-store";
import { useMessagingDirectory, useMessagingDevices, useRemoveCurrentMessagingDevice } from "@/hooks/v2/useMessaging";
import { hubKeys, useConversationContext, useConversationTimeline, useHubCapabilities, useHubContacts, useHubInbox } from "@/hooks/v2/useCommunications";
import { useWorkspace } from "@/context/WorkspaceContext";
import OrganizationMessages from "@/components/messaging/OrganizationMessages";
import { organizationsService } from "@/services/v2/organizations.service";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { communicationsErrorMessage } from "@/lib/communications/errors";
import {
  attachmentAvailability,
  attachmentSizeRefusal,
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
import { authService } from "@/services/auth.service";

function useEngine(): MessagingEngine {
  // One engine per mounted page (lazy state initializer, never re-created on render).
  const [engine] = useState(() => new MessagingEngine());
  const router = useRouter();
  useEffect(() => {
    void engine.start();
    return () => engine.stop();
  }, [engine]);
  useEffect(() => {
    // A sign-in too old to register this browser: hand over to the standard sign-in flow
    // (no Messaging activation step); the login page returns here afterwards.
    let handed = false;
    const check = () => {
      if (handed || engine.getSnapshot().phase !== "needs-signin") return;
      handed = true;
      void authService.endSessionForReauthentication().finally(() => {
        router.replace("/login?reason=secure_messaging&next=%2Fdashboard%2Fmessages");
      });
    };
    check();
    return engine.subscribe(check);
  }, [engine, router]);
  return engine;
}

/* ─── Small shared pieces ─────────────────────────────────────────────── */

const AVATAR_COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (key: string) => AVATAR_COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

function Avatar({ name, seed, size = "size-11", className }: { name: string; seed?: string; size?: string; className?: string }) {
  return (
    <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white", size, className)} style={{ backgroundColor: colorFor(seed ?? name) }}>
      {initials(name)}
    </span>
  );
}

const sameDay = (a: string, b: string) => new Date(a).toDateString() === new Date(b).toDateString();
function dayLabel(iso: string) {
  const d = new Date(iso);
  const today = new Date();
  const yesterday = new Date(); yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  const withinWeek = today.getTime() - d.getTime() < 6 * 86400000;
  return d.toLocaleDateString(undefined, withinWeek ? { weekday: "long" } : { weekday: "short", day: "numeric", month: "short", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}
function listTime(iso: string) {
  const d = new Date(iso);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const yesterday = new Date(); yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  if (now.getTime() - d.getTime() < 6 * 86400000) return d.toLocaleDateString([], { weekday: "short" });
  return d.toLocaleDateString([], { day: "numeric", month: "short" });
}
const clock = (iso: string | null) => (iso ? new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : "");

function ProfileBadge({ profile }: { profile: SecurityProfile }) {
  return profile === "PRIVATE_E2EE" ? (
    <span data-testid="profile-badge" data-profile={profile} className="inline-flex items-center gap-1 text-xs text-emerald-700 dark:text-emerald-400">
      <Lock className="size-3" /> End-to-end encrypted
    </span>
  ) : (
    <span data-testid="profile-badge" data-profile={profile} className="inline-flex items-center gap-1 text-xs text-amber-700 dark:text-amber-400">
      <LockOpen className="size-3" /> Managed · CF Meet can read this chat
    </span>
  );
}

/** Technical status, shown inside "Security & devices" (not in everyone's face). */
function StatusBar({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const connected = s.connection === "connected";
  const row = "flex items-center gap-3 rounded-xl bg-muted/60 px-3 py-2.5 text-sm";
  return (
    <div className="space-y-2" data-testid="messaging-status">
      <p className={row} data-testid="device-state" data-phase={s.phase}>
        <Smartphone className="size-4 shrink-0 text-muted-foreground" />
        {s.phase === "ready" ? "This browser can send and read encrypted messages" : s.phase === "needs-device" ? "Encrypted messages aren't set up in this browser" : s.phase === "registering" ? "Setting up this browser…" : s.phase}
      </p>
      <p className={row} data-testid="connection-state" data-connection={s.connection}>
        {connected ? <span className="size-2 shrink-0 rounded-full bg-emerald-500" /> : <WifiOff className="size-4 shrink-0 text-muted-foreground" />}
        {s.sessionsPaused ? "Live updates paused" : connected ? "Connected · messages arrive instantly" : s.connection === "connecting" ? "Connecting…" : s.connection === "offline" ? "Live updates unavailable · checking every few seconds" : "Reconnecting…"}
      </p>
      <p className={row} data-testid="kt-state">
        {s.ktPinned ? <ShieldCheck className="size-4 shrink-0 text-emerald-600" /> : <ShieldAlert className="size-4 shrink-0 text-amber-600" />}
        {s.ktPinned ? "Contacts' security keys are verified" : "Contacts' security keys couldn't be verified yet"}
      </p>
    </div>
  );
}

/** Thin banner only when something is wrong with the live connection. */
function ConnectionBanner({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  if (s.phase === "loading" || s.phase === "locked" || s.connection === "connected" || s.connection === "connecting") return null;
  return (
    <p className="flex items-center justify-center gap-2 bg-amber-500/10 px-3 py-1.5 text-xs font-medium text-amber-800 dark:text-amber-300">
      <WifiOff className="size-3.5" />{s.sessionsPaused ? "Live updates paused" : s.connection === "offline" ? "Live updates unavailable. New messages may take a few seconds." : "Reconnecting…"}
    </p>
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
    <section className="space-y-2 border-b border-border bg-primary/5 px-4 py-3" aria-label="History requests from your other devices">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {s.incomingTransfers.map((t) => (
        <div key={t.transferId} className="flex flex-wrap items-center gap-3" data-testid="incoming-transfer">
          <History className="size-5 shrink-0 text-primary" />
          <p className="min-w-0 flex-1 text-sm">Another of your devices wants your message history. Only continue if it shows <span className="font-mono font-semibold tracking-widest" data-testid="incoming-code">{t.comparisonCode}</span></p>
          <div className="flex gap-2">
            <button type="button" className="h-9 rounded-full px-4 text-sm font-semibold text-foreground ring-1 ring-border hover:bg-muted" data-testid="decline-transfer" onClick={() => void run(() => engine.declineTransfer(t.transferId))}>Decline</button>
            <button type="button" className="h-9 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90" data-testid="approve-transfer" onClick={() => void run(() => engine.approveTransfer(t.transferId))}>Codes match, send</button>
          </div>
        </div>
      ))}
    </section>
  );
}

// A new browser asking to join the account must be seen where the user is: typing the code
// it shows is the check against a browser signed in with a stolen password (ADR 0060).
function IncomingLinks({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [codes, setCodes] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  if (s.phase !== "ready" || s.incomingLinks.length === 0) return null;
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch { setError("That code doesn't match the new browser. Check it and try again."); }
  };
  return (
    <section className="space-y-2 border-b border-border bg-primary/5 px-4 py-3" aria-label="New browsers asking to use your account">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {s.incomingLinks.map((l) => (
        <div key={l.linkId} className="flex flex-wrap items-center gap-3" data-testid="incoming-link">
          <Shield className="size-5 shrink-0 text-primary" />
          <p className="min-w-0 flex-1 text-sm">A new browser wants to use private messaging on your account. If it&apos;s yours, enter the code it shows.</p>
          <input aria-label="Code shown on the new browser" inputMode="numeric" maxLength={6} data-testid="incoming-link-code"
            value={codes[l.linkId] ?? ""} onChange={(e) => setCodes({ ...codes, [l.linkId]: e.target.value.replace(/\D/g, "") })}
            className="h-9 w-28 rounded-full px-3 text-center font-mono tracking-widest ring-1 ring-border" />
          <div className="flex gap-2">
            <button type="button" className="h-9 rounded-full px-4 text-sm font-semibold text-foreground ring-1 ring-border hover:bg-muted" data-testid="reject-link" onClick={() => void run(() => engine.rejectLink(l.linkId))}>Not me</button>
            <button type="button" disabled={(codes[l.linkId] ?? "").length !== 6} className="h-9 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60" data-testid="approve-link" onClick={() => void run(() => engine.approveLink(l.linkId, codes[l.linkId] ?? ""))}>Approve</button>
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
  const removal = useRemoveCurrentMessagingDevice(engine);
  const busy = removal.isPending;
  const [emailCode, setEmailCode] = useState("");
  const sources = devices.data?.devices.filter((d) => d.status === "ACTIVE" && d.deviceId !== s.deviceId) ?? [];
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (cause) { setError(messagingErrorMessage(cause)); }
  };
  const [transfer, setTransfer] = useState<{ code: string; state: string } | null>(null);
  if (s.phase === "loading") return null;
  if (s.phase === "needs-device" || s.phase === "revoked" || s.phase === "registering" || s.phase === "needs-signin") {
    // Set up automatically (no activation step). Only a new browser's verification is shown.
    return (
      <div className="flex flex-wrap items-center gap-3" data-testid="device-setup" data-linking={s.accountLink ? "true" : "false"}>
        <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-emerald-500/10 text-emerald-600"><Shield className="size-5" /></span>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold">{s.accountLink ? "Verify this browser" : "Setting up private messaging"}</p>
          <p className="text-xs text-muted-foreground">{s.detail ?? "Your encryption keys are made here and never leave this browser."}</p>
          {s.accountLink ? (
            <p className="mt-1 text-sm">Code for your other device: <span className="font-mono font-semibold tracking-widest" data-testid="account-link-code">{s.accountLink.comparisonCode}</span></p>
          ) : null}
          {s.accountLink && !s.accountLink.emailCodeSent ? (
            <button type="button" data-testid="link-email-code" className="mt-1 text-xs font-semibold text-primary underline-offset-2 hover:underline" onClick={() => void run(() => engine.sendLinkEmailCode())}>
              No other device with you? Email me a code
            </button>
          ) : null}
          {s.accountLink ? (
            <button type="button" className="ml-3 mt-1 text-xs font-semibold text-muted-foreground underline-offset-2 hover:underline" onClick={() => void run(() => engine.cancelLink())}>
              Cancel
            </button>
          ) : null}
          {s.accountLink?.emailCodeSent ? (
            <form className="mt-2 flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); void run(async () => { await engine.completeLinkWithEmailCode(emailCode); setEmailCode(""); }); }}>
              <input inputMode="numeric" autoComplete="one-time-code" maxLength={6} value={emailCode} onChange={(e) => setEmailCode(e.target.value.replace(/\D/g, ""))} placeholder="6-digit code" aria-label="Code from the email" data-testid="link-email-code-input"
                className="h-8 w-28 rounded-lg border border-border bg-background px-2 font-mono text-sm tracking-widest" />
              <button type="submit" disabled={emailCode.length !== 6} className="inline-flex h-8 items-center rounded-full bg-primary px-3 text-xs font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60">Verify</button>
            </form>
          ) : null}
        </div>
        {s.phase === "registering" ? <Spinner /> : null}
        {error ? <p role="alert" className="w-full text-sm text-destructive">{error}</p> : null}
      </div>
    );
  }
  if (s.phase !== "ready") {
    return <p className="text-sm" data-testid="messaging-unavailable">{s.detail ?? "Messaging is unavailable."}</p>;
  }
  const action = "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm hover:bg-muted disabled:opacity-50";
  return (
    <div className="space-y-1">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      <button type="button" className={action} data-testid="recover-history" onClick={() => setRecovering(!recovering)} aria-expanded={recovering}>
        <History className="size-4 text-muted-foreground" /><span className="flex-1">Bring over history from another device</span><ChevronDown className={cn("size-4 text-muted-foreground transition-transform", recovering && "rotate-180")} />
      </button>
      {recovering && devices.isPending ? <p className="px-3 text-sm text-muted-foreground">Loading your other devices…</p> : null}
      {recovering && devices.error ? <p role="alert" className="px-3 text-sm text-destructive">{messagingErrorMessage(devices.error)} <button className="underline" onClick={() => void devices.refetch()}>Retry</button></p> : null}
      {recovering && devices.isSuccess && sources.length === 0 ? <p className="px-3 text-sm text-muted-foreground">No other devices to bring history from.</p> : null}
      {recovering && sources.length > 0 && !transfer ? (
        <ul className="space-y-1 pl-10">
          {sources.map((d) => (
            <li key={d.deviceId}>
              <button
                type="button"
                className="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left text-sm hover:bg-muted"
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
                <Smartphone className="size-4 text-muted-foreground" />{d.displayLabel ?? "Device"} <span className="text-xs text-muted-foreground">· {d.deviceId.slice(0, 8)}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {transfer ? (
        <div className="ml-10 rounded-xl bg-muted/60 p-3 text-sm" data-testid="outgoing-transfer" data-state={transfer.state}>
          <p>Check your other device shows this code:</p>
          <p className="mt-1 font-mono text-xl font-semibold tracking-[0.3em]" data-testid="outgoing-code">{transfer.code}</p>
          <p className="mt-1 text-xs text-muted-foreground">{transfer.state === "waiting" ? "Waiting for approval…" : transfer.state}</p>
        </div>
      ) : null}
      <button type="button" className={cn(action, "text-destructive")} disabled={busy} data-testid="remove-device" onClick={() => void run(() => removal.mutateAsync())}>
        <LogOut className="size-4" />Remove this browser
      </button>
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

/* ─── New chat ────────────────────────────────────────────────────────── */

function NewConversation({ engine, onCreated, open, setOpen }: { engine: MessagingEngine; onCreated: (id: string) => void; open: boolean; setOpen: (v: boolean) => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [search, setSearch] = useState("");
  const picker = usePeoplePicker(open && !!s.identity, search);
  const [chosen, setChosen] = useState<string[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});
  // The messaging policy only allows private (end-to-end encrypted) chats, so there's no choice to offer.
  const profile: SecurityProfile = "PRIVATE_E2EE";
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const toggle = (id: string, name: string, on: boolean) => {
    setChosen(on ? [...chosen, id] : chosen.filter((x) => x !== id));
    setNames((n) => ({ ...n, [id]: name }));
  };
  const close = () => { setOpen(false); setChosen([]); setSearch(""); setError(null); };
  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      const userIds = chosen.filter((x) => x.startsWith("u:")).map((x) => x.slice(2));
      const cfMeetIds = chosen.filter((x) => x.startsWith("cf:")).map((x) => x.slice(3));
      onCreated(await engine.createConversation(userIds, profile, { cfMeetIds }));
      close();
    } catch (e) {
      setError(messagingErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(v) => (v ? setOpen(true) : close())}>
      <DialogContent className="gap-0 p-0 sm:max-w-md" data-testid="new-conversation-form">
        <DialogHeader className="border-b border-border px-5 pb-4 pt-5 text-left">
          <DialogTitle>New chat</DialogTitle>
          <DialogDescription data-testid="people-source">{picker.heading}</DialogDescription>
        </DialogHeader>
        <div className="space-y-3 px-5 pt-4">
          <label className="flex h-10 items-center gap-2 rounded-full bg-muted px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
            <Search className="size-4 shrink-0" />
            <input autoFocus aria-label="Search people" placeholder="Search people" maxLength={64} value={search} onChange={(e) => setSearch(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
          </label>
          {chosen.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {chosen.map((id) => (
                <button key={id} type="button" onClick={() => toggle(id, names[id] ?? "", false)} className="inline-flex h-8 items-center gap-1.5 rounded-full bg-primary/10 pl-1 pr-2.5 text-sm font-medium text-primary hover:bg-primary/15">
                  <Avatar name={names[id] ?? "?"} seed={id} size="size-6" className="text-[10px]" />{names[id]}<X className="size-3.5" />
                </button>
              ))}
            </div>
          )}
        </div>
        <ul className="no-scrollbar mt-2 max-h-64 overflow-y-auto px-3">
          {!search ? (
            <li>
              <Link href="/dashboard/contacts?add=1" onClick={close} className="flex items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/60">
                <span className="flex size-9 items-center justify-center rounded-full bg-primary text-primary-foreground"><UserPlus className="size-4" /></span>
                <span className="text-sm font-semibold">New contact</span>
              </Link>
            </li>
          ) : null}
          {picker.pending ? <li className="flex items-center gap-2 px-2 py-4 text-sm text-muted-foreground"><Spinner /> Loading…</li>
            : picker.error ? <li role="alert" className="px-2 py-4 text-sm text-destructive">{messagingErrorMessage(picker.error)} <button className="underline" onClick={picker.retry}>Retry</button></li>
              : picker.people.length === 0 ? <li className="px-2 py-6 text-center text-sm text-muted-foreground">{search ? "No one matches." : "No one to message yet."}</li> : null}
          {picker.people.map((p) => {
            const on = chosen.includes(p.id);
            return (
              <li key={p.id}>
                <label className="flex cursor-pointer items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/60">
                  <input type="checkbox" className="sr-only" data-testid="colleague" data-kind={p.kind} data-name={p.name} checked={on} onChange={(e) => toggle(p.id, p.name, e.target.checked)} />
                  <Avatar name={p.name} seed={p.id} size="size-9" />
                  <span className="min-w-0 flex-1 truncate text-sm font-medium">{p.name}</span>
                  <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-full border", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                </label>
              </li>
            );
          })}
        </ul>
        <div className="space-y-3 border-t border-border px-5 py-4">
          {s.phase === "needs-device" || s.phase === "revoked" || s.phase === "registering" || s.phase === "needs-signin" ? (
            <div className="flex items-center gap-3 rounded-xl bg-muted/60 p-3" data-testid="new-chat-setup">
              <Shield className="size-4 shrink-0 text-emerald-600" />
              <p className="min-w-0 flex-1 text-xs text-muted-foreground">{s.accountLink ? "Verify this browser (code above) to start a chat." : "Setting up private messaging in this browser…"}</p>
              {s.phase === "registering" ? <Spinner /> : null}
            </div>
          ) : (
            <p className="flex items-center gap-2 text-xs text-muted-foreground"><Lock className="size-3.5 shrink-0 text-emerald-600" />{s.phase === "ready" ? "Private · only people in the chat can read it"
              : s.phase === "loading" ? "Loading private messaging…"
              : s.phase === "locked" ? "Messages are open in another tab. Close it, or use them here from the Messages page."
              : s.phase === "unavailable" ? "New chats can't be started yet: private messaging isn't turned on for CF Meet. Your existing chats still work."
              : `Private messaging couldn't start (${s.detail ?? "unknown error"}). Reload the page to try again.`}</p>
          )}
          {error ? <p className="text-sm text-destructive" data-testid="create-error">{error}</p> : null}
          <button type="button" data-testid="create-conversation" disabled={busy || chosen.length === 0 || (profile === "PRIVATE_E2EE" && s.phase !== "ready")} onClick={() => void create()}
            className="flex h-11 w-full items-center justify-center gap-2 rounded-full bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
            {busy ? <Spinner /> : null}{chosen.length > 1 ? `Start group with ${chosen.length}` : "Start chat"}
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

/** "last seen today at 10:02" / "yesterday at …" / "12 Nov at …" (WhatsApp wording). */
function lastSeenText(iso: string) {
  const d = new Date(iso);
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const day = new Date(); day.setHours(0, 0, 0, 0);
  const diff = Math.floor((day.getTime() - new Date(d).setHours(0, 0, 0, 0)) / 86_400_000);
  if (diff <= 0) return `last seen today at ${time}`;
  if (diff === 1) return `last seen yesterday at ${time}`;
  return `last seen ${d.toLocaleDateString([], { day: "numeric", month: "short" })} at ${time}`;
}

/** WhatsApp ticks: one = sent, two = delivered, two coloured = read. */
function Ticks({ receipt, list }: { receipt: "sent" | "delivered" | "read"; list?: boolean }) {
  if (receipt === "sent") return <Check className="size-4 shrink-0" aria-label="Sent" />;
  const read = list ? "text-sky-500" : "text-[#5EEAD4] dark:text-[#0E7490]";
  return <CheckCheck className={cn("size-4 shrink-0", receipt === "read" && read)} aria-label={receipt === "read" ? "Read" : "Delivered"} />;
}

/* ─── Message bubble ──────────────────────────────────────────────────── */

type BubbleActions = {
  reply: () => void;
  react: (reaction: string, add: boolean) => void;
  edit: (text: string) => Promise<void>;
  remove: (scope: "EVERYONE" | "ME") => void;
  pin: (pinned: boolean) => void;
  /** Star / unstar (saved messages, WhatsApp). */
  star: () => void;
  starred: boolean;
  retry: () => void;
  discard: () => void;
};

function MessageBubble({ m, name, names, onDownload, actions, managed, first, last, group }: {
  m: MessageView; name: string; names: Record<string, string>; onDownload: () => void; actions: BubbleActions; managed: boolean;
  /** first / last of a run from the same sender (grouping, Messenger-style corners) */ first: boolean; last: boolean; group: boolean;
}) {
  const [menu, setMenu] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const live = managed && !!m.messageId && m.state === "delivered";
  const mine = m.mine;
  const status = m.state === "queued" ? "Waiting…" : m.state === "pending-setup" ? "Waiting for secure setup" : m.state === "sending" ? "Sending…" : m.state === "failed" ? "Not sent" : clock(m.createdAt);
  const item = "flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-muted";

  return (
    <li
      data-testid="message"
      data-state={m.state}
      data-mine={mine ? "true" : "false"}
      data-message-id={m.messageId ?? undefined}
      className={cn("group/msg flex items-end gap-2", mine ? "flex-row-reverse" : "flex-row", first ? "mt-3" : "mt-0.5")}
    >
      {!mine && group ? (last ? <Avatar name={name} seed={m.senderPrincipalId ?? name} size="size-7" className="mb-0.5 text-[10px]" /> : <span className="w-7 shrink-0" />) : null}
      <div className={cn("flex min-w-0 max-w-[min(78%,34rem)] flex-col", mine ? "items-end" : "items-start")}>
        {!mine && group && first ? <p className="mb-1 px-3 text-xs font-semibold" style={{ color: colorFor(m.senderPrincipalId ?? name) }}>{name}</p> : null}
        <div className="relative flex items-center gap-1">
          <div
            className={cn(
              "relative min-w-0 rounded-[20px] px-3.5 py-2 text-[15px] leading-snug",
              mine ? "bg-primary text-primary-foreground" : "bg-muted text-foreground",
              mine ? cn(!first && "rounded-tr-md", !last && "rounded-br-md") : cn(!first && "rounded-tl-md", !last && "rounded-bl-md"),
              m.state === "failed" && "ring-2 ring-destructive",
              m.announcement && "ring-2 ring-amber-400",
              (m.state === "queued" || m.state === "sending" || m.state === "pending-setup") && "opacity-70",
            )}
          >
            {m.pinned ? <p className="mb-0.5 flex items-center gap-1 text-[11px] font-medium opacity-75" data-testid="message-pinned"><Pin className="size-3" /> Pinned</p> : null}
            {m.replyTo ? (
              <p className={cn("mb-1.5 rounded-lg px-2.5 py-1.5 text-xs", mine ? "bg-white/15" : "bg-background/70")} data-testid="message-reply">
                {m.replyTo.removed ? <span className="italic opacity-80">Original message deleted</span> : <><span className="block font-semibold">{m.replyTo.senderPrincipalId ? (names[m.replyTo.senderPrincipalId] ?? "CF Meet user") : "Message"}</span><span className="line-clamp-2 opacity-80">{m.replyTo.text ?? "…"}</span></>}
              </p>
            ) : null}
            {editing !== null ? (
              <form className="flex min-w-56 flex-col gap-2" onSubmit={(e) => { e.preventDefault(); void actions.edit(editing).then(() => setEditing(null)); }}>
                <input autoFocus className="min-w-0 rounded-lg bg-background px-2.5 py-1.5 text-sm text-foreground outline-none" aria-label="Edit message" value={editing} maxLength={16384} onChange={(e) => setEditing(e.target.value)} data-testid="edit-input" />
                <span className="flex justify-end gap-2 text-xs font-semibold">
                  <button type="button" className="rounded-full px-2.5 py-1 hover:bg-white/15" onClick={() => setEditing(null)}>Cancel</button>
                  <button type="submit" className="rounded-full bg-background px-2.5 py-1 text-foreground" data-testid="edit-save">Save</button>
                </span>
              </form>
            ) : m.text !== null ? (
              <p className="whitespace-pre-wrap break-words" data-testid="message-text">{m.text}</p>
            ) : null}
            {m.file ? (
              <button type="button" className={cn("mt-1 flex w-full min-w-48 items-center gap-3 rounded-xl p-2 text-left", mine ? "bg-white/15 hover:bg-white/20" : "bg-background/70 hover:bg-background")} data-testid="attachment" onClick={onDownload}>
                <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-lg", mine ? "bg-white/20" : "bg-primary/10 text-primary")}><FileText className="size-4" /></span>
                <span className="min-w-0"><span className="block truncate text-sm font-medium">{m.file.name}</span>{m.file.size ? <span className="block text-xs opacity-70">{Math.ceil(m.file.size / 1024)} KB · Download</span> : null}</span>
              </button>
            ) : null}
            {m.note ? <p className="text-xs italic opacity-80" data-testid="message-note">{m.note}</p> : null}
            <p className={cn("mt-0.5 flex items-center justify-end gap-1 text-[11px] tabular-nums", mine ? "text-primary-foreground/70" : "text-muted-foreground")}>
              {m.editCount ? "edited · " : ""}{m.transferred ? "recovered · " : ""}{status}
              {mine && (m.state === "sent" || m.state === "delivered") ? <Ticks receipt={m.seenBy?.length ? "read" : m.receipt ?? "sent"} /> : null}
            </p>
          </div>

          {/* Hover actions (Teams / Slack): react, reply, more */}
          {live ? (
            <div className={cn("flex shrink-0 items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover/msg:opacity-100", menu && "opacity-100", mine ? "order-first" : "")}>
              <Popover.Root>
                <Popover.Trigger asChild><button type="button" aria-label="React" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><SmilePlus className="size-4" /></button></Popover.Trigger>
                <Popover.Portal>
                  <Popover.Content side="top" sideOffset={4} className="z-50 flex gap-0.5 rounded-full border border-border bg-popover p-1 shadow-lg">
                    {QUICK_REACTIONS.map((r) => <Popover.Close asChild key={r}><button type="button" aria-label={`React ${r}`} onClick={() => actions.react(r, !(m.reactions ?? []).some((x) => x.reaction === r && x.reactedByMe))} className="flex size-9 items-center justify-center rounded-full text-xl transition-transform hover:scale-125 hover:bg-muted">{r}</button></Popover.Close>)}
                  </Popover.Content>
                </Popover.Portal>
              </Popover.Root>
              <button type="button" aria-label="Reply" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground" onClick={actions.reply}><CornerUpLeft className="size-4" /></button>
              <Popover.Root open={menu} onOpenChange={setMenu}>
                <Popover.Trigger asChild><button type="button" aria-label="Message actions" aria-expanded={menu} data-testid="message-actions" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreHorizontal className="size-4" /></button></Popover.Trigger>
                <Popover.Portal>
                  <Popover.Content side="top" align={mine ? "end" : "start"} sideOffset={4} className="z-50 w-52 rounded-xl border border-border bg-popover p-1.5 shadow-xl" data-testid="message-menu">
                    <div className="mb-1 flex justify-between border-b border-border px-1 pb-1.5">
                      {QUICK_REACTIONS.map((r) => <button key={r} type="button" aria-label={`React ${r}`} onClick={() => { actions.react(r, !(m.reactions ?? []).some((x) => x.reaction === r && x.reactedByMe)); setMenu(false); }} className="flex size-7 items-center justify-center rounded-full text-base hover:bg-muted">{r}</button>)}
                    </div>
                    <button type="button" className={item} data-testid="reply" onClick={() => { actions.reply(); setMenu(false); }}><CornerUpLeft className="size-4 text-muted-foreground" />Reply</button>
                    <button type="button" className={item} data-testid="star" onClick={() => { actions.star(); setMenu(false); }}><Star className={cn("size-4", actions.starred ? "fill-amber-400 text-amber-400" : "text-muted-foreground")} />{actions.starred ? "Unstar" : "Star"}</button>
                    <button type="button" className={item} data-testid="pin" onClick={() => { actions.pin(!m.pinned); setMenu(false); }}>{m.pinned ? <PinOff className="size-4 text-muted-foreground" /> : <Pin className="size-4 text-muted-foreground" />}{m.pinned ? "Unpin" : "Pin"}</button>
                    {mine && m.text !== null ? <button type="button" className={item} data-testid="edit" onClick={() => { setEditing(m.text ?? ""); setMenu(false); }}><Pencil className="size-4 text-muted-foreground" />Edit</button> : null}
                    <button type="button" className={item} data-testid="hide" onClick={() => { actions.remove("ME"); setMenu(false); }}><EyeOff className="size-4 text-muted-foreground" />Delete for me</button>
                    {mine ? <button type="button" className={cn(item, "text-destructive")} data-testid="delete" onClick={() => { actions.remove("EVERYONE"); setMenu(false); }}><Trash2 className="size-4" />Delete for everyone</button> : null}
                  </Popover.Content>
                </Popover.Portal>
              </Popover.Root>
            </div>
          ) : null}
        </div>

        {m.reactions?.length ? (
          <div className={cn("-mt-1.5 flex flex-wrap gap-1 px-2", mine ? "justify-end" : "justify-start")} data-testid="reactions">
            {m.reactions.map((r) => (
              <button key={r.reaction} type="button" aria-pressed={r.reactedByMe} data-testid="reaction" onClick={() => actions.react(r.reaction, !r.reactedByMe)}
                className={cn("relative inline-flex h-6 items-center gap-1 rounded-full border px-1.5 text-xs shadow-sm", r.reactedByMe ? "border-primary/50 bg-primary/10 text-primary" : "border-border bg-background text-foreground")}>
                <span className="text-sm leading-none">{r.reaction}</span>{r.count > 1 ? <span className="tabular-nums">{r.count}</span> : null}
              </button>
            ))}
          </div>
        ) : null}
        {m.seenBy?.length ? <p className="mt-0.5 px-2 text-[11px] text-muted-foreground" data-testid="seen-by">Seen by {m.seenBy.map((p) => names[p] ?? "CF Meet user").join(", ")}</p> : null}
        {m.outboxKey && m.state === "failed" ? (
          <span className="mt-1 flex gap-1 text-xs font-semibold">
            <button type="button" className="rounded-full px-2.5 py-1 text-destructive hover:bg-destructive/10" data-testid="retry-send" onClick={actions.retry}>Try again</button>
            <button type="button" className="rounded-full px-2.5 py-1 text-muted-foreground hover:bg-muted" onClick={actions.discard}>Discard</button>
          </span>
        ) : null}
      </div>
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
      <li className="mx-auto my-3 flex w-full max-w-xs items-center gap-3 rounded-2xl border border-border bg-card p-3 text-sm shadow-sm" data-testid="meeting-card">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><Video className="size-5" /></span>
        <span className="min-w-0 flex-1">
          <span className="block font-semibold">{meeting.origin === "INSTANT" ? "Meeting started" : "Meeting shared"}</span>
          <span className="block text-xs text-muted-foreground">{meeting.meetingCode}</span>
        </span>
        {href ? <Link className="h-8 rounded-full bg-primary px-3.5 text-sm font-semibold leading-8 text-primary-foreground hover:bg-primary/90" href={href} data-testid="meeting-join">Join</Link> : null}
      </li>
    );
  }
  const call = (card.call ?? {}) as TimelineCallCard;
  return (
    <li className="my-2 flex justify-center" data-testid="call-card">
      <span className={cn("inline-flex items-center gap-2 rounded-full bg-muted px-3 py-1.5 text-xs font-medium", call.missed ? "text-destructive" : "text-muted-foreground")}>
        {call.kind === "VIDEO" ? <Video className="size-3.5" /> : <Phone className="size-3.5" />}
        {call.missed ? "Missed " : ""}{call.kind === "VIDEO" ? "video" : "voice"} call{call.durationMs ? ` · ${formatDuration(call.durationMs)}` : call.outcome && !call.missed ? ` · ${call.outcome.toLowerCase()}` : ""}
        <time className="opacity-70">· {clock(card.occurredAt)}</time>
      </span>
    </li>
  );
}

/* ─── Chat details (members, meeting, rename) ─────────────────────────── */

function DetailsPanel({ engine, conversation, title, onClose, shareEnabled }: { engine: MessagingEngine; conversation: ConversationView; title: string; onClose: () => void; shareEnabled: boolean }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState(conversation.serverTitle ?? "");
  const [cfMeetId, setCfMeetId] = useState("");
  const me = s.identity?.principalId;
  const manager = conversation.myRole === "OWNER" || conversation.myRole === "ADMIN";
  const run = (p: Promise<unknown>) => {
    setError(null);
    void p.catch((cause) => setError(messagingErrorMessage(cause)));
  };
  const managed = conversation.securityProfile === "MANAGED";
  const isGroup = conversation.contextClass === "GROUP";
  const normalized = normalizeCfMeetId(cfMeetId);
  const field = "h-10 min-w-0 flex-1 rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary";
  const small = "h-10 shrink-0 rounded-xl px-3.5 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-40";
  return (
    <aside className="absolute inset-0 z-20 flex flex-col bg-background lg:static lg:w-80 lg:shrink-0 lg:border-l lg:border-border" data-testid="members" aria-label="Chat details">
      <header className="flex h-16 shrink-0 items-center justify-between border-b border-border px-4">
        <h3 className="font-semibold">Chat info</h3>
        <button type="button" aria-label="Close chat info" onClick={onClose} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-5" /></button>
      </header>
      <div className="no-scrollbar min-h-0 flex-1 space-y-6 overflow-y-auto p-4">
        <div className="flex flex-col items-center text-center">
          <Avatar name={title} seed={conversation.id} size="size-20" className="text-2xl" />
          <p className="mt-3 text-lg font-semibold">{title}</p>
          <div className="mt-1"><ProfileBadge profile={conversation.securityProfile} /></div>
        </div>
        {managed && isGroup ? (
          <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); run(engine.renameConversation(conversation.id, name)); }}>
            <input aria-label="Group name" placeholder="Group name" maxLength={120} value={name} onChange={(e) => setName(e.target.value)} className={field} />
            <button type="submit" className={small} data-testid="rename-group">Rename</button>
          </form>
        ) : null}
        {shareEnabled ? <ShareMeeting engine={engine} conversation={conversation} /> : null}
        <ChatInfoExtras conversationId={conversation.id} direct={!isGroup} groupTitle={(id) => s.conversations.find((c) => c.id === id)?.title ?? null} />
        <section>
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{conversation.members.length} {conversation.members.length === 1 ? "member" : "members"}</p>
          <ul className="space-y-1">
            {conversation.members.map((p) => {
              const label = p === me ? "You" : s.names[p] ?? "CF Meet user";
              const role = conversation.memberRoles[p];
              return (
                <li key={p} className="flex items-center gap-3 rounded-xl px-1 py-1.5">
                  <Avatar name={label} seed={p} size="size-9" />
                  <span className="min-w-0 flex-1"><span className="block truncate text-sm font-medium">{label}</span>{role && role !== "MEMBER" ? <span className="block text-xs capitalize text-muted-foreground">{role.toLowerCase().replace("_", " ")}</span> : null}</span>
                  {p !== me && isGroup ? (
                    <span className="flex items-center gap-1">
                      {manager ? <NiceSelect className="w-32" value={role ?? "MEMBER"} onChange={(v) => run(engine.setMemberRole(conversation.id, p, v))} options={["ADMIN", "MODERATOR", "MEMBER", "READ_ONLY"].map((r) => ({ value: r, label: r === "READ_ONLY" ? "Read only" : r.charAt(0) + r.slice(1).toLowerCase() }))} /> : null}
                      <button type="button" aria-label={`Remove ${label}`} title="Remove" data-testid="remove-member" onClick={() => run(engine.removeMember(conversation.id, p))} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive"><X className="size-4" /></button>
                    </span>
                  ) : null}
                </li>
              );
            })}
          </ul>
          {isGroup ? (
            <form className="mt-3 flex gap-2" onSubmit={(e) => { e.preventDefault(); if (normalized) run(engine.addMemberByCfMeetId(conversation.id, normalized).then(() => setCfMeetId(""))); }}>
              <input aria-label="Add a contact by CF Meet ID" placeholder="Add by CF Meet ID" value={cfMeetId} maxLength={40} onChange={(e) => setCfMeetId(e.target.value)} className={field} data-testid="add-member-id" />
              <button type="submit" className={small} disabled={!normalized}>Add</button>
            </form>
          ) : null}
        </section>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
        {isGroup ? (
          <button type="button" data-testid="leave-conversation" onClick={() => run(engine.leaveConversation(conversation.id))} className="flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-sm font-medium text-destructive hover:bg-destructive/10"><LogOut className="size-4" />Leave group</button>
        ) : null}
      </div>
    </aside>
  );
}

/** Share an existing meeting's code into the chat (the "Start meeting" button lives in the header). */
function ShareMeeting({ engine, conversation }: { engine: MessagingEngine; conversation: ConversationView }) {
  const client = useQueryClient();
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const share = async () => {
    setBusy(true);
    setError(null);
    try {
      const link = await hub.shareMeeting(conversation.id, code.trim());
      if (conversation.securityProfile === "MANAGED") await engine.send(conversation.id, meetingShareLine(link.meetingCode, link.origin));
      void client.invalidateQueries({ queryKey: hubKeys.timeline(conversation.id) });
      setCode("");
    } catch (cause) {
      setError(communicationsErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section>
      <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Share a meeting</p>
      <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); if (code.trim()) void share(); }}>
        <input aria-label="Meeting code to share" placeholder="Meeting code" value={code} maxLength={64} onChange={(e) => setCode(e.target.value)} className="h-10 min-w-0 flex-1 rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary" />
        <button type="submit" disabled={busy || !code.trim()} data-testid="share-meeting" className="h-10 shrink-0 rounded-xl px-3.5 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-40">Share</button>
      </form>
      {error ? <p role="alert" className="mt-1 text-sm text-destructive">{error}</p> : null}
    </section>
  );
}

function ConversationSearch({ engine, conversation, onClose }: { engine: MessagingEngine; conversation: ConversationView; onClose: () => void }) {
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="border-b border-border px-4 py-2.5" data-testid="conversation-message-search">
      <form className="flex items-center gap-2" onSubmit={(e) => {
        e.preventDefault();
        setError(null);
        void engine.search(conversation.id, q).then(setHits, (cause) => setError(messagingErrorMessage(cause)));
      }}>
        <label className="flex h-10 min-w-0 flex-1 items-center gap-2 rounded-full bg-muted px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
          <Search className="size-4 shrink-0" />
          <input autoFocus aria-label="Search messages" placeholder={conversation.securityProfile === "PRIVATE_E2EE" ? "Search this chat (on this device)" : "Search this chat"} value={q} maxLength={200} onChange={(e) => setQ(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
        </label>
        <button type="submit" disabled={!q.trim()} className="h-10 rounded-full px-3.5 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-40">Search</button>
        <button type="button" aria-label="Close search" onClick={onClose} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-4" /></button>
      </form>
      {error ? <p role="alert" className="mt-2 text-sm text-destructive">{error}</p> : null}
      {hits ? (
        hits.length ? (
          <ul className="no-scrollbar mt-2 max-h-48 space-y-0.5 overflow-auto" data-testid="search-results">
            {hits.map((h) => <li key={h.messageId} className="rounded-lg px-3 py-2 text-sm hover:bg-muted"><span className="line-clamp-2">{h.text}</span><time className="text-xs text-muted-foreground">{h.createdAt ? new Date(h.createdAt).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : ""}</time></li>)}
          </ul>
        ) : <p className="mt-2 px-1 text-sm text-muted-foreground">No messages match.</p>
      ) : null}
    </div>
  );
}

/* ─── Open chat ───────────────────────────────────────────────────────── */

function ConversationPane({ engine, conversation, onBack }: { engine: MessagingEngine; conversation: ConversationView; onBack?: () => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const client = useQueryClient();
  const [text, setText] = useState("");
  const [details, setDetails] = useState(false);
  const [searching, setSearching] = useState(false);
  const [replyTo, setReplyTo] = useState<MessageView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const file = useRef<HTMLInputElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const bottom = useRef<HTMLDivElement>(null);
  const messages = useMemo(() => s.messages[conversation.id] ?? [], [s.messages, conversation.id]);
  // "online" / "last seen" for a one-to-one chat (WhatsApp); groups keep the security badge.
  useEffect(() => { engine.watchPresence(conversation.id); return () => engine.watchPresence(null); }, [engine, conversation.id]);
  const others = Object.values(s.presence[conversation.id] ?? {});
  const presenceText = conversation.contextClass === "GROUP" || others.length !== 1 ? null
    : others[0].online ? "online"
    : others[0].lastSeenAt ? lastSeenText(others[0].lastSeenAt) : null;
  const e2ee = conversation.securityProfile === "PRIVATE_E2EE";
  const managed = conversation.securityProfile === "MANAGED";
  // Private chats: you can type as soon as this browser is set up; until the chat's encryption
  // is ready here, messages wait on this device and go out automatically.
  const canSend = !!s.identity && (!e2ee || s.phase === "ready");
  // Files follow Messaging's effective attachment policy (Hub capabilities): the attach
  // control is disabled when the server would refuse every file.
  const hubCapabilities = useHubCapabilities();
  const attachments = attachmentAvailability(hubCapabilities.data, conversation.contextClass);
  const canAttach = canSend && attachments.enabled;
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
  const group = conversation.contextClass === "GROUP";
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
  // Keep the newest message in view.
  useEffect(() => { bottom.current?.scrollIntoView({ block: "end" }); }, [items.length]);
  // Composer grows with the text, up to ~6 lines.
  useEffect(() => {
    const el = composer.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [text]);
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
  const queryClient = useQueryClient();
  const saved = useQuery({ queryKey: SAVED_KEY, queryFn: () => hub.savedMessages(), retry: false, staleTime: 30_000 });
  const savedIds = useMemo(() => new Map((saved.data?.data ?? []).map((x) => [x.messageId, x.savedId])), [saved.data]);
  const bubbleActions = (m: MessageView): BubbleActions => ({
    reply: () => { setReplyTo(m); composer.current?.focus(); },
    react: (reaction, add) => { if (m.messageId) void engine.react(conversation.id, m.messageId, reaction, add).catch(fail); },
    edit: async (t) => { if (m.messageId && t.trim()) await engine.editMessage(conversation.id, m.messageId, t).catch(fail); },
    remove: (scope) => { if (m.messageId) void engine.deleteMessage(conversation.id, m.messageId, scope).catch(fail); },
    pin: (pinned) => { if (m.messageId) void engine.pin(conversation.id, m.messageId, pinned).catch(fail); },
    starred: !!(m.messageId && savedIds.has(m.messageId)),
    star: () => {
      if (!m.messageId) return;
      const savedId = savedIds.get(m.messageId);
      void (savedId ? hub.unsaveMessage(savedId) : hub.saveMessage(conversation.id, m.messageId)).then(() => queryClient.invalidateQueries({ queryKey: SAVED_KEY })).catch(fail);
    },
    retry: () => { if (m.outboxKey) engine.retrySend(m.outboxKey); },
    discard: () => { if (m.outboxKey) engine.discardSend(m.outboxKey); },
  });
  const mentions = (t: string) =>
    conversation.members.filter((p) => p !== s.identity?.principalId && s.names[p] && t.includes(`@${s.names[p]}`));
  const send = () => {
    const t = text;
    if (!t.trim() || !canSend) return;
    const reply = managed ? replyTo?.messageId ?? null : null;
    setText("");
    setReplyTo(null);
    setError(null);
    engine.typing(conversation.id, false);
    void engine.send(conversation.id, t, managed ? { replyToMessageId: reply, mentionedPrincipalIds: mentions(t) } : {}).catch((cause) => {
      setText((draft) => draft || t);
      setError(messagingErrorMessage(cause));
    });
  };
  const startMeeting = async () => {
    setStarting(true);
    setError(null);
    try {
      const link = await hub.instantMeeting(conversation.id);
      // The Hub card is the server record; a MANAGED chat also gets a client-composed line (MSG-009).
      if (managed) await engine.send(conversation.id, meetingShareLine(link.meetingCode, link.origin));
      void client.invalidateQueries({ queryKey: hubKeys.timeline(conversation.id) });
    } catch (cause) {
      setError(communicationsErrorMessage(cause));
    } finally {
      setStarting(false);
    }
  };
  const senderName = (m: MessageView) => (m.senderPrincipalId && s.names[m.senderPrincipalId]) || (counterpart && !m.mine ? counterpart.alias || counterpart.displayName || "CF Meet user" : "CF Meet user");
  const iconBtn = "flex size-10 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted hover:text-foreground";

  return (
    <div className="relative flex h-full min-w-0">
      <section className="flex min-w-0 flex-1 flex-col" data-testid="conversation" data-conversation-id={conversation.id} data-profile={conversation.securityProfile}>
        <header className="flex h-16 shrink-0 items-center gap-2 border-b border-border px-2 sm:px-4">
          {onBack ? (
            <button type="button" className={cn(iconBtn, "md:hidden")} onClick={onBack} aria-label="Back to conversations" data-testid="back-to-conversations"><ArrowLeft className="size-5" /></button>
          ) : null}
          <button type="button" onClick={() => setDetails(!details)} className="flex min-w-0 flex-1 items-center gap-3 rounded-xl px-1 py-1 text-left hover:bg-muted/50">
            <Avatar name={title} seed={conversation.id} size="size-10" />
            <span className="min-w-0">
              <span className="block truncate font-semibold" data-testid="conversation-title">{title}</span>
              <span className="block truncate">{typing.length ? <span className="text-xs font-medium text-primary">{typing.join(", ")} {typing.length === 1 ? "is" : "are"} typing…</span> : presenceText ? <span className={cn("text-xs", presenceText === "online" ? "font-medium text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")} data-testid="presence">{presenceText}</span> : <ProfileBadge profile={conversation.securityProfile} />}</span>
            </span>
          </button>
          <div className="flex shrink-0 items-center">
            {actions.instantMeeting ? (
              <button type="button" disabled={starting} onClick={() => void startMeeting()} data-testid="instant-meeting" className="mr-1 hidden h-9 items-center gap-1.5 rounded-full bg-primary px-3.5 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60 sm:inline-flex">
                {starting ? <Spinner /> : <Video className="size-4" />}Meet now
              </button>
            ) : null}
            {actions.videoCall ? <Link href={callHref({ conversationId: conversation.id }, "VIDEO", title)} aria-label="Video call" title="Video call" data-testid="conversation-video-call" className={iconBtn}><Video className="size-5" /></Link> : null}
            {actions.voiceCall ? <Link href={callHref({ conversationId: conversation.id }, "VOICE", title)} aria-label="Voice call" title="Voice call" data-testid="conversation-call" className={iconBtn}><Phone className="size-5" /></Link> : null}
            <button type="button" className={cn(iconBtn, searching && "bg-muted text-foreground")} onClick={() => setSearching(!searching)} aria-expanded={searching} aria-label="Search messages" data-testid="conversation-search-toggle"><Search className="size-5" /></button>
            <button type="button" className={cn(iconBtn, details && "bg-muted text-foreground")} onClick={() => setDetails(!details)} aria-expanded={details} aria-label="Chat info" data-testid="conversation-members-toggle"><Info className="size-5" /></button>
          </div>
        </header>
        {searching ? <ConversationSearch engine={engine} conversation={conversation} onClose={() => setSearching(false)} /> : null}
        {pins.length ? (
          <div className="flex items-center gap-2 border-b border-border bg-muted/40 px-4 py-2 text-sm" data-testid="pinned-bar">
            <Pin className="size-4 shrink-0 text-primary" />
            <span className="min-w-0 flex-1 truncate"><span className="font-semibold">{pins.length === 1 ? "Pinned" : `${pins.length} pinned`}</span> <span className="text-muted-foreground">{pins.slice(0, 3).map((p) => messages.find((m) => m.messageId === p.messageId)?.text ?? "message").join(" · ")}</span></span>
          </div>
        ) : null}

        <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-3 py-2 sm:px-6">
          {e2ee && !conversation.groupReady ? (
            <p className="mx-auto my-4 flex max-w-md items-start gap-3 rounded-2xl bg-muted/60 p-4 text-sm" data-testid="e2ee-unavailable">
              <Lock className="mt-0.5 size-4 shrink-0 text-emerald-600" />
              {s.phase === "ready" ? "Securing this chat on this browser. It finishes when someone in the chat is online. You can still write; your messages are sent automatically." : "Setting up private messaging in this browser…"}
            </p>
          ) : null}
          <ol className="mx-auto max-w-3xl pb-2" data-testid="messages">
            {managed && s.hasOlder[conversation.id] ? (
              <li className="py-2 text-center">
                <button type="button" data-testid="load-older" onClick={() => void engine.loadOlder(conversation.id).catch(fail)} className="h-8 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10">Load older messages</button>
              </li>
            ) : null}
            {items.length === 0 && (!e2ee || conversation.groupReady) ? (
              <li className="flex flex-col items-center px-6 py-16 text-center">
                <Avatar name={title} seed={conversation.id} size="size-16" className="text-xl" />
                <p className="mt-3 font-semibold">{title}</p>
                <p className="mt-1 max-w-xs text-sm text-muted-foreground">{e2ee ? "Messages here are end-to-end encrypted. Only people in this chat can read them." : "Say hello to start the conversation."}</p>
              </li>
            ) : null}
            {items.map((item, index) => {
              const prev = items[index - 1];
              const next = items[index + 1];
              const at = item.type === "card" ? item.card.occurredAt : item.message.createdAt;
              const prevAt = prev ? (prev.type === "card" ? prev.card.occurredAt : prev.message.createdAt) : null;
              const separator = at && (!prevAt || !sameDay(prevAt, at)) ? (
                <li className="sticky top-1 z-10 my-3 flex justify-center"><span className="rounded-full bg-background/90 px-3 py-1 text-xs font-medium text-muted-foreground shadow-sm ring-1 ring-border backdrop-blur">{dayLabel(at)}</span></li>
              ) : null;
              if (item.type === "card") return <Fragment key={item.key}>{separator}<TimelineCardView card={item.card} /></Fragment>;
              const m = item.message;
              const same = (o?: (typeof items)[number]) => !!o && o.type === "message" && o.message.senderPrincipalId === m.senderPrincipalId && o.message.mine === m.mine && !!o.message.createdAt && !!m.createdAt && Math.abs(Date.parse(o.message.createdAt) - Date.parse(m.createdAt)) < 5 * 60000;
              const first = !!separator || !same(prev);
              const nextSeparated = next && (() => { const nAt = next.type === "card" ? next.card.occurredAt : next.message.createdAt; return !!nAt && !!m.createdAt && !sameDay(nAt, m.createdAt); })();
              const last = !same(next) || !!nextSeparated;
              return (
                <Fragment key={item.key}>
                  {separator}
                  <MessageBubble m={m} managed={managed} names={s.names} name={senderName(m)} group={group} first={first} last={last} actions={bubbleActions(m)}
                    onDownload={() => { setError(null); void download(m).catch(fail); }} />
                </Fragment>
              );
            })}
          </ol>
          <div ref={bottom} />
        </div>

        {error ? <p role="alert" className="mx-3 mb-2 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive sm:mx-6">{error}</p> : null}
        <div className="shrink-0 px-3 pb-3 sm:px-6 sm:pb-4">
          <div className="mx-auto max-w-3xl">
            {replyTo ? (
              <div className="mb-2 flex items-center gap-3 rounded-2xl bg-muted/60 py-2 pl-3 pr-1.5 text-sm" data-testid="reply-banner">
                <CornerUpLeft className="size-4 shrink-0 text-primary" />
                <span className="min-w-0 flex-1"><span className="block text-xs font-semibold text-primary">Replying to {replyTo.mine ? "yourself" : (replyTo.senderPrincipalId && s.names[replyTo.senderPrincipalId]) || "message"}</span><span className="block truncate text-muted-foreground">{replyTo.text ?? ""}</span></span>
                <button type="button" aria-label="Cancel reply" onClick={() => setReplyTo(null)} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-4" /></button>
              </div>
            ) : null}
            <form className="flex items-end gap-1.5 rounded-[26px] border border-border bg-card p-1.5 shadow-sm focus-within:border-primary/50" onSubmit={(e) => { e.preventDefault(); send(); }}>
              <input
                ref={file}
                type="file"
                className="hidden"
                data-testid="file-input"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) {
                    const tooLarge = attachmentSizeRefusal(f.size, attachments);
                    setError(tooLarge);
                    if (!tooLarge) void engine.sendFile(conversation.id, f).catch(fail);
                  }
                  e.target.value = "";
                }}
              />
              <button type="button" className="flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40" disabled={!canAttach} onClick={() => file.current?.click()} aria-label="Attach a file" title={attachments.enabled ? "Attach a file" : "Sending files isn't available in this chat"}><Paperclip className="size-5" /></button>
              <textarea
                ref={composer}
                rows={1}
                data-testid="composer"
                value={text}
                maxLength={16384}
                disabled={!canSend}
                placeholder={canSend ? (e2ee ? "Message" : "Message (CF Meet can read this chat)") : "You can't send messages from this browser"}
                onChange={(e) => { setText(e.target.value); if (managed) engine.typing(conversation.id, e.target.value.length > 0); }}
                onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); } }}
                className="max-h-40 min-h-10 min-w-0 flex-1 resize-none bg-transparent px-1 py-2.5 text-[15px] leading-5 outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed"
              />
              <button type="submit" data-testid="send" disabled={!canSend || !text.trim()} aria-label="Send" className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-opacity hover:bg-primary/90 disabled:opacity-30"><SendHorizontal className="size-5" /></button>
            </form>
            <p className="mt-1.5 hidden text-center text-[11px] text-muted-foreground sm:block">{e2ee ? <><Lock className="mr-1 inline size-3" />End-to-end encrypted</> : "Enter to send · Shift + Enter for a new line"}</p>
          </div>
        </div>
      </section>
      {details ? <DetailsPanel engine={engine} conversation={conversation} title={title} onClose={() => setDetails(false)} shareEnabled={!!actions.shareMeeting} /> : null}
    </div>
  );
}

/**
 * Messages: the selected workspace decides the scope. Personal shows the account's personal
 * conversations (this device's engine); an organization workspace shows that
 * organization's own conversations (organization-scope Messaging, J-10), never mixed.
 */
export default function MessagesContent() {
  const workspace = useWorkspace();
  if (workspace.kind === "ORGANIZATION" && workspace.organizationId) {
    return (
      <OrganizationMessages
        key={workspace.organizationId}
        organizationId={workspace.organizationId}
        organizationName={workspace.workspace?.displayName ?? null}
      />
    );
  }
  return <PersonalMessages />;
}

type ListFilter = "ALL" | "UNREAD" | SecurityProfile;

function PersonalMessages() {
  const engine = useEngine();
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const params = useSearchParams();
  const requested = params.get("conversation");
  const [selected, setSelected] = useState<string | null>(requested);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<ListFilter>("ALL");
  const [securityOpen, setSecurityOpen] = useState(false);
  const [composeOpen, setComposeOpen] = useState(false);
  const [starredOpen, setStarredOpen] = useState(false);
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const inbox = useHubInbox(areas.contacts || areas.managedMessaging);
  const muted = useMemo(() => new Set((inbox.data?.data ?? []).filter((e) => e.notifications === "MUTED").map((e) => e.conversationId)), [inbox.data]);
  useEffect(() => {
    // Contact -> conversation navigation: a chat created elsewhere appears after a refresh.
    if (requested && !s.conversations.some((c) => c.id === requested) && s.phase !== "loading") void engine.refreshConversations().catch(() => {});
  }, [engine, requested, s.conversations, s.phase]);
  const current = s.conversations.find((c) => c.id === selected) ?? null;
  const unreadTotal = s.conversations.filter((c) => c.unreadCount > 0 && c.id !== selected).length;
  const conversations = s.conversations.filter((conversation) => {
    const matchesSearch = conversation.title.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase());
    const matchesFilter = filter === "ALL" || (filter === "UNREAD" ? conversation.unreadCount > 0 && conversation.id !== selected : conversation.securityProfile === filter);
    return matchesSearch && matchesFilter;
  });
  const filters: [ListFilter, string][] = [["ALL", "All"], ["UNREAD", unreadTotal ? `Unread ${unreadTotal}` : "Unread"], ["PRIVATE_E2EE", "Private"], ["MANAGED", "Managed"]];

  if (s.phase === "locked") {
    return (
      <div className="flex h-full min-h-[32rem] flex-col items-center justify-center bg-background px-6 text-center" data-testid="messages-page">
        <span className="flex size-16 items-center justify-center rounded-2xl bg-primary/10 text-primary"><MessageSquarePlus className="size-7" /></span>
        <h1 className="mt-5 text-xl font-semibold">Messages is open in another tab</h1>
        <p className="mt-2 max-w-sm text-sm text-muted-foreground">To keep your private chats safe, Messages runs in one tab at a time. Close the other tab, then use it here.</p>
        <button type="button" onClick={() => window.location.reload()} className="mt-6 h-11 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Use here</button>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-[32rem] flex-col bg-background" data-testid="messages-page">
      <ConnectionBanner engine={engine} />
      <IncomingTransfers engine={engine} />
      <IncomingLinks engine={engine} />
      {s.phase !== "loading" && s.phase !== "ready" ? (
        <section className="border-b border-border px-4 py-3" aria-label="Secure messaging setup">
          <DevicePanel engine={engine} />
        </section>
      ) : null}
      <div className="flex min-h-0 flex-1">
        <aside className={cn("flex w-full min-h-0 flex-col border-r border-border md:w-[22rem] md:shrink-0", current && "hidden md:flex")} aria-label="Conversations">
          <div className="space-y-3 px-4 pb-2 pt-4">
            <div className="flex items-center justify-between gap-2">
              <h1 className="text-2xl font-bold tracking-tight">Chats</h1>
              <div className="flex items-center">
                {s.phase === "ready" ? (
                  <button type="button" aria-label="Security and devices" title="Security and devices" onClick={() => setSecurityOpen(true)} aria-expanded={securityOpen} aria-controls="device-security-settings" data-testid="device-settings-toggle" className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><ShieldCheck className="size-5" /></button>
                ) : null}
                <button type="button" aria-label="Starred messages" title="Starred messages" onClick={() => setStarredOpen(true)} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><Star className="size-5" /></button>
                <button type="button" aria-label="New chat" title="New chat" data-testid="new-conversation" disabled={!s.identity} onClick={() => setComposeOpen(true)} className="hidden size-10 items-center justify-center rounded-full bg-primary/10 text-primary hover:bg-primary/15 disabled:opacity-40 md:flex"><MessageSquarePlus className="size-5" /></button>
              </div>
            </div>
            <label className="flex h-10 items-center gap-2 rounded-full bg-muted px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
              <Search className="size-4 shrink-0" />
              <input type="search" aria-label="Search conversations" data-testid="conversation-search" placeholder="Search chats" value={search} onChange={(event) => setSearch(event.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
            </label>
            <div className="no-scrollbar -mx-4 flex gap-1.5 overflow-x-auto px-4" role="group" aria-label="Filter conversations">
              {filters.map(([value, label]) => (
                <button type="button" key={value} aria-pressed={filter === value} onClick={() => setFilter(value)}
                  className={cn("h-8 shrink-0 rounded-full px-3.5 text-sm font-medium transition-colors", filter === value ? "bg-primary/10 text-primary" : "bg-muted/60 text-muted-foreground hover:bg-muted hover:text-foreground")}>
                  {label}
                </button>
              ))}
            </div>
          </div>
          <ul className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-2 pb-2" data-testid="conversation-list">
            {s.phase === "loading" ? (
              [0, 1, 2, 3].map((i) => <li key={i} className="flex items-center gap-3 px-3 py-3"><span className="size-12 animate-pulse rounded-full bg-muted" /><span className="flex-1 space-y-2"><span className="block h-3.5 w-1/2 animate-pulse rounded bg-muted" /><span className="block h-3 w-3/4 animate-pulse rounded bg-muted" /></span></li>)
            ) : conversations.map((c) => {
              const latest = s.messages[c.id]?.at(-1);
              const summary = c.lastMessage;
              const preview = latest
                ? (latest.text ?? (latest.file ? `📎 ${latest.file.name}` : null) ?? latest.note ?? "")
                : summary
                  ? summary.lifecycle !== "ACTIVE" ? "Message deleted" : c.securityProfile === "PRIVATE_E2EE" ? "" : (summary.previewText ?? (summary.attachmentCount ? "📎 Attachment" : ""))
                  : "No messages yet";
              const at = latest?.createdAt ?? summary?.createdAt ?? null;
              const mine = latest ? latest.mine : summary?.senderPrincipalId === s.identity?.principalId;
              const unread = c.id === selected ? 0 : c.unreadCount;
              const active = c.id === selected;
              return (
                <li key={c.id}>
                  <button
                    type="button"
                    data-testid="conversation-item"
                    data-profile={c.securityProfile}
                    data-unread={unread || undefined}
                    aria-current={active ? "true" : undefined}
                    onClick={() => setSelected(c.id)}
                    className={cn("flex w-full items-center gap-3 rounded-2xl px-2.5 py-2.5 text-left transition-colors", active ? "bg-primary/10" : "hover:bg-muted/70")}
                  >
                    <span className="relative">
                      <Avatar name={c.title} seed={c.id} size="size-12" />
                      {c.securityProfile === "PRIVATE_E2EE" ? <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full bg-background"><Lock className="size-3 text-emerald-600" aria-label="End-to-end encrypted" /></span> : null}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="flex items-baseline justify-between gap-2">
                        <span className={cn("flex min-w-0 items-center gap-1.5 text-[15px]", unread ? "font-bold" : "font-semibold")}>
                          <span className="truncate">{c.title}</span>
                          {muted.has(c.id) ? <BellOff className="size-3.5 shrink-0 text-muted-foreground" aria-label="Muted" /> : null}
                        </span>
                        {at ? <time className={cn("shrink-0 text-xs", unread ? "font-semibold text-primary" : "text-muted-foreground")}>{listTime(at)}</time> : null}
                      </span>
                      <span className="mt-0.5 flex items-center justify-between gap-2">
                        <span className={cn("flex min-w-0 items-center gap-1 text-sm", unread ? "font-medium text-foreground" : "text-muted-foreground")}>
                          {mine && latest && (latest.state === "sent" || latest.state === "delivered") ? <Ticks receipt={latest.seenBy?.length ? "read" : latest.receipt ?? "sent"} list /> : null}
                          {latest?.state === "failed" ? <span className="shrink-0 text-destructive">Not sent ·</span> : null}
                          {!latest && summary && summary.lifecycle === "ACTIVE" && c.securityProfile === "PRIVATE_E2EE"
                            ? <span aria-label="Loading message" className="h-3 w-32 animate-pulse rounded-full bg-muted" />
                            : <span className="truncate">{mine && preview && !latest ? "You: " : ""}{preview}</span>}
                        </span>
                        {unread ? (
                          <span className="flex shrink-0 items-center gap-1">
                            {c.mentionCount ? <span className="flex size-5 items-center justify-center rounded-full bg-amber-500 text-[11px] font-bold text-white" data-testid="mention-badge">@</span> : null}
                            <span className="flex h-5 min-w-5 items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-bold text-primary-foreground" data-testid="unread-badge">{unread > 99 ? "99+" : unread}</span>
                          </span>
                        ) : null}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
            {s.phase !== "loading" && conversations.length === 0 ? (
              <li className="flex flex-col items-center px-6 py-14 text-center">
                <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><MessageSquarePlus className="size-6" /></span>
                <p className="mt-4 font-semibold">{search ? "No chats match" : filter === "UNREAD" ? "You're all caught up" : "No chats yet"}</p>
                <p className="mt-1 text-sm text-muted-foreground">{search ? "Try another name." : filter === "UNREAD" ? "New messages will show here." : "Message a contact or colleague to get started."}</p>
                {!search && filter === "ALL" && s.identity ? <button type="button" onClick={() => setComposeOpen(true)} className="mt-4 h-10 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Start a chat</button> : null}
              </li>
            ) : null}
          </ul>
          {/* WhatsApp-style floating "new chat" button on phones. */}
          <button type="button" aria-label="New chat" title="New chat" disabled={!s.identity} onClick={() => setComposeOpen(true)}
            className="fixed bottom-24 right-4 z-30 flex size-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-xl shadow-primary/30 hover:bg-primary/90 disabled:opacity-40 md:hidden">
            <MessageSquarePlus className="size-6" />
          </button>
        </aside>
        <main className={cn("min-w-0 min-h-0 flex-1", !current && "hidden md:flex md:items-center md:justify-center")} data-testid="conversation-pane">
          {current ? (
            <ConversationPane key={current.id} engine={engine} conversation={current} onBack={() => setSelected(null)} />
          ) : (
            <div className="max-w-sm px-6 text-center">
              <div className="relative mx-auto flex size-24 items-center justify-center">
                <span className="absolute inset-0 rounded-[2rem] bg-primary/10" />
                <span className="absolute -right-2 -top-2 size-10 rounded-2xl bg-primary/15" />
                <MessageSquarePlus className="relative size-10 text-primary" />
              </div>
              <h2 className="mt-6 text-xl font-semibold">Pick up a conversation</h2>
              <p className="mt-2 text-sm text-muted-foreground">Choose a chat on the left, or start a new one. Private chats are end-to-end encrypted: not even CF Meet can read them.</p>
              {s.identity ? <button type="button" onClick={() => setComposeOpen(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><MessageSquarePlus className="size-4" />New chat</button> : null}
            </div>
          )}
        </main>
      </div>
      {s.phase === "error" ? <p className="border-t border-border px-4 py-2 text-sm text-destructive" data-testid="messaging-error">Messaging error: {s.detail}</p> : null}

      <NewConversation engine={engine} onCreated={setSelected} open={composeOpen} setOpen={setComposeOpen} />
      <SavedMessages open={starredOpen} onOpenChange={setStarredOpen} onOpenChat={setSelected}
        chatTitle={(id) => s.conversations.find((c) => c.id === id)?.title ?? "Chat"}
        messageText={(id, messageId) => s.messages[id]?.find((m) => m.messageId === messageId)?.text ?? null} />

      {/* Security & devices: a sheet that stays mounted while closed so a history transfer keeps its state. */}
      <div id="device-security-settings" hidden={!securityOpen} className="fixed inset-0 z-50">
        <button type="button" aria-label="Close" className="absolute inset-0 bg-black/40" onClick={() => setSecurityOpen(false)} />
        <aside role="dialog" aria-modal="true" aria-label="Security and devices" className="absolute inset-y-0 right-0 flex w-full max-w-md flex-col bg-background shadow-2xl">
          <header className="flex h-16 shrink-0 items-center justify-between border-b border-border px-5">
            <h2 className="font-semibold">Security & devices</h2>
            <button type="button" aria-label="Close" onClick={() => setSecurityOpen(false)} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-5" /></button>
          </header>
          <div className="no-scrollbar min-h-0 flex-1 space-y-6 overflow-y-auto p-5">
            <StatusBar engine={engine} />
            <DevicePanel engine={engine} />
            <MessagingDevicesPanel engine={engine} />
          </div>
        </aside>
      </div>
    </div>
  );
}

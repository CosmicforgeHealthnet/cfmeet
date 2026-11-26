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
import { ForwardDialog, GroupAdminTools, ReportDialog } from "./ChatActions";
import SavedMessages, { SAVED_KEY } from "./SavedMessages";
import ChatInfoExtras from "./ChatInfoExtras";
import { ArchivedHeader, ArchivedRow, ChatListToggles, DisappearingMessages } from "./ChatListControls";
import { AttachMenu, RichContentView } from "./RichMessages";
import { ChatImage, ChatVideo, LinkifiedText, PhotoViewer } from "./BubbleParts";
import { AttachmentPreview } from "./AttachmentPreview";
import { isInlineImage, isPlayableAudio, isPlayableVideo } from "@/lib/messaging/engine";
import { SecurityCode } from "./SecurityCode";
import { MicButton, VoicePlayer, VoiceRecorder, voiceRecordingSupported } from "./VoiceNote";
import PrivateShares from "./PrivateShares";
import { isLiveStatus, statusPostId } from "@/lib/messaging/private-shares";
import { arrangeChats, pinnedCount } from "@/lib/messaging/chat-list";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowDown, ArrowLeft, Bell, BellOff, Check, Clock3, Copy, Star, Forward, Flag, CheckCheck, ChevronDown, CornerUpLeft, FileText, History, Info, Lock, LogOut, MessageSquarePlus,
  MoreHorizontal, Pencil, Phone, Pin, PinOff, Search, SendHorizontal, Shield, ShieldCheck, ShieldAlert, SmilePlus, Smartphone,
  Trash2, EyeOff, Video, WifiOff, X, Camera, Mic, Headphones, Paperclip,
  UserPlus, Users, Settings2, Link2, Download, MinusCircle, ThumbsDown, Ban,
} from "lucide-react";
import { Popover } from "radix-ui";

import { Spinner } from "@/components/ui/spinner";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { MessagingEngine, type ConversationView, type MessageView, type SearchHit, type SecurityProfile } from "@/lib/messaging/engine";
import { mergeTimeline, type TimelineCard } from "@/lib/messaging/managed-store";
import { attachmentKind, chatPreview, type AttachmentKind } from "@/lib/messaging/e2ee-membership";
import { useMessagingDevices, useRemoveCurrentMessagingDevice } from "@/hooks/v2/useMessaging";
import { hubKeys, useConversationContext, useConversationTimeline, useHubCapabilities, useHubInbox } from "@/hooks/v2/useCommunications";
import { useWorkspace } from "@/context/WorkspaceContext";
import OrganizationMessages from "@/components/messaging/OrganizationMessages";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { communicationsErrorMessage } from "@/lib/communications/errors";
import { directCounterpartLabel } from "@/lib/messaging/conversation-title";
import {
  attachmentAvailability,
  attachmentSizeRefusal,
  callHref,
  communicationsAreas,
  formatDuration,
  meetingJoinHref,
  meetingShareLine,
  normalizeCfMeetId,
} from "@/lib/communications/comms-core";
import type { TimelineCallCard, TimelineMeetingCard } from "@/types/communications";
import MessagingDevicesPanel from "./MessagingDevicesPanel";
import { useSharedMessagingEngine } from "./MessagingEngineProvider";
import { EmojiPicker } from "./EmojiPicker";
import { usePeoplePicker } from "./people-picker";
import { authService } from "@/services/auth.service";

function useEngine(): MessagingEngine {
  // The app-wide engine (live on every page of the signed-in app); a page-local one only
  // where no provider exists.
  const shared = useSharedMessagingEngine();
  const [own] = useState(() => (shared ? null : new MessagingEngine()));
  const engine = shared ?? own!;
  const router = useRouter();
  useEffect(() => {
    if (!own) return;
    void own.start();
    return () => own.stop();
  }, [own]);
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

/** Why private messages can't be sent from this browser right now, in plain words. */
const noSubscribe = () => () => {};

function composerBlockedReason(s: { phase: string; detail: string | null; identity: unknown }): string {
  if (!s.identity || s.phase === "loading") return "Loading…";
  if (s.phase === "locked") return "Messages are open in another tab of this browser";
  if (s.phase === "unavailable") return "Private messaging isn't turned on for CF Meet yet";
  if (s.phase === "needs-signin") return "Sign in again to send private messages";
  if (s.phase === "error") return "Private messaging couldn't start. Reload the page to try again";
  return s.detail ?? "Private messaging isn't available in this browser";
}

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
    <span data-testid="profile-badge" data-profile={profile} title="End-to-end encrypted: only people in this chat can read it" className="inline-flex min-w-0 items-center gap-1 text-xs text-emerald-700 dark:text-emerald-400">
      <Lock className="size-3 shrink-0" /><span className="truncate"><span className="sm:hidden">Encrypted</span><span className="hidden sm:inline">End-to-end encrypted</span></span>
    </span>
  ) : null; // Server-stored chats are the normal kind: no badge.
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
        {s.phase === "ready" && !s.deviceId ? "Signed in · your chats sync on every device" : s.phase === "ready" ? "This browser can send and read encrypted messages" : s.phase === "needs-device" ? "Encrypted messages aren't set up in this browser" : s.phase === "registering" ? "Setting up this browser…" : s.phase}
      </p>
      <p className={row} data-testid="connection-state" data-connection={s.connection}>
        {connected ? <span className="size-2 shrink-0 rounded-full bg-emerald-500" /> : <WifiOff className="size-4 shrink-0 text-muted-foreground" />}
        {s.sessionsPaused ? "Live updates paused" : connected ? "Connected · messages arrive instantly" : s.connection === "connecting" ? "Connecting…" : s.connection === "offline" ? "Live updates unavailable · checking every few seconds" : "Reconnecting…"}
      </p>
      <p className={row} data-testid="kt-state">
        {s.ktPinned ? <ShieldCheck className="size-4 shrink-0 text-emerald-600" /> : <ShieldAlert className="size-4 shrink-0 text-amber-600" />}
        {s.ktPinned ? "Contacts' keys are checked against CF Meet's key log (run by CF Meet)" : "Contacts' keys couldn't be checked against the key log yet"}
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
    try { await action(); } catch { setError("That code doesn't match the new device. Check it and try again."); }
  };
  return (
    <section className="space-y-2 border-b border-border bg-primary/5 px-4 py-3" aria-label="New devices asking to use your account">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {s.incomingLinks.map((l) => (
        <div key={l.linkId} className="flex flex-wrap items-center gap-3" data-testid="incoming-link">
          <Shield className="size-5 shrink-0 text-primary" />
          <p className="min-w-0 flex-1 text-sm">A new phone wants to use private messaging on your account. If it&apos;s yours, enter the code it shows.</p>
          <input aria-label="Code shown on the new device" inputMode="numeric" maxLength={6} data-testid="incoming-link-code"
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
  const sources = devices.data?.devices.filter((d) => d.status === "ACTIVE" && d.deviceId !== s.deviceId) ?? [];
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (cause) { setError(messagingErrorMessage(cause)); }
  };
  const [transfer, setTransfer] = useState<{ code: string; state: string } | null>(null);
  if (s.phase === "loading") return null;
  if (s.phase === "needs-device" || s.phase === "revoked" || s.phase === "registering" || s.phase === "needs-signin") {
    // Set up automatically: no activation or verification step (signing in is enough).
    return (
      <div className="flex flex-wrap items-center gap-3" data-testid="device-setup">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-emerald-500/10 text-emerald-600"><Shield className="size-5" /></span>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold">Setting up private messaging</p>
          <p className="text-xs text-muted-foreground">{s.detail ?? "Your encryption keys are made here and never leave this browser."}</p>
        </div>
        {s.phase === "registering" ? <Spinner /> : null}
        {error ? <p role="alert" className="w-full text-sm text-destructive">{error}</p> : null}
      </div>
    );
  }
  if (s.phase !== "ready") {
    return <p className="text-sm" data-testid="messaging-unavailable">{s.detail ?? "Messaging is unavailable."}</p>;
  }
  // Server-stored chats are on every device already: nothing to set up or bring over.
  if (!s.deviceId) return null;
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

/** WhatsApp's "Add member" in group info: search your contacts / colleagues and tap to add. */
function AddPeople({ onAdd }: { onAdd: (p: { kind: "contact" | "colleague"; value: string }) => void }) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const picker = usePeoplePicker(open, search);
  if (!open) {
    return (
      <button type="button" data-testid="add-member" onClick={() => setOpen(true)}
        className="mt-3 flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm font-medium text-primary hover:bg-muted">
        <span className="flex size-9 items-center justify-center rounded-full bg-primary text-primary-foreground"><UserPlus className="size-4" /></span>
        Add member
      </button>
    );
  }
  return (
    <div className="mt-3 rounded-xl ring-1 ring-border" data-testid="add-member-picker">
      <input autoFocus aria-label="Search people to add" placeholder="Search name" value={search} onChange={(e) => setSearch(e.target.value)}
        className="h-10 w-full rounded-t-xl bg-transparent px-3 text-sm outline-none" />
      <ul className="max-h-60 overflow-y-auto border-t border-border">
        {picker.pending ? <li className="flex items-center gap-2 px-3 py-3 text-sm text-muted-foreground"><Spinner /> Loading…</li> : null}
        {!picker.pending && picker.people.length === 0 ? <li className="px-3 py-3 text-sm text-muted-foreground">{search ? "No one matches." : "No one to add yet."}</li> : null}
        {picker.people.map((p) => (
          <li key={p.id}>
            <button type="button" data-testid="add-member-person" data-name={p.name} onClick={() => { onAdd(p); setOpen(false); setSearch(""); }}
              className="flex w-full items-center gap-3 px-3 py-2 text-left text-sm hover:bg-muted">
              <Avatar name={p.name} seed={p.id} size="size-9" />
              <span className="min-w-0 flex-1 truncate">{p.name}</span>
            </button>
          </li>
        ))}
      </ul>
      <button type="button" onClick={() => setOpen(false)} className="w-full border-t border-border py-2 text-xs font-semibold text-muted-foreground hover:bg-muted">Cancel</button>
    </div>
  );
}

/* ─── New chat ────────────────────────────────────────────────────────── */

type Step = "chat" | "members" | "name";

/**
 * WhatsApp-style New chat: "New group" and "New contact" on top, then people (tap one to chat).
 * New group: pick people → name the group → create.
 */
function NewConversation({ engine, onCreated, open, setOpen }: { engine: MessagingEngine; onCreated: (id: string) => void; open: boolean; setOpen: (v: boolean) => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [step, setStep] = useState<Step>("chat");
  const [search, setSearch] = useState("");
  const picker = usePeoplePicker(open && !!s.identity, search);
  const [chosen, setChosen] = useState<string[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});
  const [groupName, setGroupName] = useState("");
  // New chats are server-stored (MANAGED): available on every device the user signs in
  // on, with no device setup. Existing PRIVATE_E2EE chats keep working where they are.
  const profile: SecurityProfile = "MANAGED";
  const ready = !!s.identity && s.phase !== "loading";
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const toggle = (id: string, name: string) => {
    setChosen((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id]));
    setNames((n) => ({ ...n, [id]: name }));
  };
  const close = () => { setOpen(false); setStep("chat"); setChosen([]); setSearch(""); setGroupName(""); setError(null); };
  const go = (next: Step) => { setStep(next); setSearch(""); setError(null); };
  const create = async (ids: string[], title?: string) => {
    setBusy(ids.length === 1 && !title ? ids[0] : "group");
    setError(null);
    try {
      const userIds = ids.filter((x) => x.startsWith("u:")).map((x) => x.slice(2));
      const cfMeetIds = ids.filter((x) => x.startsWith("cf:")).map((x) => x.slice(3));
      // Managed groups send the name with the create request; private groups carry it inside their encrypted group.
      const id = await engine.createConversation(userIds, profile, { cfMeetIds, title });
      onCreated(id);
      close();
    } catch (e) {
      setError(messagingErrorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const title = step === "chat" ? "New chat" : step === "members" ? "Add group members" : "New group";
  const row = "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left hover:bg-muted/60 disabled:opacity-50";
  const people = (
    <>
      {picker.pending ? <li className="flex items-center gap-2 px-3 py-4 text-sm text-muted-foreground"><Spinner /> Loading…</li>
        : picker.error ? <li role="alert" className="px-3 py-4 text-sm text-destructive">{messagingErrorMessage(picker.error)} <button className="underline" onClick={picker.retry}>Retry</button></li>
          : picker.people.length === 0 ? <li className="px-3 py-8 text-center text-sm text-muted-foreground">{search ? "No one matches." : "No one to message yet."}</li> : null}
      {picker.people.length > 0 ? <li className="px-3 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground" data-testid="people-source">{picker.heading}</li> : null}
      {picker.people.map((p) => {
        const on = chosen.includes(p.id);
        return (
          <li key={p.id}>
            {step === "chat" ? (
              <button type="button" className={row} data-testid="colleague" data-kind={p.kind} data-name={p.name} disabled={!ready || !!busy} onClick={() => void create([p.id])}>
                <Avatar name={p.name} seed={p.id} size="size-11" />
                <span className="min-w-0 flex-1 truncate text-[15px] font-medium">{p.name}</span>
                {busy === p.id ? <Spinner /> : null}
              </button>
            ) : (
              <button type="button" className={row} data-testid="colleague" data-kind={p.kind} data-name={p.name} aria-pressed={on} onClick={() => toggle(p.id, p.name)}>
                <span className="relative">
                  <Avatar name={p.name} seed={p.id} size="size-11" />
                  {on ? <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full border-2 border-background bg-primary text-primary-foreground"><Check className="size-3" strokeWidth={3} /></span> : null}
                </span>
                <span className="min-w-0 flex-1 truncate text-[15px] font-medium">{p.name}</span>
              </button>
            )}
          </li>
        );
      })}
    </>
  );

  return (
    <Dialog open={open} onOpenChange={(v) => (v ? setOpen(true) : close())}>
      <DialogContent className="flex h-[min(720px,92dvh)] flex-col gap-0 p-0 sm:max-w-lg" data-testid="new-conversation-form">
        <DialogHeader className="flex-row items-center gap-2 space-y-0 border-b border-border px-3 py-3 text-left">
          {step !== "chat" ? (
            <button type="button" aria-label="Back" onClick={() => go(step === "name" ? "members" : "chat")} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></button>
          ) : <span className="w-2" />}
          <div className="min-w-0 flex-1">
            <DialogTitle className="text-lg">{title}</DialogTitle>
            <DialogDescription className="text-xs">{step === "members" ? (chosen.length ? `${chosen.length} selected` : "Pick people to add") : step === "name" ? `${chosen.length} ${chosen.length === 1 ? "member" : "members"}` : "Start a chat or create a group"}</DialogDescription>
          </div>
        </DialogHeader>

        {step !== "name" ? (
          <div className="space-y-3 px-4 pt-3">
            <label className="flex h-10 items-center gap-2 rounded-full bg-muted px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
              <Search className="size-4 shrink-0" />
              <input autoFocus aria-label="Search people" placeholder="Search name" maxLength={64} value={search} onChange={(e) => setSearch(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
            </label>
            {step === "members" && chosen.length > 0 ? (
              <div className="no-scrollbar flex gap-3 overflow-x-auto pb-1">
                {chosen.map((id) => (
                  <button key={id} type="button" onClick={() => toggle(id, names[id] ?? "")} aria-label={`Remove ${names[id]}`} className="flex w-14 shrink-0 flex-col items-center gap-1">
                    <span className="relative"><Avatar name={names[id] ?? "?"} seed={id} size="size-12" /><span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full border-2 border-background bg-muted-foreground text-background"><X className="size-3" strokeWidth={3} /></span></span>
                    <span className="w-full truncate text-center text-[11px]">{names[id]?.split(" ")[0]}</span>
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        ) : null}

        {step === "name" ? (
          <div className="flex-1 space-y-5 overflow-y-auto px-5 py-6">
            <div className="flex items-center gap-4">
              <span className="flex size-16 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary"><Users className="size-7" /></span>
              <input autoFocus aria-label="Group name" placeholder="Group name" maxLength={120} value={groupName} onChange={(e) => setGroupName(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && groupName.trim() && ready && !busy) void create(chosen, groupName.trim()); }}
                className="h-11 min-w-0 flex-1 border-b-2 border-primary bg-transparent text-base outline-none" />
            </div>
            <div>
              <p className="mb-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Members: {chosen.length}</p>
              <div className="flex flex-wrap gap-4">
                {chosen.map((id) => (
                  <span key={id} className="flex w-14 flex-col items-center gap-1"><Avatar name={names[id] ?? "?"} seed={id} size="size-12" /><span className="w-full truncate text-center text-[11px]">{names[id]?.split(" ")[0]}</span></span>
                ))}
              </div>
            </div>
          </div>
        ) : (
          <ul className="no-scrollbar mt-2 flex-1 overflow-y-auto px-2 pb-3">
            {step === "chat" && !search ? (
              <>
                <li>
                  <button type="button" className={row} data-testid="new-group" onClick={() => go("members")}>
                    <span className="flex size-11 items-center justify-center rounded-full bg-primary text-primary-foreground"><Users className="size-5" /></span>
                    <span className="text-[15px] font-semibold">New group</span>
                  </button>
                </li>
                <li>
                  <Link href="/dashboard/contacts?add=1" onClick={close} className={row}>
                    <span className="flex size-11 items-center justify-center rounded-full bg-primary text-primary-foreground"><UserPlus className="size-5" /></span>
                    <span className="text-[15px] font-semibold">New contact</span>
                  </Link>
                </li>
              </>
            ) : null}
            {people}
          </ul>
        )}

        <div className="space-y-3 border-t border-border px-5 py-3">
          {!ready ? (
            <div className="flex items-center gap-3 rounded-xl bg-muted/60 p-3" data-testid="new-chat-setup">
              <Shield className="size-4 shrink-0 text-emerald-600" />
              <p className="min-w-0 flex-1 text-xs text-muted-foreground">{
                s.phase === "needs-device" || s.phase === "revoked" || s.phase === "registering" || s.phase === "needs-signin"
                  ? (s.detail ?? "Getting private messaging ready…")
                  : s.phase === "loading" ? "Loading private messaging…"
                  : s.phase === "locked" ? "Messages are open in another tab. Close it, or use them here from the Messages page."
                  : s.phase === "unavailable" ? "New chats can't be started yet: private messaging isn't turned on for CF Meet. Your existing chats still work."
                  : `Private messaging couldn't start (${s.detail ?? "unknown error"}). Reload the page to try again.`}</p>
              {s.phase === "registering" || s.phase === "loading" ? <Spinner /> : null}
            </div>
          ) : null}
          {error ? <p className="text-sm text-destructive" data-testid="create-error">{error}</p> : null}
          {step === "members" ? (
            <button type="button" data-testid="group-next" disabled={chosen.length === 0} onClick={() => go("name")}
              className="flex h-11 w-full items-center justify-center gap-2 rounded-full bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">Next</button>
          ) : step === "name" ? (
            <button type="button" data-testid="create-conversation" disabled={!!busy || !groupName.trim() || !ready} onClick={() => void create(chosen, groupName.trim())}
              className="flex h-11 w-full items-center justify-center gap-2 rounded-full bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
              {busy ? <Spinner /> : null}Create group
            </button>
          ) : null}
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
  if (receipt === "sent") return <Check className={cn("size-4 shrink-0", !list && "opacity-70")} aria-label="Sent" />;
  if (receipt === "delivered") return <CheckCheck className={cn("size-4 shrink-0", !list && "opacity-70")} aria-label="Delivered" />;
  // Read: on your own blue bubble a bright aqua with a heavier stroke (blue on blue would vanish).
  return <CheckCheck strokeWidth={2.75} className={cn("size-4 shrink-0", list ? "text-sky-500" : "text-[#7CFFF4] drop-shadow-[0_0_1px_rgba(0,0,0,0.35)]")} aria-label="Read" />;
}

/** One tick state for a message from every other member's state (read only when all read). */
function summaryReceipt(rows: { state: "read" | "delivered" | "sent" }[]): "read" | "delivered" | "sent" {
  if (!rows.length) return "sent";
  return rows.every((r) => r.state === "read") ? "read" : rows.every((r) => r.state !== "sent") ? "delivered" : "sent";
}

/** WhatsApp's Message info: read by / delivered to, live while open. */
function MessageInfo({ engine, conversationId, message, names, onClose }: { engine: MessagingEngine; conversationId: string; message: MessageView; names: Record<string, string>; onClose: () => void }) {
  useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const rows = message.sequence != null ? engine.messageInfo(conversationId, message.sequence) : [];
  const section = (title: string, state: "read" | "delivered" | "sent", receipt: "read" | "delivered" | "sent") => {
    const people = rows.filter((r) => r.state === state);
    if (!people.length) return null;
    return (
      <section className="space-y-1" data-testid={`info-${state}`}>
        <h3 className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground"><Ticks receipt={receipt} list />{title}</h3>
        <ul>{people.map((p) => <li key={p.principalId} className="flex items-center gap-3 py-1.5 text-sm"><Avatar name={names[p.principalId] ?? "CF Meet user"} seed={p.principalId} size="size-8" />{names[p.principalId] ?? "CF Meet user"}</li>)}</ul>
      </section>
    );
  };
  return (
    <Dialog open onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="max-w-sm">
        <DialogTitle>Message info</DialogTitle>
        <p className="line-clamp-3 rounded-xl bg-muted/60 px-3 py-2 text-sm">{message.text ?? message.file?.name ?? "Message"}</p>
        <div className="space-y-4">
          {section("Read by", "read", "read")}
          {section("Delivered to", "delivered", "delivered")}
          {section("Waiting", "sent", "sent")}
        </div>
      </DialogContent>
    </Dialog>
  );
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
  /** S11 / S2: forward to another chat, report a message. */
  forward: () => void;
  /** Who your message was delivered to and read by (absent: not yours / not sent yet). */
  info?: () => void;
  report: () => void;
  retry: () => void;
  discard: () => void;
  /** A failed send that "Try again" can resend. */
  canRetry: boolean;
  /** F4: vote in a private chat's poll (absent: not a poll). */
  vote?: (choices: number[]) => void;
};

/** The chat list's icon for a file message (WhatsApp: camera for a photo, mic for a voice message). */
const FILE_ICONS = { photo: Camera, video: Video, voice: Mic, audio: Headphones, document: FileText, file: Paperclip } as const;

function MessageBubble({ m, name, names, onDownload, loadImage, cachedMedia, onJump, highlight, actions, first, last, group, onOpenPhoto }: {
  m: MessageView; name: string; names: Record<string, string>; onDownload: () => void; actions: BubbleActions; managed: boolean;
  /** Opens the chat's photo viewer at this photo. */ onOpenPhoto?: () => void;
  /** A photo shown in the chat (null: not an inline photo). */ loadImage: (() => Promise<string | null>) | null;
  /** That photo or voice note, if it was already loaded on this visit. */ cachedMedia: string | null;
  /** Scroll to the quoted message. */ onJump: (messageId: string) => void;
  /** Briefly highlighted after a jump. */ highlight: boolean;
  /** first / last of a run from the same sender (grouping, Messenger-style corners) */ first: boolean; last: boolean; group: boolean;
}) {
  const [menu, setMenu] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  // ↑ in an empty composer edits your last message (WhatsApp / Slack).
  useEffect(() => {
    if (!m.mine || !m.messageId || m.text === null) return;
    const onEdit = (e: Event) => { if ((e as CustomEvent<string>).detail === m.messageId) setEditing(m.text ?? ""); };
    window.addEventListener(EDIT_EVENT, onEdit);
    return () => window.removeEventListener(EDIT_EVENT, onEdit);
  }, [m.mine, m.messageId, m.text]);
  // Private chats get the same actions as managed ones (the server holds no content, only ids).
  const live = !!m.messageId && (m.state === "delivered" || m.state === "sent");
  const mine = m.mine;
  // A message held until the chat's encryption is ready looks like any message being sent.
  // WhatsApp: the time shows at once, with a small clock until the server has it (then a tick).
  const pending = m.state === "queued" || m.state === "sending" || m.state === "pending-setup";
  const status = m.state === "failed" ? "Not sent" : clock(m.createdAt);
  const item = "flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-muted";
  const [dx, setDx] = useState(0);
  const touch = useRef<{ x: number; y: number; timer: ReturnType<typeof setTimeout> | null; moved: boolean } | null>(null);
  const onTouchStart = (e: React.TouchEvent) => {
    if (!live || editing !== null) return;
    const t = e.touches[0];
    const timer = setTimeout(() => { navigator.vibrate?.(10); setMenu(true); touch.current = null; }, 450);
    touch.current = { x: t.clientX, y: t.clientY, timer, moved: false };
  };
  const onTouchMove = (e: React.TouchEvent) => {
    const start = touch.current;
    if (!start) return;
    const t = e.touches[0];
    const x = t.clientX - start.x;
    const y = t.clientY - start.y;
    if (Math.abs(x) > 8 || Math.abs(y) > 8) { if (start.timer) clearTimeout(start.timer); start.timer = null; }
    // A mostly sideways drag to the right is a swipe-to-reply; anything else is scrolling.
    if (x > 0 && Math.abs(x) > Math.abs(y) * 1.5) { start.moved = true; setDx(Math.min(x, 80)); }
  };
  const onTouchEnd = () => {
    const start = touch.current;
    if (start?.timer) clearTimeout(start.timer);
    if (start?.moved && dx >= 56) { navigator.vibrate?.(10); actions.reply(); }
    touch.current = null;
    setDx(0);
  };
  const copy = () => { if (m.text) void navigator.clipboard?.writeText(m.text); setMenu(false); };

  return (
    <li
      data-testid="message"
      data-state={m.state}
      data-receipt={mine ? m.receipt ?? undefined : undefined}
      data-mine={mine ? "true" : "false"}
      data-message-id={m.messageId ?? undefined}
      className={cn("group/msg relative flex items-end gap-2 rounded-xl transition-colors duration-700", mine ? "flex-row-reverse" : "flex-row", first ? "mt-3" : "mt-0.5", highlight && "bg-primary/15")}
      onTouchStart={onTouchStart} onTouchMove={onTouchMove} onTouchEnd={onTouchEnd} onTouchCancel={onTouchEnd}
      onContextMenu={(e) => { if (live && editing === null && window.matchMedia("(hover: none)").matches) e.preventDefault(); }}
      style={dx ? { transform: `translateX(${dx}px)` } : undefined}
    >
      {dx ? <CornerUpLeft aria-hidden className="absolute -left-7 bottom-3 size-5 text-muted-foreground" style={{ opacity: Math.min(dx / 56, 1) }} /> : null}
      {!mine && group ? (last ? <Avatar name={name} seed={m.senderPrincipalId ?? name} size="size-7" className="mb-0.5 text-[10px]" /> : <span className="w-7 shrink-0" />) : null}
      <div className={cn("flex min-w-0 max-w-[min(78%,34rem)] flex-col", mine ? "items-end" : "items-start")}>
        {!mine && group && first ? <p className="mb-1 px-3 text-xs font-semibold" style={{ color: colorFor(m.senderPrincipalId ?? name) }}>{name}</p> : null}
        <div className="relative flex items-center gap-1">
          <div
            className={cn(
              "relative min-w-0 rounded-[20px] px-3.5 py-2 text-[15px] leading-snug",
              mine ? "bg-primary text-primary-foreground shadow-sm" : "bg-card text-foreground shadow-sm",
              // WhatsApp: the first bubble of a run has a tail at its top corner (square there).
              mine ? cn(first ? "rounded-tr-none" : "rounded-tr-md", !last && "rounded-br-md") : cn(first ? "rounded-tl-none" : "rounded-tl-md", !last && "rounded-bl-md"),
              m.state === "failed" && "ring-2 ring-destructive",
              m.announcement && "ring-2 ring-amber-400",
            )}
          >
            {first ? (
              <svg aria-hidden viewBox="0 0 8 13" className={cn("absolute top-0 h-[13px] w-2", mine ? "-right-2 text-primary" : "-left-2 -scale-x-100 text-card")}>
                <path d="M0 0h8L1.5 9.5C.9 10.3 0 9.9 0 9z" fill="currentColor" />
              </svg>
            ) : null}
            {m.pinned ? <p className="mb-0.5 flex items-center gap-1 text-[11px] font-medium opacity-75" data-testid="message-pinned"><Pin className="size-3" /> Pinned</p> : null}
            {m.forwarded ? <p className="mb-0.5 flex items-center gap-1 text-[11px] italic opacity-75" data-testid="message-forwarded"><Forward className="size-3" /> Forwarded</p> : null}
            {m.replyTo ? (
              <button type="button" disabled={m.replyTo.removed} onClick={() => onJump(m.replyTo!.messageId)}
                className={cn("mb-1.5 block w-full rounded-lg border-l-4 px-2.5 py-1.5 text-left text-xs", mine ? "border-white/60 bg-white/15" : "border-primary bg-background/70")} data-testid="message-reply">
                {m.replyTo.removed ? <span className="italic opacity-80">Original message deleted</span> : <><span className="block font-semibold">{m.replyTo.senderPrincipalId ? (names[m.replyTo.senderPrincipalId] ?? "CF Meet user") : "Message"}</span><span className="line-clamp-2 opacity-80">{m.replyTo.text ?? "…"}</span></>}
              </button>
            ) : null}
            {editing !== null ? (
              <form className="flex min-w-56 flex-col gap-2" onSubmit={(e) => { e.preventDefault(); void actions.edit(editing).then(() => setEditing(null)); }}>
                <input autoFocus className="min-w-0 rounded-lg bg-background px-2.5 py-1.5 text-sm text-foreground outline-none" aria-label="Edit message" value={editing} maxLength={16384} onChange={(e) => setEditing(e.target.value)} onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); setEditing(null); } }} data-testid="edit-input" />
                <span className="flex justify-end gap-2 text-xs font-semibold">
                  <button type="button" className="rounded-full px-2.5 py-1 hover:bg-white/15" onClick={() => setEditing(null)}>Cancel</button>
                  <button type="submit" className="rounded-full bg-background px-2.5 py-1 text-foreground" data-testid="edit-save">Save</button>
                </span>
              </form>
            ) : m.text !== null && !m.file ? (
              m.rich && m.rich.kind !== "link" ? null : <p className="whitespace-pre-wrap break-words" data-testid="message-text"><LinkifiedText text={m.text} mine={mine} /></p>
            ) : null}
            {m.rich && editing === null ? <RichContentView rich={m.rich} mine={mine} onVote={actions.vote} /> : null}
            {m.file && loadImage && isPlayableAudio(m.file.mediaType) ? <VoicePlayer load={loadImage} initial={cachedMedia} mine={mine} onDownload={onDownload} /> : m.file && loadImage && isPlayableVideo(m.file.mediaType) ? <ChatVideo load={loadImage} initial={cachedMedia} name={m.file.name} onDownload={onDownload} /> : m.file && loadImage ? <ChatImage load={loadImage} initial={cachedMedia} name={m.file.name} onDownload={onDownload} onOpen={onOpenPhoto} /> : m.file ? (
              <button type="button" className={cn("mt-1 flex w-full min-w-48 items-center gap-3 rounded-xl p-2 text-left", mine ? "bg-white/15 hover:bg-white/20" : "bg-background/70 hover:bg-background")} data-testid="attachment" onClick={onDownload}>
                <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-lg", mine ? "bg-white/20" : "bg-primary/10 text-primary")}><FileText className="size-4" /></span>
                <span className="min-w-0"><span className="block truncate text-sm font-medium">{m.file.name}</span>{m.file.size ? <span className="block text-xs opacity-70">{Math.ceil(m.file.size / 1024)} KB · Download</span> : null}</span>
              </button>
            ) : null}
            {/* A photo's caption sits under it (WhatsApp); a normal chat's file without one carries its name, so that isn't repeated. */}
            {m.file && m.text && editing === null && m.text !== m.file.name ? <p className="whitespace-pre-wrap break-words" data-testid="file-caption"><LinkifiedText text={m.text} mine={mine} /></p> : null}
            {m.note ? <p className="text-xs italic opacity-80" data-testid="message-note">{m.note}</p> : null}
            <p className={cn("mt-0.5 flex items-center justify-end gap-1 text-[11px] tabular-nums", mine ? "text-primary-foreground/70" : "text-muted-foreground")}>
              {m.editCount ? "edited · " : ""}{m.transferred ? "recovered · " : ""}{status}
              {mine && pending ? <Clock3 className="size-3.5 shrink-0 opacity-70" aria-label="Sending" /> : null}
              {mine && (m.state === "sent" || m.state === "delivered") ? <Ticks receipt={m.seenBy?.length ? "read" : m.receipt ?? "sent"} /> : null}
            </p>
          </div>

          {/* Hover actions (Teams / Slack): react, reply, more */}
          {live ? (
            <div className={cn("flex shrink-0 items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover/msg:opacity-100 [@media(hover:none)]:w-0 [@media(hover:none)]:overflow-hidden [@media(hover:none)]:!opacity-0", menu && "opacity-100", mine ? "order-first" : "")}>
              {/* Hover buttons (desktop). On touch screens they take no width — long-press a message
                  for its menu (WhatsApp), which opens at the bubble's edge — so bubbles keep their width. */}
              <Popover.Root>
                <Popover.Trigger asChild><button type="button" aria-label="React" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><SmilePlus className="size-4" /></button></Popover.Trigger>
                <Popover.Portal>
                  <Popover.Content side="top" sideOffset={4} className="z-50 flex gap-0.5 rounded-full border border-border bg-popover p-1 shadow-lg">
                    {QUICK_REACTIONS.map((r) => <Popover.Close asChild key={r}><button type="button" aria-label={`React ${r}`} onClick={() => actions.react(r, !(m.reactions ?? []).some((x) => x.reaction === r && x.reactedByMe))} className="flex size-9 items-center justify-center rounded-full text-xl transition-transform hover:scale-125 hover:bg-muted">{r}</button></Popover.Close>)}
                  </Popover.Content>
                </Popover.Portal>
              </Popover.Root>
              <button type="button" aria-label="Reply" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground" onClick={actions.reply}><CornerUpLeft className="size-4" /></button>
              {/* WhatsApp's quick forward arrow. */}
              <button type="button" aria-label="Forward" data-testid="quick-forward" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground" onClick={actions.forward}><Forward className="size-4" /></button>
              <Popover.Root open={menu} onOpenChange={setMenu}>
                <Popover.Trigger asChild><button type="button" aria-label="Message actions" aria-expanded={menu} data-testid="message-actions" className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreHorizontal className="size-4" /></button></Popover.Trigger>
                <Popover.Portal>
                  <Popover.Content side="top" align={mine ? "end" : "start"} sideOffset={4} className="z-50 w-52 rounded-xl border border-border bg-popover p-1.5 shadow-xl" data-testid="message-menu">
                    <div className="mb-1 flex justify-between border-b border-border px-1 pb-1.5">
                      {QUICK_REACTIONS.map((r) => <button key={r} type="button" aria-label={`React ${r}`} onClick={() => { actions.react(r, !(m.reactions ?? []).some((x) => x.reaction === r && x.reactedByMe)); setMenu(false); }} className="flex size-7 items-center justify-center rounded-full text-base hover:bg-muted">{r}</button>)}
                    </div>
                    <button type="button" className={item} data-testid="reply" onClick={() => { actions.reply(); setMenu(false); }}><CornerUpLeft className="size-4 text-muted-foreground" />Reply</button>
                    {m.text ? <button type="button" className={item} data-testid="copy" onClick={copy}><Copy className="size-4 text-muted-foreground" />Copy</button> : null}
                    <button type="button" className={item} data-testid="forward" onClick={() => { actions.forward(); setMenu(false); }}><Forward className="size-4 text-muted-foreground" />Forward</button>
                    {actions.info ? <button type="button" className={item} data-testid="message-info" onClick={() => { actions.info?.(); setMenu(false); }}><Info className="size-4 text-muted-foreground" />Message info</button> : null}
                    {!mine ? <button type="button" className={item} data-testid="report" onClick={() => { actions.report(); setMenu(false); }}><Flag className="size-4 text-muted-foreground" />Report</button> : null}
                    <button type="button" className={item} data-testid="star" onClick={() => { actions.star(); setMenu(false); }}><Star className={cn("size-4", actions.starred ? "fill-amber-400 text-amber-400" : "text-muted-foreground")} />{actions.starred ? "Unstar" : "Star"}</button>
                    <button type="button" className={item} data-testid="pin" onClick={() => { actions.pin(!m.pinned); setMenu(false); }}>{m.pinned ? <PinOff className="size-4 text-muted-foreground" /> : <Pin className="size-4 text-muted-foreground" />}{m.pinned ? "Unpin" : "Pin"}</button>
                    {mine && m.text !== null && (!m.rich || m.rich.kind === "link") ? <button type="button" className={item} data-testid="edit" onClick={() => { setEditing(m.text ?? ""); setMenu(false); }}><Pencil className="size-4 text-muted-foreground" />Edit</button> : null}
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
        {actions.canRetry && m.state === "failed" ? (
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

/**
 * WhatsApp's Group info / Contact info: who it is and quick actions on top, then media,
 * starred messages, settings, the members (searchable, admins marked), and the chat's
 * actions (export, clear, exit, report) at the bottom.
 */
function DetailsPanel({ engine, conversation, title, onClose, shareEnabled, callLinks, onSearch, onOpenStarred, onOpenChat }: {
  engine: MessagingEngine; conversation: ConversationView; title: string; onClose: () => void; shareEnabled: boolean;
  callLinks: { voice: string | null; video: string | null };
  onSearch: () => void;
  onOpenStarred?: () => void;
  onOpenChat?: (id: string) => void;
}) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const client = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(conversation.serverTitle ?? conversation.groupTitle ?? "");
  const [cfMeetId, setCfMeetId] = useState("");
  const [memberQuery, setMemberQuery] = useState("");
  const [searchingMembers, setSearchingMembers] = useState(false);
  const [adding, setAdding] = useState(false);
  const [similar, setSimilar] = useState<string | null>(null);
  const [inviteUrl, setInviteUrl] = useState<string | null>(null);
  const [reporting, setReporting] = useState(false);
  const [confirm, setConfirm] = useState<"clear" | "exit" | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const me = s.identity?.principalId;
  const manager = conversation.myRole === "OWNER" || conversation.myRole === "ADMIN";
  const run = (p: Promise<unknown>, done?: string) => {
    setError(null);
    setNotice(null);
    void p.then(() => { if (done) setNotice(done); }).catch((cause) => setError(messagingErrorMessage(cause)));
  };
  const managed = conversation.securityProfile === "MANAGED";
  const isGroup = conversation.contextClass === "GROUP";
  const normalized = normalizeCfMeetId(cfMeetId);
  const field = "h-10 min-w-0 flex-1 rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary";
  const small = "h-10 shrink-0 rounded-xl px-3.5 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-40";
  const row = "flex w-full items-center gap-4 rounded-xl px-3 py-3 text-left text-[15px] hover:bg-muted disabled:opacity-50";
  const quick = "flex w-20 flex-col items-center gap-1.5 rounded-2xl py-3 text-xs font-medium hover:bg-muted";
  const quickIcon = "flex size-11 items-center justify-center rounded-full bg-muted text-foreground";
  const label = (p: string) => (p === me ? "You" : s.names[p] ?? "CF Meet user");
  const needle = memberQuery.trim().toLocaleLowerCase();
  const members = [...conversation.members]
    .sort((a, b) => (a === me ? -1 : b === me ? 1 : 0) || Number(isAdmin(conversation.memberRoles[b])) - Number(isAdmin(conversation.memberRoles[a])) || label(a).localeCompare(label(b)))
    .filter((p) => !needle || label(p).toLocaleLowerCase().includes(needle));
  const lastSequence = [...(s.messages[conversation.id] ?? [])].reverse().find((m) => m.sequence !== null)?.sequence ?? conversation.lastMessage?.sequence ?? 0;

  const exportChat = async () => {
    // WhatsApp's "Export chat": the whole history as text, fetched page by page first.
    setBusy("export");
    try {
      for (let i = 0; i < 100 && s.hasOlder[conversation.id] !== false && engine.getSnapshot().hasOlder[conversation.id]; i++) await engine.loadOlder(conversation.id);
      const list = engine.getSnapshot().messages[conversation.id] ?? [];
      const lines = list.filter((m) => m.state !== "system" && m.state !== "removed").map((m) => {
        const at = m.createdAt ? new Date(m.createdAt).toLocaleString() : "";
        const who = m.mine ? "You" : m.senderPrincipalId ? (s.names[m.senderPrincipalId] ?? "CF Meet user") : "CF Meet";
        const body = m.text ?? (m.file ? `<${m.file.name}>` : "");
        return `[${at}] ${who}: ${body}`;
      });
      const blob = new Blob([`${title}\n\n${lines.join("\n")}\n`], { type: "text/plain" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `CF Meet chat with ${title.replace(/[\\/:*?"<>|]+/g, " ").trim() || "chat"}.txt`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
    } catch (cause) {
      setError(messagingErrorMessage(cause));
    } finally {
      setBusy(null);
    }
  };
  const clearChat = () => run(hub.clearForMe(conversation.id, lastSequence).then(() => Promise.all([
    client.invalidateQueries({ queryKey: hubKeys.context(conversation.id) }),
    client.invalidateQueries({ queryKey: hubKeys.timeline(conversation.id) }),
  ])), "Chat cleared on your devices.");
  const invite = async () => {
    setBusy("invite");
    setError(null);
    try {
      const link = await engine.createInviteLink(conversation.id, { expiresInSeconds: 7 * 24 * 3600 });
      const url = `${window.location.origin}/dashboard/messages/join?token=${encodeURIComponent(link.token)}`;
      setInviteUrl(url);
      await navigator.clipboard?.writeText(url).catch(() => {});
    } catch (cause) {
      setError(messagingErrorMessage(cause));
    } finally {
      setBusy(null);
    }
  };

  return (
    <aside className="absolute inset-0 z-20 flex flex-col bg-background lg:static lg:w-96 lg:shrink-0 lg:border-l lg:border-border" data-testid="members" aria-label={isGroup ? "Group info" : "Contact info"}>
      <header className="flex h-16 shrink-0 items-center gap-3 border-b border-border px-4">
        <button type="button" aria-label="Close chat info" onClick={onClose} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-5" /></button>
        <h3 className="font-semibold">{isGroup ? "Group info" : "Contact info"}</h3>
      </header>
      <div className="no-scrollbar min-h-0 flex-1 space-y-2 overflow-y-auto pb-6">
        {/* Who it is, and quick actions. */}
        <section className="flex flex-col items-center px-4 pb-3 pt-6 text-center">
          <Avatar name={title} seed={conversation.id} size="size-28" className="text-4xl" />
          {renaming ? (
            <form className="mt-3 flex w-full gap-2" onSubmit={(e) => { e.preventDefault(); run(engine.renameConversation(conversation.id, name).then(() => setRenaming(false))); }}>
              <input autoFocus aria-label="Group name" placeholder="Group name" maxLength={120} value={name} onChange={(e) => setName(e.target.value)} className={field} />
              <button type="submit" className={small} data-testid="rename-group">Save</button>
            </form>
          ) : (
            <p className="mt-3 flex items-center gap-1.5 text-xl font-semibold" data-testid="info-title">
              {title}
              {isGroup && (managed || manager) ? <button type="button" aria-label="Edit group name" onClick={() => setRenaming(true)} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Pencil className="size-4" /></button> : null}
            </p>
          )}
          <p className="text-sm text-muted-foreground" data-testid="info-subtitle">{isGroup ? `Group · ${conversation.members.length} ${conversation.members.length === 1 ? "member" : "members"}` : null}</p>
          {!managed ? <div className="mt-1"><ProfileBadge profile={conversation.securityProfile} /></div> : null}
          <div className="mt-4 flex flex-wrap justify-center gap-1">
            {callLinks.voice ? <Link href={callLinks.voice} className={quick} data-testid="info-voice"><span className={quickIcon}><Phone className="size-5" /></span>Voice</Link> : null}
            {callLinks.video ? <Link href={callLinks.video} className={quick} data-testid="info-video"><span className={quickIcon}><Video className="size-5" /></span>Video</Link> : null}
            {isGroup ? <button type="button" className={quick} onClick={() => setAdding(true)} data-testid="info-add"><span className={quickIcon}><UserPlus className="size-5" /></span>Add</button> : null}
            <button type="button" className={quick} onClick={() => { onSearch(); onClose(); }} data-testid="info-search"><span className={quickIcon}><Search className="size-5" /></span>Search</button>
          </div>
        </section>
        {!managed && !isGroup && conversation.members.length === 2 ? <div className="px-4"><SecurityCode engine={engine} conversationId={conversation.id} name={title} /></div> : null}
        {shareEnabled ? <div className="px-4"><ShareMeeting engine={engine} conversation={conversation} /></div> : null}

        {/* Media, links and docs; notifications; groups in common. */}
        <div className="border-t border-border px-4 pt-4"><ChatInfoExtras conversationId={conversation.id} direct={!isGroup} groupTitle={(id) => s.conversations.find((c) => c.id === id)?.title ?? null} /></div>
        <div className="px-1">
          {onOpenStarred ? <button type="button" className={row} onClick={onOpenStarred} data-testid="info-starred"><Star className="size-5 text-muted-foreground" />Starred messages</button> : null}
        </div>
        <div className="space-y-3 px-4"><ChatListToggles engine={engine} conversation={conversation} pinnedTotal={pinnedCount(s.conversations)} /><DisappearingMessages engine={engine} conversation={conversation} canChange={!isGroup || manager} /></div>

        {isGroup && manager ? (
          <section className="border-t border-border px-4 pt-4" data-testid="group-permissions">
            <p className="mb-2 flex items-center gap-2 text-sm font-semibold"><Settings2 className="size-4 text-muted-foreground" />Group permissions</p>
            <GroupAdminTools engine={engine} conversation={conversation} names={s.names} me={me ?? null} />
          </section>
        ) : null}
        {isGroup ? (
          <div className="border-t border-border px-1 pt-2">
            {similar === null ? (
              <button type="button" className={row} onClick={() => setSimilar(`${title} (new)`)} data-testid="similar-group">
                <Users className="size-5 text-muted-foreground" />
                <span className="min-w-0"><span className="block">Create a similar group</span><span className="block text-xs text-muted-foreground">Start with the same members. You can add or remove people.</span></span>
              </button>
            ) : (
              <form className="flex gap-2 px-3 py-2" onSubmit={(e) => {
                e.preventDefault();
                const others = conversation.members.filter((p) => p !== me);
                run(engine.createConversation([], "MANAGED", { principalIds: others, title: similar.trim() || undefined }).then((id) => { setSimilar(null); onOpenChat?.(id); }));
              }}>
                <input autoFocus aria-label="New group name" value={similar} maxLength={120} onChange={(e) => setSimilar(e.target.value)} className={field} />
                <button type="submit" className={small} disabled={!similar.trim()}>Create</button>
              </form>
            )}
          </div>
        ) : null}

        {/* Members: count, search, add, invite link, admins marked. */}
        <section className="border-t border-border px-1 pt-3">
          <div className="flex items-center justify-between px-3">
            <p className="text-sm font-semibold text-muted-foreground">{conversation.members.length} {conversation.members.length === 1 ? "member" : "members"}</p>
            <button type="button" aria-label="Search members" onClick={() => setSearchingMembers(!searchingMembers)} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Search className="size-4" /></button>
          </div>
          {searchingMembers ? <div className="px-3 pt-2"><input autoFocus aria-label="Search members" placeholder="Search members" value={memberQuery} onChange={(e) => setMemberQuery(e.target.value)} className={cn(field, "w-full")} data-testid="member-search" /></div> : null}
          {isGroup ? (
            <>
              {adding ? <div className="px-3"><AddPeople onAdd={(p) => { setAdding(false); run(p.kind === "contact" ? engine.addMemberByCfMeetId(conversation.id, p.value) : engine.addMember(conversation.id, p.value)); }} /></div>
                : <button type="button" className={row} onClick={() => setAdding(true)} data-testid="add-member"><span className="flex size-10 items-center justify-center rounded-full bg-primary text-primary-foreground"><UserPlus className="size-5" /></span>Add member</button>}
              {manager && managed ? (
                <button type="button" className={row} onClick={() => void invite()} disabled={busy === "invite"} data-testid="invite-link">
                  <span className="flex size-10 items-center justify-center rounded-full bg-primary text-primary-foreground"><Link2 className="size-5" /></span>
                  <span className="min-w-0"><span className="block">Invite to group via link</span>{inviteUrl ? <span className="block truncate text-xs text-muted-foreground" data-testid="invite-url">Copied · {inviteUrl}</span> : null}</span>
                </button>
              ) : null}
            </>
          ) : null}
          <ul className="space-y-0.5">
            {members.map((p) => {
              const role = conversation.memberRoles[p];
              return (
                <li key={p} className="flex items-center gap-3 rounded-xl px-3 py-2 hover:bg-muted/60" data-testid="member-row">
                  <Avatar name={label(p)} seed={p} size="size-10" />
                  <span className="min-w-0 flex-1"><span className="block truncate text-[15px] font-medium">{label(p)}</span>{role === "READ_ONLY" || role === "MODERATOR" ? <span className="block text-xs capitalize text-muted-foreground">{role.toLowerCase().replace("_", " ")}</span> : null}</span>
                  {isGroup && isAdmin(role) ? <span className="shrink-0 rounded-md bg-muted px-2 py-0.5 text-xs font-semibold" data-testid="admin-badge">Group admin</span> : null}
                  {p !== me && isGroup && manager ? (
                    <span className="flex items-center gap-1">
                      <NiceSelect className="w-28" value={role ?? "MEMBER"} onChange={(v) => run(engine.setMemberRole(conversation.id, p, v))} options={["ADMIN", "MODERATOR", "MEMBER", "READ_ONLY"].map((r) => ({ value: r, label: r === "READ_ONLY" ? "Read only" : r === "ADMIN" ? "Admin" : r.charAt(0) + r.slice(1).toLowerCase() }))} />
                      <button type="button" aria-label={`Remove ${label(p)}`} title="Remove" data-testid="remove-member" onClick={() => run(engine.removeMember(conversation.id, p))} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive"><X className="size-4" /></button>
                      {managed ? <button type="button" aria-label={`Remove and ban ${label(p)}`} title="Remove and ban" data-testid="ban-member" onClick={() => run(engine.ban(conversation.id, p).then(() => client.invalidateQueries({ queryKey: ["messaging", "bans", conversation.id] })))} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive"><Ban className="size-4" /></button> : null}
                    </span>
                  ) : null}
                </li>
              );
            })}
            {!members.length ? <li className="px-3 py-3 text-sm text-muted-foreground">No member matches.</li> : null}
          </ul>
          {isGroup ? (
            <form className="mt-2 flex gap-2 px-3" onSubmit={(e) => { e.preventDefault(); if (normalized) run(engine.addMemberByCfMeetId(conversation.id, normalized).then(() => setCfMeetId(""))); }}>
              <input aria-label="Add a contact by CF Meet ID" placeholder="Or add by CF Meet ID" value={cfMeetId} maxLength={40} onChange={(e) => setCfMeetId(e.target.value)} className={field} data-testid="add-member-id" />
              <button type="submit" className={small} disabled={!normalized}>Add</button>
            </form>
          ) : null}
        </section>

        {isGroup && manager && managed ? <BannedMembers engine={engine} conversationId={conversation.id} names={s.names} /> : null}
        {error ? <p role="alert" className="px-4 text-sm text-destructive">{error}</p> : null}
        {notice ? <p role="status" className="px-4 text-sm text-emerald-600">{notice}</p> : null}

        {/* The chat's actions. */}
        <section className="border-t border-border px-1 pt-2">
          <button type="button" className={row} onClick={() => void exportChat()} disabled={busy === "export"} data-testid="export-chat"><Download className="size-5 text-muted-foreground" />{busy === "export" ? "Exporting…" : "Export chat"}</button>
          {confirm === "clear" ? (
            <div className="flex items-center gap-2 px-3 py-2 text-sm"><span className="flex-1">Clear this chat on your devices? Others keep their messages.</span><button type="button" className={cn(small, "text-destructive hover:bg-destructive/10")} onClick={() => { setConfirm(null); clearChat(); }} data-testid="clear-chat-confirm">Clear</button><button type="button" className={small} onClick={() => setConfirm(null)}>Cancel</button></div>
          ) : (
            <button type="button" className={cn(row, "text-destructive")} onClick={() => setConfirm("clear")} disabled={!lastSequence} data-testid="clear-chat"><MinusCircle className="size-5" />Clear chat</button>
          )}
          {isGroup ? (confirm === "exit" ? (
            <div className="flex items-center gap-2 px-3 py-2 text-sm"><span className="flex-1">Exit &ldquo;{title}&rdquo;?</span><button type="button" className={cn(small, "text-destructive hover:bg-destructive/10")} onClick={() => { setConfirm(null); run(engine.leaveConversation(conversation.id)); }} data-testid="leave-conversation">Exit</button><button type="button" className={small} onClick={() => setConfirm(null)}>Cancel</button></div>
          ) : (
            <button type="button" className={cn(row, "text-destructive")} onClick={() => setConfirm("exit")} data-testid="exit-group"><LogOut className="size-5" />Exit group</button>
          )) : null}
          <button type="button" className={cn(row, "text-destructive")} onClick={() => setReporting(true)} data-testid="report-chat"><ThumbsDown className="size-5" />{isGroup ? "Report group" : `Report ${title}`}</button>
        </section>
      </div>
      {reporting ? <ReportDialog engine={engine} conversationId={conversation.id} privateChat={!managed} onClose={() => setReporting(false)} /> : null}
    </aside>
  );
}

/** Group info (admins): who is banned, with Unban. */
function BannedMembers({ engine, conversationId, names }: { engine: MessagingEngine; conversationId: string; names: Record<string, string> }) {
  const client = useQueryClient();
  const key = ["messaging", "bans", conversationId];
  const bans = useQuery({ queryKey: key, queryFn: () => engine.bans(conversationId), retry: false });
  const unban = useMutation({ mutationFn: (p: string) => engine.unban(conversationId, p), onSettled: () => client.invalidateQueries({ queryKey: key }) });
  if (!bans.data?.length) return null;
  return (
    <section className="border-t border-border px-4 pt-3" data-testid="banned-members">
      <p className="mb-1 text-sm font-semibold text-muted-foreground">Banned</p>
      <ul className="space-y-0.5">
        {bans.data.map((b) => (
          <li key={b.principalId} className="flex items-center gap-3 py-1.5 text-sm">
            <Avatar name={names[b.principalId] ?? "CF Meet user"} seed={b.principalId} size="size-8" />
            <span className="min-w-0 flex-1 truncate">{names[b.principalId] ?? "CF Meet user"}</span>
            <button type="button" disabled={unban.isPending} onClick={() => unban.mutate(b.principalId)} className="rounded-full px-3 py-1 text-xs font-semibold text-primary hover:bg-primary/10" data-testid="unban-member">Unban</button>
          </li>
        ))}
      </ul>
    </section>
  );
}

const isAdmin = (role: string | undefined) => role === "OWNER" || role === "ADMIN";

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

/** Groups someone added you to that wait for your answer: join or decline here. */
function GroupInvitations({ engine, names, onOpen }: { engine: MessagingEngine; names: Record<string, string>; onOpen: (id: string) => void }) {
  const client = useQueryClient();
  const list = useQuery({ queryKey: ["messaging", "group-invitations"], queryFn: () => engine.groupInvitations(), refetchInterval: 60_000, retry: false });
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const items = list.data ?? [];
  if (!items.length) return null;
  const answer = (id: string, accept: boolean) => {
    setBusy(id);
    setError(null);
    void engine.answerGroupInvitation(id, accept)
      .then(() => { if (accept) onOpen(id); })
      .catch((e) => setError(messagingErrorMessage(e)))
      .finally(() => { setBusy(null); void client.invalidateQueries({ queryKey: ["messaging", "group-invitations"] }); });
  };
  return (
    <section className="space-y-1.5 rounded-2xl bg-primary/5 p-2.5" data-testid="group-invitations" aria-label="Group invitations">
      <p className="px-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Group invitations</p>
      {error ? <p role="alert" className="px-1 text-xs text-destructive">{error}</p> : null}
      {items.map((inv) => {
        const who = inv.invitedByPrincipalId ? names[inv.invitedByPrincipalId] ?? "Someone" : "Someone";
        return (
          <div key={inv.conversationId} className="flex items-center gap-2 rounded-xl bg-background px-2.5 py-2 ring-1 ring-border" data-testid="group-invitation">
            <Users className="size-5 shrink-0 text-primary" />
            <span className="min-w-0 flex-1 text-sm"><span className="font-medium">{who}</span> added you to a group</span>
            <button type="button" disabled={busy === inv.conversationId} onClick={() => answer(inv.conversationId, false)} className="h-8 rounded-full px-3 text-xs font-semibold ring-1 ring-border hover:bg-muted disabled:opacity-60" data-testid="decline-group-invitation">Decline</button>
            <button type="button" disabled={busy === inv.conversationId} onClick={() => answer(inv.conversationId, true)} className="h-8 rounded-full bg-primary px-3 text-xs font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60" data-testid="accept-group-invitation">Join</button>
          </div>
        );
      })}
    </section>
  );
}

/** The chat list search's "Messages" section: matches in every chat (WhatsApp). */
function AllChatsSearch({ engine, q, conversations, names, me, onOpen }: { engine: MessagingEngine; q: string; conversations: ConversationView[]; names: Record<string, string>; me: string | null; onOpen: (id: string, messageId?: string) => void }) {
  const [hits, setHits] = useState<Awaited<ReturnType<MessagingEngine["searchAll"]>> | null>(null);
  useEffect(() => {
    let active = true;
    const t = setTimeout(() => { void engine.searchAll(q).then((h) => { if (active) setHits(h); }); }, 300);
    return () => { active = false; clearTimeout(t); };
  }, [engine, q]);
  const shown = (hits ?? []).filter((h) => conversations.some((c) => c.id === h.conversationId));
  if (hits === null) return <li className="px-3 pb-1 pt-4 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Searching messages…</li>;
  if (!shown.length) return null;
  const needle = q.trim().toLocaleLowerCase();
  return (
    <>
      <li className="px-3 pb-1 pt-4 text-xs font-semibold uppercase tracking-wider text-muted-foreground" data-testid="search-messages-heading">Messages</li>
      {shown.map((h) => {
        const chat = conversations.find((c) => c.id === h.conversationId)!;
        const who = h.senderPrincipalId === me ? "You" : names[h.senderPrincipalId]?.split(" ")[0] ?? null;
        const text = h.text ?? "";
        const at = text.toLocaleLowerCase().indexOf(needle);
        const start = Math.max(0, at - 24);
        return (
          <li key={`${h.conversationId}:${h.messageId}`}>
            <button type="button" onClick={() => onOpen(h.conversationId, h.messageId)} data-testid="search-message-hit"
              className="flex w-full flex-col gap-0.5 rounded-xl px-3 py-2.5 text-left hover:bg-muted/60">
              <span className="flex items-center justify-between gap-2"><span className="truncate text-[15px] font-medium">{chat.title}</span><span className="shrink-0 text-xs text-muted-foreground">{listTime(h.createdAt)}</span></span>
              <span className="truncate text-sm text-muted-foreground">
                {chat.contextClass === "GROUP" && who ? `${who}: ` : who === "You" ? "You: " : ""}
                {start > 0 ? "…" : ""}{text.slice(start, at < 0 ? 80 : at)}<mark className="rounded bg-primary/20 px-0.5 text-foreground">{at < 0 ? "" : text.slice(at, at + needle.length)}</mark>{at < 0 ? "" : text.slice(at + needle.length, at + needle.length + 60)}
              </span>
            </button>
          </li>
        );
      })}
    </>
  );
}

/** WhatsApp Web's "Get notified of new messages" row, until the browser has decided. */
function NotifyPrompt() {
  const [permission, setPermission] = useState<NotificationPermission | "unsupported">(() =>
    typeof window !== "undefined" && "Notification" in window ? Notification.permission : "unsupported");
  const [dismissed, setDismissed] = useState(() => {
    try { return typeof window !== "undefined" && window.localStorage.getItem(NOTIFY_DISMISSED) === "1"; } catch { return false; }
  });
  if (permission !== "default" || dismissed) return null;
  // Desktop only (WhatsApp Web): on a phone it would push the chat list down for nothing.
  return (
    <div className="hidden items-center gap-1 rounded-2xl bg-primary/10 pr-1 md:flex">
      <button type="button" data-testid="enable-notifications"
        onClick={() => void Notification.requestPermission().then(setPermission).catch(() => setPermission("denied"))}
        className="flex min-w-0 flex-1 items-center gap-3 rounded-2xl px-3 py-2.5 text-left text-sm hover:bg-primary/15">
        <Bell className="size-5 shrink-0 text-primary" />
        <span className="min-w-0 flex-1"><span className="block font-semibold">Get notified of new messages</span><span className="block text-xs text-muted-foreground">Turn on desktop notifications</span></span>
      </button>
      <button type="button" aria-label="Not now" title="Not now" className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-primary/15"
        onClick={() => { setDismissed(true); try { window.localStorage.setItem(NOTIFY_DISMISSED, "1"); } catch { /* not kept */ } }}>
        <X className="size-4" />
      </button>
    </div>
  );
}
const NOTIFY_DISMISSED = "cfmeet:notify-prompt-dismissed";

/**
 * An older end-to-end encrypted chat: it only works on the devices that hold its keys, and
 * messages to someone who has never opened CF Meet wait. It continues as a chat that syncs
 * on every device (same people, same group name); this one stays readable here.
 */
function ContinueChat({ engine, conversation, onOpenChat }: { engine: MessagingEngine; conversation: ConversationView; onOpenChat?: (id: string) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const needsName = conversation.contextClass !== "DIRECT" && !engine.knownGroupName(conversation.id);
  const [name, setName] = useState("");
  return (
    // Stacked: the explanation across the full width, then the name box and the button on
    // one row (a narrow phone never squeezes the text into a column).
    <div className="mb-2 space-y-2 rounded-2xl bg-muted/60 px-3 py-2.5 text-sm" data-testid="continue-chat">
      <p className="flex items-start gap-2 text-muted-foreground">
        <Lock className="mt-0.5 size-4 shrink-0" />
        <span>{error ?? "This older encrypted chat only works on this device. Continue it in a chat that's on all your devices."}</span>
      </p>
      <div className="flex items-center gap-2">
      {needsName ? (
        <input aria-label="Group name" placeholder="Group name" maxLength={120} value={name} onChange={(e) => setName(e.target.value)}
          data-testid="continue-chat-name" className="h-9 min-w-0 flex-1 rounded-full bg-background px-3 text-sm ring-1 ring-border" />
      ) : null}
      <button type="button" disabled={busy || (needsName && !name.trim())} data-testid="continue-chat-button"
        className={cn("h-9 shrink-0 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60", !needsName && "ml-auto")}
        onClick={() => {
          setBusy(true);
          setError(null);
          void engine.continueAsManaged(conversation.id, name)
            .then((id) => onOpenChat?.(id))
            .catch((e) => setError(messagingErrorMessage(e)))
            .finally(() => setBusy(false));
        }}>
        {busy ? "Opening…" : "Continue chat"}
      </button>
      </div>
    </div>
  );
}

function ConversationPane({ engine, conversation, focusMessageId, onFocused, onBack, onOpenChat, onOpenStarred }: { engine: MessagingEngine; conversation: ConversationView; focusMessageId?: string; onFocused?: () => void; onBack?: () => void; onOpenChat?: (id: string) => void; onOpenStarred?: () => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const client = useQueryClient();
  // Drafts stay with their chat (WhatsApp): kept in this browser, per account and chat.
  const draftKey = `cfmeet:draft:${s.identity?.principalId ?? ""}:${conversation.id}`;
  const [text, setText] = useState(() => {
    try { return typeof window === "undefined" ? "" : window.localStorage.getItem(draftKey) ?? ""; } catch { return ""; }
  });
  useEffect(() => {
    try {
      if (text) window.localStorage.setItem(draftKey, text);
      else window.localStorage.removeItem(draftKey);
    } catch { /* storage unavailable: drafts just aren't kept */ }
  }, [draftKey, text]);
  const [recording, setRecording] = useState(false);
  /** A picked file waiting in the preview until you press Send. */
  const [staged, setStaged] = useState<File | null>(null);
  const canRecord = useSyncExternalStore(noSubscribe, voiceRecordingSupported, () => false);
  const [details, setDetails] = useState(false);
  const [searching, setSearching] = useState(false);
  const [replyTo, setReplyTo] = useState<MessageView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const file = useRef<HTMLInputElement>(null);
  const mediaInput = useRef<HTMLInputElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const bottom = useRef<HTMLDivElement>(null);
  const messages = useMemo(() => s.messages[conversation.id] ?? [], [s.messages, conversation.id]);
  // "online" / "last seen" for a one-to-one chat (WhatsApp); groups keep the security badge.
  useEffect(() => { engine.watchPresence(conversation.id); return () => engine.watchPresence(null); }, [engine, conversation.id]);
  const others = Object.values(s.presence[conversation.id] ?? {});
  // Groups list their members under the name ("Ainz, Davidayo, You"), as WhatsApp does.
  const memberLine = conversation.contextClass === "GROUP"
    ? [...conversation.members.filter((m) => m !== s.identity?.principalId).map((m) => s.names[m] ?? "CF Meet user"), "You"].join(", ")
    : null;
  const presenceText = conversation.contextClass === "GROUP" || others.length !== 1 ? null
    : others[0].online ? "online"
    : others[0].lastSeenAt ? lastSeenText(others[0].lastSeenAt) : null;
  const e2ee = conversation.securityProfile === "PRIVATE_E2EE";
  const managed = conversation.securityProfile === "MANAGED";
  // Private chats: you can type as soon as this browser is set up; until the chat's encryption
  // is ready here, messages wait on this device and go out automatically.
  // S13: in an admins-only group, members read but can't send.
  const adminsOnly = conversation.announcementOnly && conversation.myRole !== "OWNER" && conversation.myRole !== "ADMIN";
  // While this browser is set up (silently), private messages can already be written: they
  // are sent automatically once it is ready.
  const canSend = !!s.identity && !adminsOnly && (!e2ee || s.phase === "ready" || MessagingEngine.settingUp(s));
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
  // A direct chat is named after the other person (your alias for them first), never a member list.
  const title = conversation.contextClass === "DIRECT" ? directCounterpartLabel({ counterpart, members: conversation.members, me: s.identity?.principalId, names: s.names }) ?? conversation.title : conversation.title;
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
  // The engine counts new messages as unread unless this chat is on screen (tab visible).
  useEffect(() => {
    const sync = () => engine.setOpenConversation(document.visibilityState === "visible" ? conversation.id : null);
    sync();
    document.addEventListener("visibilitychange", sync);
    return () => { document.removeEventListener("visibilitychange", sync); engine.setOpenConversation(null); };
  }, [engine, conversation.id]);
  // Coming back to the tab or window reads what arrived while it was in the background.
  useEffect(() => {
    const read = () => { if (document.visibilityState === "visible") void engine.markRead(conversation.id); };
    document.addEventListener("visibilitychange", read);
    window.addEventListener("focus", read);
    return () => { document.removeEventListener("visibilitychange", read); window.removeEventListener("focus", read); };
  }, [engine, conversation.id]);
  // WhatsApp scrolling: open at the "unread messages" line (or the end); keep the end in view
  // only while you are at it, otherwise count what arrives below and offer a ↓ button.
  const scroller = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  const shown = useRef(0);
  const [newBelow, setNewBelow] = useState(0);
  const [away, setAway] = useState(false);
  const [highlight, setHighlight] = useState<string | null>(null);
  const [unreadAtOpen] = useState(() => conversation.unreadCount);
  const [divider, setDivider] = useState<string | null | undefined>(undefined);
  if (divider === undefined && messages.length) {
    const incoming = messages.filter((m) => !m.mine && m.sequence !== null);
    setDivider(unreadAtOpen > 0 && incoming.length ? incoming[Math.max(0, incoming.length - unreadAtOpen)].key : null);
  }
  const toBottom = (smooth = false) => {
    bottom.current?.scrollIntoView({ block: "end", behavior: smooth ? "smooth" : "auto" });
    atBottom.current = true;
    setNewBelow(0);
    setAway(false);
  };
  useEffect(() => {
    if (divider === undefined) return;
    const line = divider ? scroller.current?.querySelector('[data-testid="unread-divider"]') : null;
    if (line) { line.scrollIntoView({ block: "start" }); atBottom.current = false; }
    else bottom.current?.scrollIntoView({ block: "end" });
    shown.current = items.length;
    // Only when the chat first has messages: later arrivals are handled below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [divider]);
  const lastKey = useRef<string | null>(null);
  useEffect(() => {
    // Only what arrived AFTER the previous last item counts as new: older messages loaded at
    // the top ("Load older") are not new messages below.
    const previous = lastKey.current;
    lastKey.current = items.at(-1)?.key ?? null;
    const at = previous ? items.findIndex((i) => i.key === previous) : -1;
    const added = at >= 0 ? items.length - 1 - at : items.length - shown.current;
    shown.current = items.length;
    if (added <= 0 || divider === undefined) return;
    const latest = items.at(-1);
    const fromMe = latest?.type === "message" && latest.message.mine;
    const frame = requestAnimationFrame(() => {
      if (atBottom.current || fromMe) toBottom(true);
      else setNewBelow((n) => n + added);
    });
    return () => cancelAnimationFrame(frame);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [items.length]);
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setAway(!atBottom.current);
    if (atBottom.current && newBelow) setNewBelow(0);
  };
  const jump = (messageId: string) => {
    const el = scroller.current?.querySelector(`[data-message-id="${CSS.escape(messageId)}"]`);
    if (!el) { setError("That message is further back than this chat has loaded."); return; }
    el.scrollIntoView({ block: "center", behavior: "smooth" });
    setHighlight(messageId);
    setTimeout(() => setHighlight((h) => (h === messageId ? null : h)), 1600);
  };
  // A search result: older pages load until it's on screen, then it's scrolled to and highlighted.
  useEffect(() => {
    if (!focusMessageId || !messages.length) return;
    let cancelled = false;
    void (async () => {
      for (let page = 0; page < 20 && !cancelled; page++) {
        await new Promise((r) => requestAnimationFrame(r));
        if (scroller.current?.querySelector(`[data-message-id="${CSS.escape(focusMessageId)}"]`)) break;
        if (!engine.getSnapshot().hasOlder[conversation.id]) break;
        await engine.loadOlder(conversation.id).catch(() => {});
      }
      if (cancelled) return;
      jump(focusMessageId);
      onFocused?.();
    })();
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusMessageId, messages.length > 0]);
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
  const [forwarding, setForwarding] = useState<MessageView | null>(null);
  const [infoFor, setInfoFor] = useState<MessageView | null>(null);
  // In bubbles (quotes, reactions) your own messages are "You", as on WhatsApp.
  const myId = s.identity?.principalId;
  const bubbleNames = useMemo(() => (myId ? { ...s.names, [myId]: "You" } : s.names), [s.names, myId]);
  // The chat's photos, in order, for the viewer's previous / next.
  const [viewing, setViewing] = useState<number | null>(null);
  const photos = useMemo(() => messages
    .filter((m) => m.file?.attachmentId && isInlineImage(m.file.mediaType) && m.state !== "removed")
    .map((m) => ({
      key: m.key,
      name: m.file!.name,
      load: () => engine.imageUrl(conversation.id, m),
      download: () => { void download(m).catch(fail); },
    })), [messages, engine, conversation.id, download]);
  const [reporting, setReporting] = useState<{ messageId: string; text: string | null } | null>(null);
  const queryClient = useQueryClient();
  const saved = useQuery({ queryKey: SAVED_KEY, queryFn: () => hub.savedMessages(), retry: false, staleTime: 30_000 });
  const savedIds = useMemo(() => new Map((saved.data?.data ?? []).map((x) => [x.messageId, x.savedId])), [saved.data]);
  const bubbleActions = (m: MessageView): BubbleActions => ({
    forward: () => setForwarding(m),
    info: m.mine && m.sequence != null ? () => setInfoFor(m) : undefined,
    report: () => { if (m.messageId) setReporting({ messageId: m.messageId, text: m.text }); },
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
    canRetry: !!m.outboxKey || engine.canRetryFile(m.key),
    retry: () => { if (m.outboxKey) engine.retrySend(m.outboxKey); else engine.retryFile(conversation.id, m.key); },
    discard: () => { if (m.outboxKey) engine.discardSend(m.outboxKey); else engine.discardFile(conversation.id, m.key); },
    vote: m.messageId && m.rich?.kind === "poll" ? (choices) => { void engine.sendRich(conversation.id, { t: "vote", poll: m.messageId!, choices }).catch(fail); } : undefined,
  });
  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const mentionOptions = mentionQuery === null ? [] : conversation.members
    .filter((p) => p !== s.identity?.principalId && s.names[p] && s.names[p].toLocaleLowerCase().includes(mentionQuery.toLocaleLowerCase()))
    .slice(0, 8);
  const pickMention = (p: string) => {
    const el = composer.current;
    const caret = el?.selectionStart ?? text.length;
    const start = text.slice(0, caret).lastIndexOf("@");
    if (start < 0) return;
    const inserted = `@${s.names[p]} `;
    const next = text.slice(0, start) + inserted + text.slice(caret);
    setText(next);
    setMentionQuery(null);
    requestAnimationFrame(() => { el?.focus(); el?.setSelectionRange(start + inserted.length, start + inserted.length); });
  };
  const mentions = (t: string) =>
    conversation.members.filter((p) => p !== s.identity?.principalId && s.names[p] && t.includes(`@${s.names[p]}`));
  const send = () => {
    const t = text;
    if (!t.trim() || !canSend) return;
    const reply = replyTo?.messageId ?? null;
    setText("");
    setReplyTo(null);
    setError(null);
    engine.typing(conversation.id, false);
    // @mentions: only the mentioned members' ids reach the server (for their mention badge and
    // "only @mentions" notifications); in a private chat the text stays end-to-end encrypted.
    void engine.send(conversation.id, t, { replyToMessageId: reply, mentionedPrincipalIds: mentions(t) }).catch((cause) => {
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
      {viewing !== null && viewing >= 0 ? <PhotoViewer items={photos} index={Math.min(viewing, photos.length - 1)} onIndex={setViewing} onClose={() => setViewing(null)} /> : null}
      {infoFor ? <MessageInfo engine={engine} conversationId={conversation.id} message={infoFor} names={s.names} onClose={() => setInfoFor(null)} /> : null}
      {forwarding ? <ForwardDialog engine={engine} conversations={s.conversations} sourceId={conversation.id} message={forwarding} onClose={() => setForwarding(null)} /> : null}
      {reporting ? <ReportDialog engine={engine} conversationId={conversation.id} message={reporting} privateChat={e2ee} onClose={() => setReporting(null)} /> : null}
      <section className="flex min-w-0 flex-1 flex-col" data-testid="conversation" data-conversation-id={conversation.id} data-profile={conversation.securityProfile}>
        <header className="flex h-16 shrink-0 items-center gap-2 border-b border-border px-2 sm:px-4">
          {onBack ? (
            <button type="button" className={cn(iconBtn, "md:hidden")} onClick={onBack} aria-label="Back to conversations" data-testid="back-to-conversations"><ArrowLeft className="size-5" /></button>
          ) : null}
          <button type="button" onClick={() => setDetails(!details)} className="flex min-w-0 flex-1 items-center gap-3 rounded-xl px-1 py-1 text-left hover:bg-muted/50">
            <Avatar name={title} seed={conversation.id} size="size-10" />
            <span className="min-w-0">
              <span className="block truncate font-semibold" data-testid="conversation-title">{title}</span>
              <span className="block truncate">{typing.length ? <span className="text-xs font-medium text-primary">{typing.join(", ")} {typing.length === 1 ? "is" : "are"} typing…</span> : presenceText ? <span className={cn("text-xs", presenceText === "online" ? "font-medium text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")} data-testid="presence">{presenceText}</span> : memberLine && !e2ee ? <span className="text-xs text-muted-foreground" data-testid="group-members">{memberLine}</span> : <ProfileBadge profile={conversation.securityProfile} />}</span>
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

        <div className="chat-wallpaper relative flex min-h-0 flex-1 flex-col">
        <div ref={scroller} onScroll={onScroll} className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-3 py-2 sm:px-6">
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
                  {divider && m.key === divider ? (
                    <li className="my-3 flex justify-center" data-testid="unread-divider"><span className="rounded-full bg-background/90 px-3 py-1 text-xs font-semibold text-primary shadow-sm ring-1 ring-border">{unreadAtOpen === 1 ? "1 unread message" : `${unreadAtOpen} unread messages`}</span></li>
                  ) : null}
                  <MessageBubble m={m} managed={managed} names={bubbleNames} name={senderName(m)} group={group} first={first} last={last} actions={bubbleActions(m)}
                    onDownload={() => { setError(null); void download(m).catch(fail); }}
                    loadImage={m.file?.attachmentId && (isInlineImage(m.file.mediaType) || isPlayableAudio(m.file.mediaType) || isPlayableVideo(m.file.mediaType)) ? () => engine.imageUrl(conversation.id, m) : null}
                    cachedMedia={engine.cachedMediaUrl(m)}
                    onOpenPhoto={m.file && isInlineImage(m.file.mediaType) ? () => setViewing(photos.findIndex((p) => p.key === m.key)) : undefined}
                    onJump={jump} highlight={!!m.messageId && highlight === m.messageId} />
                </Fragment>
              );
            })}
          </ol>
          <div ref={bottom} />
        </div>
        {newBelow > 0 || away ? (
          <button type="button" onClick={() => toBottom(true)} aria-label={newBelow ? `${newBelow} new messages` : "Go to latest"} data-testid="scroll-to-latest"
            className={cn("absolute bottom-3 right-3 z-10 flex size-11 items-center justify-center rounded-full border border-border bg-background text-muted-foreground shadow-lg transition-opacity hover:text-foreground sm:right-6")}>
            <ArrowDown className="size-5" />
            {newBelow ? <span className="absolute -top-1.5 -right-1 flex h-5 min-w-5 items-center justify-center rounded-full bg-primary px-1 text-[11px] font-bold text-primary-foreground">{newBelow > 99 ? "99+" : newBelow}</span> : null}
          </button>
        ) : null}

        {error ? <p role="alert" className="mx-3 mb-2 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive sm:mx-6">{error}</p> : null}
        <div className="shrink-0 px-3 pb-3 sm:px-6 sm:pb-4">
          <div className="mx-auto max-w-3xl">
            {e2ee ? <ContinueChat engine={engine} conversation={conversation} onOpenChat={onOpenChat} /> : null}
            {mentionQuery !== null && mentionOptions.length ? (
              <ul className="mb-2 max-h-56 overflow-y-auto rounded-2xl border border-border bg-popover py-1 shadow-lg" data-testid="mention-picker" role="listbox" aria-label="Mention someone">
                {mentionOptions.map((p) => (
                  <li key={p}>
                    <button type="button" role="option" aria-selected={false} onMouseDown={(e) => { e.preventDefault(); pickMention(p); }}
                      className="flex w-full items-center gap-3 px-3 py-2 text-left text-sm hover:bg-muted" data-testid="mention-option">
                      <Avatar name={s.names[p]} seed={p} size="size-8" />{s.names[p]}
                    </button>
                  </li>
                ))}
              </ul>
            ) : null}
            {replyTo ? (
              <div className="mb-2 flex items-center gap-3 rounded-2xl bg-muted/60 py-2 pl-3 pr-1.5 text-sm" data-testid="reply-banner">
                <CornerUpLeft className="size-4 shrink-0 text-primary" />
                <span className="min-w-0 flex-1"><span className="block text-xs font-semibold text-primary">Replying to {replyTo.mine ? "yourself" : (replyTo.senderPrincipalId && s.names[replyTo.senderPrincipalId]) || "message"}</span><span className="block truncate text-muted-foreground">{replyTo.text ?? ""}</span></span>
                <button type="button" aria-label="Cancel reply" onClick={() => setReplyTo(null)} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-4" /></button>
              </div>
            ) : null}
            {recording ? (
              <VoiceRecorder onDone={() => setRecording(false)} onError={setError}
                onSend={(f, ms) => {
                  const tooLarge = attachmentSizeRefusal(f.size, attachments);
                  setError(tooLarge);
                  if (!tooLarge) void engine.sendFile(conversation.id, f, ms).catch(fail);
                }} />
            ) : (
            <form className="flex items-end gap-1.5 rounded-[26px] border border-border bg-card p-1.5 shadow-sm focus-within:border-primary/50" onSubmit={(e) => { e.preventDefault(); send(); }}>
              <AttachmentPreview file={staged} to={title} onCancel={() => setStaged(null)}
                onSend={(f, caption) => { setStaged(null); void engine.sendFile(conversation.id, f, undefined, caption).catch(fail); }} />
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
                    if (!tooLarge) setStaged(f);
                  }
                  e.target.value = "";
                }}
              />
              <input
                ref={mediaInput}
                type="file"
                className="hidden"
                data-testid="media-input"
                accept="image/*,video/*"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) {
                    const tooLarge = attachmentSizeRefusal(f.size, attachments);
                    setError(tooLarge);
                    if (!tooLarge) setStaged(f);
                  }
                  e.target.value = "";
                }}
              />
              <EmojiPicker disabled={!canSend} onPick={(emoji) => {
                // At the cursor, as WhatsApp does; the composer keeps focus.
                const el = composer.current;
                const at = el ? el.selectionStart ?? text.length : text.length;
                const end = el ? el.selectionEnd ?? at : at;
                const next = text.slice(0, at) + emoji + text.slice(end);
                setText(next);
                requestAnimationFrame(() => { el?.focus(); el?.setSelectionRange(at + emoji.length, at + emoji.length); });
              }} />
              <AttachMenu canAttach={canAttach} canShare={e2ee} disabled={!canSend} onFile={() => file.current?.click()} onMedia={() => mediaInput.current?.click()} onSend={(payload) => engine.sendRich(conversation.id, payload)}
                fileTitle={attachments.enabled ? "Attach a file" : "Sending files isn't available in this chat"} />
              <textarea
                ref={composer}
                rows={1}
                data-testid="composer"
                value={text}
                maxLength={16384}
                disabled={!canSend}
                placeholder={canSend ? "Type a message" : adminsOnly ? "Only admins can send messages" : composerBlockedReason(s)}
                onChange={(e) => {
                  setText(e.target.value);
                  engine.typing(conversation.id, e.target.value.length > 0);
                  // "@" in a group opens the member list (WhatsApp), filtered by what follows.
                  const before = e.target.value.slice(0, e.target.selectionStart ?? e.target.value.length);
                  const at = conversation.contextClass === "GROUP" ? /(^|\s)@([^\s@]{0,30})$/.exec(before) : null;
                  setMentionQuery(at ? at[2] : null);
                }}
                onKeyDown={(e) => {
                  if (mentionQuery !== null && mentionOptions.length && (e.key === "Enter" || e.key === "Tab")) { e.preventDefault(); pickMention(mentionOptions[0]); return; }
                  if (mentionQuery !== null && e.key === "Escape") { e.preventDefault(); setMentionQuery(null); return; }
                  if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); }
                  // Esc cancels a reply first (WhatsApp), then closes an open panel.
                  else if (e.key === "Escape" && replyTo) { e.preventDefault(); setReplyTo(null); }
                  else if (e.key === "Escape" && (details || searching)) { e.preventDefault(); setDetails(false); setSearching(false); }
                  // ↑ in an empty composer: edit your last message.
                  else if (e.key === "ArrowUp" && !text && !e.shiftKey && !e.altKey && !e.metaKey && !e.ctrlKey) {
                    const last = [...messages].reverse().find((x) => x.mine && x.messageId && x.text !== null && (x.state === "delivered" || x.state === "sent") && (!x.rich || x.rich.kind === "link"));
                    if (last?.messageId) { e.preventDefault(); window.dispatchEvent(new CustomEvent(EDIT_EVENT, { detail: last.messageId })); }
                  }
                }}
                className="max-h-40 min-h-10 min-w-0 flex-1 resize-none bg-transparent px-1 py-2.5 text-[15px] leading-5 outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed"
              />
              {!text.trim() && canAttach && canRecord ? <MicButton disabled={!canSend} onClick={() => { setError(null); setRecording(true); }} /> : (
                <button type="submit" data-testid="send" disabled={!canSend || !text.trim()} aria-label="Send" className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-opacity hover:bg-primary/90 disabled:opacity-30"><SendHorizontal className="size-5" /></button>
              )}
            </form>
            )}
            <p className="mt-1.5 hidden text-center text-[11px] text-muted-foreground sm:block">{e2ee ? <><Lock className="mr-1 inline size-3" />End-to-end encrypted</> : "Enter to send · Shift + Enter for a new line"}</p>
          </div>
        </div>
        </div>
      </section>
      {details ? <DetailsPanel engine={engine} conversation={conversation} title={title} onClose={() => setDetails(false)} shareEnabled={!!actions.shareMeeting}
        callLinks={{ voice: actions.voiceCall ? callHref({ conversationId: conversation.id }, "VOICE", title) : null, video: actions.videoCall ? callHref({ conversationId: conversation.id }, "VIDEO", title) : null }}
        onSearch={() => setSearching(true)} onOpenStarred={onOpenStarred} onOpenChat={onOpenChat} /> : null}
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

/** Window event: open the editor of one of your messages (detail: its messageId). */
const EDIT_EVENT = "cfmeet:edit-message";

type ListFilter = "ALL" | "UNREAD" | "STATUS" | "GROUPS" | "PRIVATE_E2EE";

function PersonalMessages() {
  const engine = useEngine();
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const params = useSearchParams();
  const requested = params.get("conversation");
  const [selected, setSelected] = useState<string | null>(requested);
  /** A search result to show once its chat is open. */
  const [focus, setFocus] = useState<{ conversationId: string; messageId: string } | null>(null);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<ListFilter>("ALL");
  const [securityOpen, setSecurityOpen] = useState(false);
  const [composeOpen, setComposeOpen] = useState(false);
  const [shareMode, setShareMode] = useState<"status" | "broadcast" | null>(null);
  const [starredOpen, setStarredOpen] = useState(false);
  const [showArchived, setShowArchived] = useState(false);
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const inbox = useHubInbox(areas.contacts || areas.managedMessaging);
  const muted = useMemo(() => new Set((inbox.data?.data ?? []).filter((e) => e.notifications === "MUTED").map((e) => e.conversationId)), [inbox.data]);
  useEffect(() => {
    // Contact -> conversation navigation: a chat created elsewhere appears after a refresh.
    if (requested && !s.conversations.some((c) => c.id === requested) && s.phase !== "loading") void engine.refreshConversations().catch(() => {});
  }, [engine, requested, s.conversations, s.phase]);
  const current = s.conversations.find((c) => c.id === selected) ?? null;
  // Someone took you out of the chat you had open: say so instead of it just vanishing.
  const removedHere = !current && selected ? s.removedFrom?.find((r) => r.conversationId === selected) ?? null : null;
  const unreadTotal = s.conversations.filter((c) => c.unreadCount > 0 && c.id !== selected).length;
  // Keep the number on the Chats tab in step with this list.
  const tabCounter = useQueryClient();
  useEffect(() => { tabCounter.setQueryData(["messaging", "unread-chats"], unreadTotal); }, [tabCounter, unreadTotal]);
  const archivedChats = s.conversations.filter((c) => c.archived);
  const statusCopies = new Set<string>();
  const conversations = arrangeChats(s.conversations, showArchived && archivedChats.length > 0).filter((conversation) => {
    const post = statusPostId(conversation.groupTitle);
    if (post) {
      if (filter !== "STATUS" || !isLiveStatus(conversation) || statusCopies.has(post)) return false;
      statusCopies.add(post);
    } else if (filter === "STATUS") return false;
    const matchesSearch = conversation.title.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase());
    const matchesFilter = filter === "ALL" || filter === "STATUS"
      || (filter === "UNREAD" ? conversation.unreadCount > 0 && conversation.id !== selected
        : filter === "GROUPS" ? conversation.contextClass === "GROUP"
        : conversation.securityProfile === filter);
    return matchesSearch && matchesFilter;
  });
  // WhatsApp's filters; "Private" only while the account still has older encrypted chats.
  const hasPrivate = s.conversations.some((c) => c.securityProfile === "PRIVATE_E2EE");
  const filters: [ListFilter, string][] = [["ALL", "All"], ["UNREAD", unreadTotal ? `Unread ${unreadTotal}` : "Unread"], ["GROUPS", "Groups"], ["STATUS", "Status"], ...(hasPrivate ? [["PRIVATE_E2EE", "Private"] as [ListFilter, string]] : [])];

  return (
    <div className="flex h-full min-h-[32rem] flex-col bg-background" data-testid="messages-page">
      <ConnectionBanner engine={engine} />
      {s.phase === "locked" ? (
        // Only private (end-to-end encrypted) chats are limited to one tab; everything else works here.
        <section className="flex flex-wrap items-center gap-3 border-b border-border bg-muted/40 px-4 py-2.5 text-sm" data-testid="private-locked">
          <p className="min-w-0 flex-1 text-muted-foreground">Your private chats are open in another tab. Your other chats work here.</p>
          <button type="button" onClick={() => window.location.reload()} className="h-8 rounded-full px-3 text-sm font-semibold ring-1 ring-border hover:bg-muted">Use private chats here</button>
        </section>
      ) : null}
      <IncomingTransfers engine={engine} />
      <IncomingLinks engine={engine} />
      {s.phase !== "loading" && s.phase !== "ready" && !MessagingEngine.settingUp(s) ? (
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
                {s.phase === "ready" ? <>
                  <button type="button" aria-label="Post a status" title="Post a status" onClick={() => setShareMode("status")} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><History className="size-5" /></button>
                  <button type="button" aria-label="Send a broadcast" title="Send a broadcast" onClick={() => setShareMode("broadcast")} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><SendHorizontal className="size-5" /></button>
                </> : null}
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
            <NotifyPrompt />
            <GroupInvitations engine={engine} names={s.names} onOpen={setSelected} />
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
            {s.phase === "loading" ? null : showArchived && archivedChats.length ? <ArchivedHeader onBack={() => setShowArchived(false)} />
              : <ArchivedRow count={archivedChats.length} unread={archivedChats.filter((c) => c.unreadCount > 0 && c.id !== selected).length} onOpen={() => setShowArchived(true)} />}
            {s.phase === "loading" ? (
              [0, 1, 2, 3].map((i) => <li key={i} className="flex items-center gap-3 px-3 py-3"><span className="size-12 animate-pulse rounded-full bg-muted" /><span className="flex-1 space-y-2"><span className="block h-3.5 w-1/2 animate-pulse rounded bg-muted" /><span className="block h-3 w-3/4 animate-pulse rounded bg-muted" /></span></li>)
            ) : conversations.map((c) => {
              const latest = s.messages[c.id]?.at(-1);
              const summary = c.lastMessage;
              // A private message this browser can still receive reads "Waiting for this message";
              // one sent before it joined reads so. The bar only shows while a readable chat loads.
              const preview = chatPreview({ e2ee: c.securityProfile === "PRIVATE_E2EE", groupReady: c.groupReady, loaded: s.messages[c.id] !== undefined, latest, summary, joinedAt: s.deviceRegisteredAt });
              const at = latest?.createdAt ?? summary?.createdAt ?? null;
              const fileKind: AttachmentKind | "file" | null = latest ? (latest.file ? attachmentKind(latest.file) : null)
                : summary?.attachmentCount && summary.lifecycle === "ACTIVE" && c.securityProfile !== "PRIVATE_E2EE" ? // The summary only has text: a file name says the kind, a caption doesn't (a paperclip then).
                  (summary.previewText && /\.\w{2,5}$/.test(summary.previewText) ? attachmentKind({ name: summary.previewText }) : "file") : null;
              const FileIcon = fileKind ? FILE_ICONS[fileKind] : null;
              const mine = latest ? latest.mine : summary?.senderPrincipalId === s.identity?.principalId;
              // Groups name who wrote the latest message ("Ainz: hi"), as WhatsApp does.
              const senderId = latest?.senderPrincipalId ?? summary?.senderPrincipalId ?? null;
              const senderName = c.contextClass === "GROUP" && senderId ? (s.names[senderId] ?? "").split(" ")[0] || null : null;
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
                          {c.pinned ? <Pin className="size-3.5 shrink-0 text-muted-foreground" aria-label="Pinned" /> : null}
                        </span>
                        {at ? <time className={cn("shrink-0 text-xs", unread ? "font-semibold text-primary" : "text-muted-foreground")}>{listTime(at)}</time> : null}
                      </span>
                      <span className="mt-0.5 flex items-center justify-between gap-2">
                        <span className={cn("flex min-w-0 items-center gap-1 text-sm", unread ? "font-medium text-foreground" : "text-muted-foreground")}>
                          {s.typing[c.id]?.length ? <span className="truncate font-medium text-primary">{c.contextClass === "GROUP" ? `${s.names[s.typing[c.id][0]]?.split(" ")[0] ?? "Someone"} is typing…` : "typing…"}</span> : <>
                          {mine && latest && (latest.state === "queued" || latest.state === "sending" || latest.state === "pending-setup") ? <Clock3 className="size-3.5 shrink-0" aria-label="Sending" /> : null}
                          {mine && latest && (latest.state === "sent" || latest.state === "delivered") ? <Ticks receipt={latest.seenBy?.length ? "read" : latest.receipt ?? "sent"} list /> : null}
                          {mine && !latest && summary && summary.sequence > 0 && summary.lifecycle === "ACTIVE" ? <Ticks receipt={summaryReceipt(engine.messageInfo(c.id, summary.sequence))} list /> : null}
                          {latest?.state === "failed" ? <span className="shrink-0 text-destructive">Not sent ·</span> : null}
                          {preview === null
                            ? <span aria-label="Loading message" className="h-3 w-32 animate-pulse rounded-full bg-muted" />
                            : <>{FileIcon ? <FileIcon className="size-4 shrink-0" aria-hidden /> : null}<span className="truncate">{mine && preview && !latest ? "You: " : ""}{!mine && preview && senderName ? `${senderName}: ` : ""}{preview}</span></>}
                          </>}
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
            {search.trim().length >= 2 ? <AllChatsSearch engine={engine} q={search} conversations={s.conversations} names={s.names} me={s.identity?.principalId ?? null} onOpen={(id, messageId) => { setSelected(id); setFocus(messageId ? { conversationId: id, messageId } : null); }} /> : null}
          </ul>
          {/* WhatsApp-style floating "new chat" button on phones. */}
          <button type="button" aria-label="New chat" title="New chat" disabled={!s.identity} onClick={() => setComposeOpen(true)}
            className="fixed bottom-24 right-4 z-30 flex size-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-xl shadow-primary/30 hover:bg-primary/90 disabled:opacity-40 md:hidden">
            <MessageSquarePlus className="size-6" />
          </button>
        </aside>
        <main className={cn("min-w-0 min-h-0 flex-1", !current && (removedHere ? "flex items-center justify-center" : "hidden md:flex md:items-center md:justify-center"))} data-testid="conversation-pane">
          {current ? (
            <ConversationPane key={current.id} engine={engine} conversation={current} focusMessageId={focus?.conversationId === current.id ? focus.messageId : undefined} onFocused={() => setFocus(null)} onBack={() => setSelected(null)} onOpenChat={setSelected} onOpenStarred={() => setStarredOpen(true)} />
          ) : removedHere ? (
            <div className="max-w-sm px-6 text-center" role="status" data-testid="removed-from-chat">
              <div className="mx-auto flex size-16 items-center justify-center rounded-full bg-muted"><MessageSquarePlus className="size-7 text-muted-foreground" /></div>
              <h2 className="mt-5 text-lg font-semibold">You&apos;re no longer in &ldquo;{removedHere.title}&rdquo;</h2>
              <p className="mt-2 text-sm text-muted-foreground">An admin removed you, so you can&apos;t see or send messages in it any more.</p>
              <button type="button" onClick={() => setSelected(null)} className="mt-5 inline-flex h-10 items-center rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Back to chats</button>
            </div>
          ) : (
            <div className="max-w-sm px-6 text-center">
              <div className="relative mx-auto flex size-24 items-center justify-center">
                <span className="absolute inset-0 rounded-[2rem] bg-primary/10" />
                <span className="absolute -right-2 -top-2 size-10 rounded-2xl bg-primary/15" />
                <MessageSquarePlus className="relative size-10 text-primary" />
              </div>
              <h2 className="mt-6 text-xl font-semibold">Pick up a conversation</h2>
              <p className="mt-2 text-sm text-muted-foreground">Choose a chat on the left, or start a new one. Your chats are on every device you sign in on.</p>
              {s.identity ? <button type="button" onClick={() => setComposeOpen(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><MessageSquarePlus className="size-4" />New chat</button> : null}
            </div>
          )}
        </main>
      </div>
      {s.phase === "error" ? <p className="border-t border-border px-4 py-2 text-sm text-destructive" data-testid="messaging-error">Messaging error: {s.detail}</p> : null}

      <NewConversation engine={engine} onCreated={setSelected} open={composeOpen} setOpen={setComposeOpen} />
      {shareMode ? <PrivateShares engine={engine} mode={shareMode} onClose={() => { if (shareMode === "status") setFilter("STATUS"); setShareMode(null); }} /> : null}
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

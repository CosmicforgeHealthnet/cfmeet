/**
 * The CF Meet web Messaging client (Part V Milestone B): one engine per signed-in account
 * and tab, driving
 *
 * * the device worker (`crypto-client.ts`) — registration, MLS groups, encryption,
 *   encrypted local history, key transparency, history transfer;
 * * the Meeting Service relay (`api.ts`) — every REST operation, acting for the
 *   account's own principal;
 * * the realtime gateway (`services/messaging-realtime.ts`) — device-bound client token
 *   (an E2EE device) or, without one, a MANAGED-only session-device token (ADR 0049 A1);
 *   single-use ticket; resume from the server's per-device ack cursor (MSG-018).
 *   PRIVATE_E2EE events trigger an authoritative REST sync; MANAGED events carry the
 *   message's current representation and are applied directly (edits, tombstones), with a
 *   REST replay on gaps, so duplicates, gaps and reconnects converge on the server's order.
 * * the durable MANAGED outbox (`outbox.ts`) — offline sends queued in this browser and
 *   retried with the same clientMessageId (idempotent, MSG-008 / MSG-019).
 *
 * Security rules this file keeps (and the browser tests check): a PRIVATE_E2EE
 * conversation is never downgraded or sent in the clear (no MANAGED fallback); the UI is
 * told the conversation's real profile; a KeyPackage is added only after its device and
 * key were verified (and, with a pinned log key, found in the transparency log); message
 * text is only ever rendered as text.
 */
import { MessagingRealtimeTransport, type MessagingRealtimeEvent } from "@/services/messaging-realtime";
import { messagingClientAccessToken, messagingSessionDeviceToken } from "@/services/messaging-session";

import { messagingService as identityApi, relay } from "@/services/v2/messaging.service";
import { b64u } from "./api";
import { MessagingApiError } from "./errors";
import { accountDevice } from "@/lib/auth/account-device";
import type { LinkView } from "@/lib/auth/account-device-core";
import { accountSession } from "@/lib/auth/session";
import type { Colleague, MessagingIdentity } from "@/types/messaging";
import { CryptoClient, CryptoWorkerError, terminateMessagingWorkers, type StoredRecord } from "./crypto-client";
import { cancelLocalAuthenticatorPrompts, createLocalProtection, LocalAuthenticatorError, unlockLocalProtection, type LocalProtectionBinding, type LocalProtectionDescriptor } from "./local-authenticator";
import { ManagedConversationState, emptyRefresh, type ConversationEvent, type ManagedRecord, type Refresh, type ServerMessage as ManagedServerMessage } from "./managed-store";
import { MessageOutbox, browserOutboxStorage, flushOutbox, type OutboxItem } from "./outbox";
import { reactionSegment } from "./routes";
import { messagingRealtimeOrigin } from "./urls";
import { browserDeviceLabel } from "@/lib/auth/device-label";
import { KEY_PACKAGE_TARGET, WAITING_NOTE, beforeJoinNote, joinerEntry, keyPackagesToPublish, missingDevices, removalPlan } from "./e2ee-membership";
import { inboxFlags, type InboxFlags } from "./chat-list";
import { linkPreviewFor, richContent, richSummary, privateReactionState, type PrivateReactionRecord, type RichContent, type RichPayload, type VoteRecord } from "./rich-content";
import { conversationTitle, decodeGroupTitle, encodeGroupTitle, groupTitleFromRecords, isGroupTitleText, normalizeTitle } from "./conversation-title";
import { statusPostId } from "./private-shares";
import { allDevicePages, allMemberPages } from "./member-pages";
import { PrivateHistoryWindow, type PrivateHistoryPage } from "./private-history";

export type SecurityProfile = "MANAGED" | "PRIVATE_E2EE";
/** `needs-signin`: this sign-in is too old to register the browser; the standard sign-in flow takes over. */
const START_BACKOFF_MS = 2_000;
const START_BACKOFF_MAX_MS = 60_000;

/** Errors worth retrying on their own: offline, timeouts, rate limits, server unavailable. */
export function isTransientStartError(e: unknown): boolean {
  if (e instanceof TypeError) return true;
  if (!(e instanceof MessagingApiError)) return false;
  if (e.code === "RATE_LIMITED" || e.code === "SERVICE_UNAVAILABLE") return true;
  return e.status === 0 ? e.code === "NETWORK_UNAVAILABLE" : e.status === 408 || e.status === 429 || e.status >= 500;
}

export type Phase = "loading" | "needs-device" | "registering" | "needs-signin" | "needs-unlock" | "ready" | "locked" | "revoked" | "unavailable" | "error";
export type Connection = "connected" | "connecting" | "disconnected" | "auth-required" | "offline";
/** How live updates arrive: an E2EE device token, a MANAGED-only session device, or polling. */
export type RealtimeMode = "device" | "session" | "none";
export type ReactionView = { reaction: string; count: number; reactedByMe: boolean };
export type PinView = { messageId: string; sequence: number; pinnedByPrincipalId: string; pinnedAt: string; expiresAt: string | null };
export type MemberRole = "OWNER" | "ADMIN" | "MODERATOR" | "MEMBER" | "READ_ONLY" | "BOT_SERVICE" | string;
export type AttachmentMeta = { name: string; mediaType: string; size: number | null; state: string; kind: string };
export type SearchHit = { messageId: string; sequence: number; senderPrincipalId: string; createdAt: string; text: string | null };

/** S12: a group invite link (the token is only returned when it's created). */
export type InviteLink = { inviteLinkId: string; expiresAt: string | null; maxUses: number | null; useCount: number; revokedAt: string | null; status: string; role: string };

export type ConversationView = {
  id: string;
  contextClass: string;
  securityProfile: SecurityProfile;
  serverCanReadContent: boolean;
  lifecycleState: string;
  mlsGroupId: string | null;
  /** This device holds the conversation's MLS group (it can read and send). */
  groupReady: boolean;
  createdAt: string;
  members: string[];
  expiresAt: string | null;
  title: string;
  transparency: "VERIFIED" | "UNPINNED" | "NOT_APPLICABLE";
  version: number;
  ownerPrincipalId: string | null;
  /** The MANAGED conversation title set by a member (null: derived from the members). */
  serverTitle: string | null;
  /** A PRIVATE_E2EE group's name, read from its encrypted group on this device (never held by the server). */
  groupTitle: string | null;
  avatarRef: string | null;
  unreadCount: number;
  mentionCount: number;
  lastActivityAt: string;
  lastMessage: { sequence: number; senderPrincipalId: string; previewText: string | null; attachmentCount: number; lifecycle: string; createdAt: string } | null;
  memberRoles: Record<string, MemberRole>;
  myRole: MemberRole | null;
  /** Only owners and admins can send (S13). */
  announcementOnly: boolean;
  /** Disappearing messages: seconds a new message is kept (null: off). */
  retentionSeconds: number | null;
} & InboxFlags;

export type MessageView = {
  key: string;
  sequence: number | null;
  senderPrincipalId: string | null;
  mine: boolean;
  createdAt: string | null;
  profile: SecurityProfile;
  /** `pending-setup`: held (sealed) on this device until another member has a messaging device; never shown as sent. */
  state: "queued" | "pending-setup" | "sending" | "sent" | "delivered" | "failed" | "unreadable" | "removed" | "before-join" | "system";
  text: string | null;
  file: { name: string; mediaType: string; size: number | null; attachmentId: string | null; state?: string } | null;
  note: string | null;
  transferred: boolean;
  /** Server message id (MANAGED; null while queued or for E2EE records). */
  messageId?: string | null;
  /** Outbox key of a queued / failed MANAGED send (retry / discard). */
  outboxKey?: string | null;
  editCount?: number;
  replyTo?: { messageId: string; senderPrincipalId: string | null; text: string | null; removed: boolean } | null;
  reactions?: ReactionView[];
  pinned?: boolean;
  /** WhatsApp ticks for your own messages: sent (one), delivered (two), read (two, coloured). */
  receipt?: "sent" | "delivered" | "read";
  /** Members whose read receipt reached this message (only computed for your latest message). */
  seenBy?: string[];
  announcement?: boolean;
  /** F4-F7: a private chat's poll, location, contact card or link preview (encrypted content). */
  rich?: RichContent | null;
};

export type IncomingTransfer = {
  transferId: string;
  targetDeviceId: string;
  comparisonCode: string;
  expiresAt: string;
};

/** A link request from a new device, shown on a trusted device (code typed by the user). */
export type IncomingLink = { linkId: string; platform: string; label: string | null; expiresAt: string; keyThumbprint: string };

export type EngineSnapshot = {
  /** Per conversation: who is online and when others were last seen. */
  presence: Record<string, Record<string, { online: boolean; lastSeenAt: string | null }>>;
  phase: Phase;
  /** When this browser's messaging device was registered (messages sent earlier never reach it). */
  deviceRegisteredAt: string | null;
  /** New devices of the account waiting for this (trusted) browser's approval. */
  incomingLinks: IncomingLink[];
  /** Encrypted conversations in which no other member has a messaging device yet. */
  awaitingSetup: Record<string, boolean>;
  detail: string | null;
  connection: Connection;
  identity: MessagingIdentity | null;
  deviceId: string | null;
  conversations: ConversationView[];
  messages: Record<string, MessageView[]>;
  /** Received private sequence head, including hidden encrypted control messages. */
  processedSequences?: Record<string, number>;
  names: Record<string, string>;
  incomingTransfers: IncomingTransfer[];
  ktPinned: boolean;
  sessionsPaused: boolean;
  realtime: RealtimeMode;
  /** Who is typing, per conversation (principal ids, never yourself). */
  typing: Record<string, string[]>;
  pins: Record<string, PinView[]>;
  /** Older MANAGED history can be loaded (back-pagination). */
  hasOlder: Record<string, boolean>;
};

type ServerConversation = {
  conversationId: string;
  contextClass: string;
  securityProfile: SecurityProfile;
  lifecycleState: string;
  createdAt: string;
  expiresAt?: string | null;
  encryption: { mlsGroupId: string | null; serverCanReadContent: boolean };
  members?: { principalId: string; role?: string; state?: string }[];
  version?: number;
  announcementOnly?: boolean;
  /** Disappearing messages: seconds a message is kept (null: off). */
  retentionSeconds?: number | null;
  ownerPrincipalId?: string;
  title?: string | null;
  avatarRef?: string | null;
  inbox?: {
    lastActivityAt: string;
    lastMessage: { messageId: string; sequence: number; senderPrincipalId: string; createdAt: string; lifecycle: string; attachmentCount: number; previewText: string | null } | null;
    unreadCount: number;
    mentionCount: number;
    /** F1: this member's own archive / pin of the chat. */
    archived?: boolean;
    pinned?: boolean;
    pinnedAt?: string | null;
  };
};

type ServerMessage = {
  messageId: string;
  conversationId: string;
  sequence: number;
  kind: "APPLICATION" | "MLS_COMMIT" | "EDIT";
  securityProfile: SecurityProfile;
  senderPrincipalId: string;
  senderDeviceId?: string | null;
  clientMessageId: string;
  lifecycle: string;
  createdAt: string;
  attachmentIds: string[];
  e2ee?: { epoch: number; ciphertext: string | null } | null;
  managed?: { text: string | null } | null;
  replyToMessageId?: string | null;
  targetMessageId?: string | null;
};

/** What the server knows about a private message (never its content): deletion, reply and edit links. */
type E2eeMeta = { lifecycle: string; replyTo: string | null; target: string | null };

type ServerDevice = { deviceId: string; status: string; signaturePublicKey: string; displayLabel: string | null; keyPackagesAvailable?: number; registeredAt?: string | null };

/** The Delivery Service's view of a group (`GET …/mls/state`). */
type MlsState = {
  mlsEpoch: number;
  pendingRemovals: { deviceId: string; principalId: string; reason: string }[];
  /** The current epoch's GroupInfo (a member's device joins from it by itself); absent on older servers. */
  groupInfo?: string | null;
  groupInfoEpoch?: number | null;
};

/** A local group change (worker output) to submit to the Delivery Service. */
type GroupChange = { commit: Uint8Array; epoch: number; welcome?: Uint8Array | null; groupInfo?: Uint8Array | null };

/** An external commit the worker stopped at: its joining device must be verified first. */
type PendingJoiner = { deviceId: string; sequence: number; senderPrincipalId: string };

const PAGE = 100;
/** Newest MANAGED messages loaded when a conversation is first synchronized. */
const INITIAL_PAGE = 50;
/** Newest MANAGED messages re-read per poll when no realtime events arrive (edits / tombstones). */
const WINDOW = 100;
const SESSION_DEVICE_KEY = "cfmeet.messaging.session-device.";
const TYPING_THROTTLE_MS = 3000;
/** A device not yet in a chat's group retries joining it by itself at most this often. */
const JOIN_RETRY_MS = 10_000;
/** A member device re-publishes a missing GroupInfo at most this often per chat. */
const GROUP_INFO_RETRY_MS = 60_000;
/** A device that had no KeyPackage to claim is not claimed again before this. */
const UNCLAIMABLE_RETRY_MS = 5 * 60_000;

const POLL_MS = Math.max(1000, Number(process.env.NEXT_PUBLIC_MESSAGING_POLL_MS) || 15_000);

/** The API error code of an Account authority call (axios), if any. */
function accountErrorCode(e: unknown): string | null {
  const code = (e as { response?: { data?: { error?: { code?: unknown } } } })?.response?.data?.error?.code;
  return typeof code === "string" ? code : null;
}
/** How often a member device checks an encrypted group for members' new devices. */
const DEVICE_CHECK_MS = Math.max(1000, Number(process.env.NEXT_PUBLIC_MESSAGING_DEVICE_CHECK_MS) || 30_000);
// Realtime origin: the configured Messaging URL, else the API origin (where production
// routes /v1/realtime). An empty value would silently leave live updates off.
const MESSAGING_URL = messagingRealtimeOrigin(process.env.NEXT_PUBLIC_MESSAGING_URL, process.env.NEXT_PUBLIC_API_URL);
const KT_LOG_KEY = process.env.NEXT_PUBLIC_MESSAGING_KT_LOG_KEY ?? "";
const KT_LOG_ID = process.env.NEXT_PUBLIC_MESSAGING_KT_LOG_ID ?? "";
const ATTACHMENT_FORMAT = "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1";

const uuid = () => crypto.randomUUID();

/** Photos the chat shows inline; any other type (SVG included) is only ever downloaded. */
export function isInlineImage(mediaType: string | null | undefined): boolean {
  return mediaType === "image/jpeg" || mediaType === "image/png" || mediaType === "image/webp" || mediaType === "image/gif";
}

/** Voice notes and audio files the chat plays inline. */
export function isPlayableAudio(mediaType: string | null | undefined): boolean {
  return !!mediaType && /^audio\/(webm|ogg|mp4|mpeg|aac|wav)$/.test(mediaType);
}

/**
 * Photos and voice notes already shown, by attachment, for the whole visit. Kept outside the
 * engine so leaving the chat and coming back doesn't download and decrypt them again.
 */
const mediaUrls = new Map<string, string>();

/** Plaintext Blob URLs must not survive an Account session, even off the Messages page. */
function clearMediaUrls(): void {
  for (const url of mediaUrls.values()) URL.revokeObjectURL(url);
  mediaUrls.clear();
}

accountSession.subscribe((signedIn) => {
  if (signedIn) return;
  cancelLocalAuthenticatorPrompts();
  terminateMessagingWorkers();
  clearMediaUrls();
  MessagingEngine.active?.endLocalSession();
});

const lockChannel = typeof window !== "undefined" && typeof BroadcastChannel !== "undefined" ? new BroadcastChannel("cfmeet-messaging-local-lock") : null;
if (lockChannel) lockChannel.onmessage = (event: MessageEvent) => {
  if (event.data !== "lock") return;
  cancelLocalAuthenticatorPrompts();
  terminateMessagingWorkers();
  clearMediaUrls();
  MessagingEngine.active?.endLocalSession();
};

/** Close other tabs' workers before ending the shared browser session or erasing a legacy key. */
export function lockOtherMessagingTabs(): void {
  lockChannel?.postMessage("lock");
}

/** Remembers a file this device just sent, so its own photo shows without downloading it back. */
function rememberSentMedia(attachmentId: string, file: Blob, mediaType: string) {
  if (!mediaUrls.has(attachmentId) && (isInlineImage(mediaType) || isPlayableAudio(mediaType))) mediaUrls.set(attachmentId, URL.createObjectURL(new Blob([file], { type: mediaType })));
}

/** Length of an audio or video file, read by the browser (null: it can't tell). */
function mediaDurationMs(file: Blob): Promise<number | null> {
  if (typeof document === "undefined") return Promise.resolve(null);
  return new Promise((resolve) => {
    const el = document.createElement(file.type.startsWith("video/") ? "video" : "audio");
    const url = URL.createObjectURL(file);
    const done = (v: number | null) => { URL.revokeObjectURL(url); el.removeAttribute("src"); resolve(v); };
    el.preload = "metadata";
    el.onloadedmetadata = () => done(Number.isFinite(el.duration) && el.duration > 0 ? Math.max(1, Math.round(el.duration * 1000)) : null);
    el.onerror = () => done(null);
    setTimeout(() => done(null), 8000);
    el.src = url;
  });
}

function attachmentKind(mediaType: string): string {
  if (mediaType.startsWith("image/")) return "IMAGE";
  if (mediaType.startsWith("video/")) return "VIDEO";
  if (mediaType.startsWith("audio/")) return "VOICE_NOTE";
  if (mediaType === "application/pdf") return "DOCUMENT";
  return "FILE";
}

function fromHex(hex: string): Uint8Array {
  if (!/^(?:[0-9a-f]{2})+$/.test(hex)) throw new MessagingApiError(0, "BAD_HEX", null);
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", data as BufferSource));
  return Array.from(d, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** The engine running in this tab, if any (logout goes through it: it holds the device). */
export function currentMessagingEngine(): MessagingEngine | null {
  return MessagingEngine.active;
}

/** Why a send failed, in plain words (files switched off on the server get their own reason). */
function sendFailureText(e: unknown): string {
  if (e instanceof MessagingApiError && e.code === "FEATURE_DISABLED" && (e.reason === "ATTACHMENTS_DISABLED" || e.reason === "ATTACHMENT_STORAGE_NOT_CONFIGURED")) {
    return "sending photos and files isn't switched on for CF Meet yet";
  }
  return e instanceof Error ? e.message : "error";
}

export class MessagingEngine {
  static active: MessagingEngine | null = null;
  private snapshot: EngineSnapshot = {
    presence: {},
    phase: "loading",
    deviceRegisteredAt: null,
    incomingLinks: [],
    awaitingSetup: {},
    detail: null,
    connection: "disconnected",
    identity: null,
    deviceId: null,
    conversations: [],
    messages: {},
    names: {},
    incomingTransfers: [],
    ktPinned: Boolean(KT_LOG_KEY && KT_LOG_ID),
    sessionsPaused: false,
    realtime: "none",
    typing: {},
    pins: {},
    hasOlder: {},
  };
  private readonly listeners = new Set<() => void>();
  private crypto: CryptoClient | null = null;
  private transport: MessagingRealtimeTransport | null = null;
  /** Per conversation, each member's delivered / read sequence (from receipt events and snapshots). */
  private receipts = new Map<string, Map<string, { read: number; delivered: number }>>();
  private presenceTimer: ReturnType<typeof setInterval> | null = null;
  private server = new Map<string, ServerConversation>();
  /** MANAGED message stores (current representation per message, MSG-008). */
  private managed = new Map<string, ManagedConversationState>();
  private loadedManaged = new Set<string>();
  private privateWindows = new Map<string, PrivateHistoryWindow>();
  private privateVisibility = new Map<string, number>();
  private privatePaging = new Map<string, Promise<void>>();
  private reactions = new Map<string, Map<string, ReactionView[]>>();
  private e2eeMeta = new Map<string, Map<string, E2eeMeta>>();
  private attachmentMeta = new Map<string, AttachmentMeta>();
  private attachmentLoading = new Set<string>();
  private outbox: MessageOutbox | null = null;
  private flushing: Promise<number> | null = null;
  private lastReadSent = new Map<string, number>();
  private lastTypingSent = new Map<string, number>();
  private typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private authTimer: ReturnType<typeof setTimeout> | null = null;
  // A transient failure while starting (429, 5xx, network) restarts on its own with backoff
  // instead of stranding the page in "error"; server rate limits are unchanged.
  private startRetryTimer: ReturnType<typeof setTimeout> | null = null;
  private startBackoff = START_BACKOFF_MS;
  /** Session devices are switched off (FEATURE_DISABLED): MANAGED falls back to polling. */
  private sessionDevicesUnavailable = false;
  private syncing = new Map<string, Promise<void>>();
  private resync = new Set<string>();
  private timers: ReturnType<typeof setInterval>[] = [];
  private reconnectDelay = 1000;
  private stopped = false;
  /** Bumped by every start(); an older start() that resumes after a stop() must not keep going. */
  private generation = 0;
  private pendingSends = new Map<string, MessageView[]>();
  private deviceChecks = new Map<string, number>();
  /** Per chat: when a sender last checked for new devices right before sending. */
  private sendDeviceChecks = new Map<string, number>();
  private lastStockCheck = 0;
  private missingSince = new Map<string, number>();
  /** Group changes of this device (worker snapshot / rollback) run one at a time. */
  private mlsChain: Promise<unknown> = Promise.resolve();
  /** Per chat: when this device last tried to join its group by itself. */
  private joinAttempts = new Map<string, number>();
  /** Per chat: when this device last published a missing GroupInfo. */
  private groupInfoChecks = new Map<string, number>();
  /** Per chat this device is not (yet) in: the latest messages, shown as placeholders. */
  private waiting = new Map<string, ServerMessage[]>();
  /** Private messages written while this browser was still being set up (sent once ready). */
  private beforeReady: { conversationId: string; key: string; text: string; options: { replyToMessageId?: string | null; mentionedPrincipalIds?: string[] } }[] = [];
  /** Devices whose claim returned no KeyPackage (none published): not claimed again for a while. */
  private unclaimable = new Map<string, number>();
  /** Members' device lists (cached briefly: adding new devices before every send). */
  private deviceLists = new Map<string, { at: number; devices: ServerDevice[] }>();
  /** Automatic enrollment: in progress, and when a failed attempt may be retried. */
  private enrolling: Promise<void> | null = null;
  private nextEnrollAt = 0;
  private enrollBackoff = 15_000;
  /** This account's own messaging devices (cached briefly; used to tell "only me" groups). */
  private ownDeviceCache: { at: number; ids: Set<string> } | null = null;
  /** Sends of one conversation leave in the order the user wrote them. */
  private sendQueues = new Map<string, Promise<void>>();
  /** PRIVATE_E2EE group names read from (or sent into) each encrypted group on this device. */
  private groupTitles = new Map<string, string>();
  private persistentInstallation = false;
  private protectedLocalIdentity = false;

  private enqueue(conversationId: string, job: () => Promise<void>): Promise<void> {
    const previous = this.sendQueues.get(conversationId) ?? Promise.resolve();
    const generation = this.generation;
    const run = () => {
      if (this.stopped || generation !== this.generation) return;
      return job();
    };
    const next = previous.then(run, run);
    this.sendQueues.set(conversationId, next.catch(() => {}));
    return next;
  }

  // ------------------------------------------------------------- store API

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  getSnapshot = (): EngineSnapshot => this.snapshot;
  private set(patch: Partial<EngineSnapshot>): void {
    if (this.stopped) return;
    this.snapshot = { ...this.snapshot, ...patch };
    // Setup stopped for a reason the user must act on: messages written meanwhile are "Not sent".
    if (this.beforeReady.length && this.snapshot.phase !== "ready" && !MessagingEngine.settingUp(this.snapshot)) {
      const note = `Not sent (${this.snapshot.detail ?? "private messaging isn't available in this browser"})`;
      const messages = { ...this.snapshot.messages };
      for (const q of this.beforeReady.splice(0)) {
        this.settlePending(q.conversationId, q.key, note);
        messages[q.conversationId] = (messages[q.conversationId] ?? []).map((m) => (m.key === q.key ? { ...m, state: "failed" as const, note } : m));
      }
      this.snapshot = { ...this.snapshot, messages };
    }
    for (const l of this.listeners) l();
  }

  // ------------------------------------------------------------- lifecycle

  private readonly onOffline = (): void => {
    // The network is gone: drop the socket now instead of waiting for a timeout.
    this.detachTransport();
    this.set({ connection: "offline" });
  };

  /** Closes the current transport without its close starting another reconnect. */
  private detachTransport(): void {
    const old = this.transport;
    this.transport = null;
    old?.close();
  }
  private readonly onOnline = (): void => {
    if (this.stopped) return;
    this.reconnectDelay = 1000;
    this.connect();
    // Queued MANAGED sends go out now (same clientMessageId: never duplicated).
    this.outbox?.wake();
    void this.flush();
  };

  async start(cryptoFactory?: () => CryptoClient): Promise<void> {
    this.stopped = false;
    const generation = ++this.generation;
    const stale = () => this.stopped || generation !== this.generation;
    MessagingEngine.active = this;
    if (typeof window !== "undefined") {
      window.addEventListener("offline", this.onOffline);
      window.addEventListener("online", this.onOnline);
    }
    try {
      const identity = await identityApi.me();
      // Stopped (or restarted) while waiting: don't create a worker nobody will ever close.
      if (stale()) return;
      this.set({ identity });
      this.outbox = new MessageOutbox(browserOutboxStorage(identity.principalId));
      this.outbox.subscribe(() => {
        for (const id of this.loadedManaged) this.renderManaged(id);
      });
      const crypto = cryptoFactory ? cryptoFactory() : new CryptoClient();
      this.crypto = crypto;
      const openArgs = {
        account: identity.principalId,
        scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId },
      };
      type Opened = { status: string; deviceId?: string; descriptor?: LocalProtectionDescriptor; localProtection?: LocalProtectionDescriptor };
      let opened = await this.crypto.call<Opened>("open", openArgs);
      // The previous page instance in THIS tab (navigating back, or React's dev double-mount)
      // releases the lock only when its worker has fully shut down: wait briefly before
      // concluding that another tab really holds it.
      for (let attempt = 0; opened.status === "LOCKED" && attempt < 6 && !stale(); attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 250 * (attempt + 1)));
        opened = await this.crypto.call<Opened>("open", openArgs);
      }
      if (stale()) {
        // This start was overtaken by stop() / a newer start(): always release its own worker (and lock).
        if (this.crypto === crypto) this.crypto = null;
        crypto.terminate();
        return;
      }
      if (opened.status === "LOCKED") {
        this.set({ phase: "locked", detail: "Messaging is open in another tab of this browser." });
        return;
      }
      if (opened.status === "AUTHENTICATOR_REQUIRED" && opened.descriptor) {
        this.protectedLocalIdentity = true;
        if (!identity.deviceLifecycle?.persistentInstallationBinding) {
          this.set({ phase: "needs-unlock", detail: "This server must support protected device history before this browser can unlock it." });
          return;
        }
        if (!(await this.ensureTrustedAccountDevice(browserDeviceLabel(navigator.userAgent)))) return;
        const binding = await this.localProtectionBinding(opened.descriptor.messagingDeviceId);
        if (!(await this.deviceIsActive(opened.descriptor.messagingDeviceId))) {
          await crypto.call("wipe", { account: identity.principalId });
          this.set({ phase: "revoked", detail: "This browser's messaging device was removed. Its local keys and history were erased." });
          return;
        }
        this.set({ phase: "needs-unlock", detail: "Unlock this browser's private history with your device authenticator." });
        const prfSecret = await unlockLocalProtection(opened.descriptor, binding);
        try {
          if (stale()) return;
          opened = await crypto.callWithTransfer<Opened>("unlock", { prfSecret }, [prfSecret.buffer]);
        } finally { if (prfSecret.byteLength) prfSecret.fill(0); }
        if (stale()) return;
        await this.bindProtectedInstallation(opened.localProtection ?? opened.descriptor!);
      }
      if (opened.status === "READY" && opened.deviceId) {
        this.protectedLocalIdentity = Boolean(opened.localProtection);
        const active = await this.deviceIsActive(opened.deviceId);
        if (!active) {
          await this.crypto.call("wipe", { account: identity.principalId });
          this.set({ phase: "revoked", detail: "This browser's messaging device was removed. Its local keys and history were erased." });
          await this.reopenCrypto();
          await this.refreshConversations();
          this.connect();
          this.timers.push(setInterval(() => void this.poll(), POLL_MS));
          // A fresh device is set up on its own (earlier history stays with other devices).
          void this.autoEnroll();
          return;
        }
        this.set({ phase: "ready", deviceId: opened.deviceId, detail: null });
        await this.prepareLocalProtection();
        if (stale()) return;
        await this.ensureKeyPackageStock(true).catch(() => {});
        this.connect();
      } else {
        this.set({ phase: "needs-device" });
      }
      await this.refreshConversations();
      // Without an E2EE device, MANAGED conversations still get live updates (session device).
      if (this.snapshot.phase !== "ready") this.connect();
      await this.checkWelcomes();
      void this.flush();
      this.timers.push(setInterval(() => void this.poll(), POLL_MS));
      // No manual step: this browser's encryption device is set up on its own (account
      // device trusted by the sign-in, MLS enrollment, KeyPackages, realtime).
      if (this.snapshot.phase === "needs-device") void this.autoEnroll();
      this.startBackoff = START_BACKOFF_MS;
    } catch (e) {
      if (stale()) return;
      if (isTransientStartError(e)) {
        this.retryStart(cryptoFactory, generation);
        return;
      }
      this.fail(e);
    }
  }

  /** Tears down the partial start and starts again after a jittered, capped backoff. */
  private retryStart(cryptoFactory: (() => CryptoClient) | undefined, generation: number): void {
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    this.detachTransport();
    this.crypto?.terminate();
    this.crypto = null;
    const delay = this.startBackoff / 2 + Math.random() * (this.startBackoff / 2);
    this.startBackoff = Math.min(this.startBackoff * 2, START_BACKOFF_MAX_MS);
    this.set({ phase: "loading", detail: null });
    if (this.startRetryTimer) clearTimeout(this.startRetryTimer);
    this.startRetryTimer = setTimeout(() => {
      this.startRetryTimer = null;
      if (!this.stopped && generation === this.generation) void this.start(cryptoFactory);
    }, delay);
  }

  stop(): void {
    this.privateWindows.clear(); this.privateVisibility.clear(); this.privatePaging.clear();
    this.stopped = true;
    if (MessagingEngine.active === this) MessagingEngine.active = null;
    if (typeof window !== "undefined") {
      window.removeEventListener("offline", this.onOffline);
      window.removeEventListener("online", this.onOnline);
    }
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    if (this.authTimer) clearTimeout(this.authTimer);
    this.authTimer = null;
    if (this.startRetryTimer) clearTimeout(this.startRetryTimer);
    this.startRetryTimer = null;
    this.startBackoff = START_BACKOFF_MS;
    for (const t of this.typingTimers.values()) clearTimeout(t);
    this.typingTimers.clear();
    this.detachTransport();
    this.crypto?.terminate();
    this.crypto = null;
  }

  /** Ends display/cached plaintext immediately on expiry or a sign-out from another tab. */
  endLocalSession(): void {
    this.stop();
    this.persistentInstallation = false;
    this.protectedLocalIdentity = false;
    this.receipts.clear();
    this.server.clear();
    this.managed.clear();
    this.loadedManaged.clear();
    this.reactions.clear();
    this.e2eeMeta.clear();
    this.attachmentMeta.clear();
    this.pendingSends.clear();
    this.beforeReady = [];
    this.failedFiles.clear();
    this.groupTitles.clear();
    this.waiting.clear();
    clearMediaUrls();
    this.snapshot = { ...this.snapshot, phase: "locked", detail: "Sign in to unlock your messages.", identity: null, deviceId: null, messages: {}, processedSequences: {}, conversations: [], names: {}, incomingTransfers: [], incomingLinks: [], typing: {}, pins: {}, connection: "auth-required" };
    for (const listener of this.listeners) listener();
  }

  private async localProtectionBinding(deviceId: string): Promise<LocalProtectionBinding> {
    const identity = this.snapshot.identity!;
    const accountBinding = await accountDevice.binding();
    if (!accountBinding) throw new LocalAuthenticatorError("LOCAL_UNLOCK_INSTALLATION_REQUIRED");
    return { account: identity.principalId, messagingDeviceId: deviceId, accountDeviceId: accountBinding.accountDeviceId, installationId: accountBinding.installationId,
      scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId } };
  }

  private async bindProtectedInstallation(descriptor: LocalProtectionDescriptor): Promise<void> {
    const challenge = await identityApi.installationChallenge(descriptor.messagingDeviceId);
    if (challenge.deviceId !== descriptor.messagingDeviceId || challenge.accountDeviceId !== descriptor.accountDeviceId
      || !Number.isFinite(Date.parse(challenge.expiresAt)) || Date.parse(challenge.expiresAt) <= Date.now()) throw new LocalAuthenticatorError("LOCAL_UNLOCK_BINDING_MISMATCH");
    const keyPackage = await this.crypto!.call<Uint8Array>("registrationKeyPackage", { challengeId: challenge.challengeId, nonce: fromHex(challenge.nonce) });
    const bound = await identityApi.bindInstallation(descriptor.messagingDeviceId, challenge.challengeId, b64u.enc(keyPackage));
    if (bound.persistentIdentity !== true || bound.deviceId !== descriptor.messagingDeviceId || bound.accountDeviceId !== descriptor.accountDeviceId) throw new LocalAuthenticatorError("LOCAL_UNLOCK_BINDING_MISMATCH");
    this.persistentInstallation = true;
  }

  /** Server capability negotiation prevents new local retention on older lifecycle contracts. */
  private async prepareLocalProtection(): Promise<void> {
    if (this.persistentInstallation || !this.crypto || !this.snapshot.deviceId || !this.snapshot.identity?.deviceLifecycle?.persistentInstallationBinding) return;
    if (!(await this.ensureTrustedAccountDevice(browserDeviceLabel(navigator.userAgent)))) return;
    const crypto = this.crypto;
    const state = await crypto.call<{ localProtection?: LocalProtectionDescriptor }>("status");
    try {
      let descriptor = state.localProtection;
      if (!descriptor) {
        const protection = await createLocalProtection(await this.localProtectionBinding(this.snapshot.deviceId));
        descriptor = protection.descriptor;
        try {
          if (this.stopped || this.crypto !== crypto) return;
          await this.withMlsLock(() => crypto.callWithTransfer("protectLocal", { descriptor, prfSecret: protection.prfSecret }, [protection.prfSecret.buffer]));
          this.protectedLocalIdentity = true;
        } finally { if (protection.prfSecret.byteLength) protection.prfSecret.fill(0); }
      }
      if (!this.stopped && this.crypto === crypto) await this.bindProtectedInstallation(descriptor);
    } catch (error) {
      // Unsupported/cancelled authenticators do not silently enable decryptable retention.
      // Existing session behaviour remains available; protected sign-out is not claimed.
      this.persistentInstallation = false;
      if (!(error instanceof LocalAuthenticatorError) && !(error instanceof MessagingApiError)) throw error;
    }
  }

  private async reopenCrypto(): Promise<void> {
    if (this.stopped || !this.snapshot.identity) return;
    const identity = this.snapshot.identity!;
    this.crypto?.terminate();
    this.crypto = new CryptoClient();
    await this.crypto.call("open", {
      account: identity.principalId,
      scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId },
    });
  }

  private fail(e: unknown): void {
    const code = e instanceof MessagingApiError || e instanceof CryptoWorkerError || e instanceof LocalAuthenticatorError ? e.code : "UNEXPECTED_ERROR";
    if (code === "FEATURE_DISABLED") {
      this.set({ phase: "unavailable", detail: "Messaging is not enabled for this CF Meet deployment." });
    } else if (code === "LOCAL_UNLOCK_CANCELLED" || code === "LOCAL_AUTHENTICATOR_UNAVAILABLE" || code === "LOCAL_AUTHENTICATOR_PRF_UNAVAILABLE") {
      this.set({ phase: "needs-unlock", detail: "Your history is locked. Use this browser's device authenticator to unlock it." });
    } else {
      this.set({ phase: "error", detail: code });
    }
  }

  private async poll(): Promise<void> {
    if (this.stopped) return;
    try {
      await this.refreshConversations();
      await this.checkWelcomes();
      await this.checkIncomingTransfers();
      await this.checkIncomingLinks();
      await this.recoverHistoryInBackground();
      // Realtime is the fast path; a periodic catch-up also lets member devices add new
      // devices and commit pending removals of encrypted groups.
      for (const id of this.server.keys()) this.scheduleSync(id);
      void this.flush();
      if (this.snapshot.phase === "needs-device" && Date.now() >= this.nextEnrollAt) void this.autoEnroll();
      for (const id of Object.keys(this.snapshot.awaitingSetup)) void this.flushDrafts(id);
      await this.ensureKeyPackageStock().catch(() => {});
      if (this.snapshot.phase === "ready" && this.snapshot.deviceId && !(await this.deviceIsActive(this.snapshot.deviceId))) {
        await this.onDeviceRevoked();
      }
    } catch {
      // Periodic work retries on the next tick; the UI shows the connection state.
    }
  }

  // ---------------------------------------------------------------- device

  private async ownDevices(): Promise<ServerDevice[]> {
    const me = this.snapshot.identity!.principalId;
    const devices = await this.loadDeviceDirectory(me);
    const self = devices.find((d) => d.deviceId === this.snapshot.deviceId);
    if (self?.registeredAt && self.registeredAt !== this.snapshot.deviceRegisteredAt) this.set({ deviceRegisteredAt: self.registeredAt });
    return devices;
  }

  private async loadDeviceDirectory(principalId: string): Promise<ServerDevice[]> {
    return allDevicePages<ServerDevice>(
      query => relay("GET", `v1/messaging/principals/${principalId}/devices${query}`),
      code => new MessagingApiError(0, code, null),
    );
  }

  private async deviceIsActive(deviceId: string): Promise<boolean> {
    return (await this.ownDevices()).some((d) => d.deviceId === deviceId && d.status === "ACTIVE");
  }

  /**
   * Sets up this browser's encryption device without any user action; retried with backoff
   * on transient failures. Never runs twice at once.
   */
  autoEnroll(): Promise<void> {
    if (this.enrolling || this.stopped) return this.enrolling ?? Promise.resolve();
    const label = typeof navigator !== "undefined" ? browserDeviceLabel(navigator.userAgent) : "CF Meet web browser";
    this.enrolling = this.registerDevice(label).finally(() => {
      this.enrolling = null;
    });
    return this.enrolling;
  }

  /** Creates this browser's device: keys generated in the worker, proof of possession. */
  async registerDevice(label = "CF Meet web browser"): Promise<void> {
    if (!this.crypto || !this.snapshot.identity) return;
    this.set({ phase: "registering", detail: null });
    try {
      // Multi-device policy: only a trusted device of the account may hold an E2EE device.
      if (!(await this.ensureTrustedAccountDevice(label))) return;
      const challenge = await identityApi.registrationChallenge();
      await this.crypto.call("generate", { deviceId: challenge.deviceId });
      const kp = await this.crypto.call<Uint8Array>("registrationKeyPackage", {
        challengeId: challenge.challengeId,
        nonce: fromHex(challenge.nonce),
      });
      const device = await identityApi.registerDevice(challenge.challengeId, b64u.enc(kp), label.slice(0, 64));
      await this.crypto.call("adopt");
      // Publish this device's KeyPackages BEFORE it reports ready: from then on others may
      // start chats with it while it is offline, adding it from these (WhatsApp). A failed
      // publication is retried by the stock check.
      this.set({ deviceId: device.deviceId, deviceRegisteredAt: device.registeredAt ?? null });
      await this.topUpKeyPackages(KEY_PACKAGE_TARGET).catch(() => {});
      this.set({ phase: "ready", deviceId: device.deviceId, detail: null });
      await this.prepareLocalProtection();
      this.flushBeforeReady();
      this.enrollBackoff = 15_000;
      this.connect();
      await this.refreshConversations();
      await this.checkWelcomes();
    } catch (e) {
      if (e instanceof MessagingApiError && (e.code === "E2EE_DEVICES_DISABLED" || e.reason === "E2EE_DEVICES_DISABLED")) {
        this.set({ phase: "unavailable", detail: "End-to-end encrypted messaging is not enabled in this environment. Managed conversations remain available." });
        return;
      }
      await this.reopenCrypto().catch(() => {});
      if (this.stopped) return;
      // Transient (network, server busy): retry on its own with backoff; nothing for the user to do.
      const status = e instanceof MessagingApiError ? e.status : 0;
      if (status === 0 || status === 408 || status === 429 || status >= 500 || e instanceof TypeError) {
        this.nextEnrollAt = Date.now() + this.enrollBackoff;
        this.enrollBackoff = Math.min(this.enrollBackoff * 2, 5 * 60_000);
        // Silent: setup keeps going on its own; nothing is shown to the user.
        this.set({ phase: "needs-device", detail: null });
        return;
      }
      this.fail(e);
    }
  }

  /**
   * Keeps this device's published KeyPackages stocked (WhatsApp: keys published in
   * advance), so a sender can add this device to a chat on its own while it is offline.
   * Every claim spends one; below KEY_PACKAGE_STOCK the device publishes up to
   * KEY_PACKAGE_TARGET. Servers that do not report the stock are left alone.
   */
  private async ensureKeyPackageStock(force = false): Promise<void> {
    const deviceId = this.snapshot.deviceId;
    if (!this.crypto || this.snapshot.phase !== "ready" || !deviceId) return;
    if (!force && Date.now() - this.lastStockCheck < DEVICE_CHECK_MS) return;
    this.lastStockCheck = Date.now();
    const own = (await this.ownDevices()).find((d) => d.deviceId === deviceId);
    await this.topUpKeyPackages(keyPackagesToPublish(own?.keyPackagesAvailable));
  }

  private async topUpKeyPackages(count = 0): Promise<void> {
    const c = this.crypto;
    if (!c || !this.snapshot.deviceId || count < 1) return;
    const kps = await this.withMlsLock(() => c.call<Uint8Array[]>("keyPackages", { count }));
    await identityApi.publishKeyPackages(this.snapshot.deviceId, kps.map((k) => b64u.enc(k)));
  }

  /** Group changes of this device's worker (snapshot / confirm / rollback) never interleave. */
  private withMlsLock<T>(job: () => Promise<T>): Promise<T> {
    // A group mutation spans local RPCs and the delivery service's acceptance. The
    // shared worker serializes RPCs; this separate lock serializes whole mutations
    // across tabs so another port cannot mutate a pending snapshot before confirm.
    const execute = async (): Promise<T> => {
      const account = this.snapshot.identity?.principalId;
      if (this.stopped || !account || !this.crypto) return Promise.reject<T>(new MessagingApiError(0, "HISTORY_LOCKED", null));
      if (typeof navigator !== "undefined" && navigator.locks) return await navigator.locks.request(`cfmeet-messaging-transaction:${account}`, job);
      return job();
    };
    const run = this.mlsChain.then(execute, execute);
    this.mlsChain = run.catch(() => {});
    return run;
  }

  /**
   * Submits a group change this device already made locally, with the GroupInfo of the
   * epoch it creates (so a member's new device can join by itself), and keeps it only if
   * the Delivery Service accepts it. A server that does not take GroupInfos yet (an
   * older image) gets the same commit without it.
   */
  private async submitChange(conv: ServerConversation, out: GroupChange, extra: { welcomed?: string[]; removed?: string[] } = {}): Promise<void> {
    const c = this.crypto!;
    const body = {
      clientMessageId: uuid(),
      deviceId: this.snapshot.deviceId,
      epoch: out.epoch,
      commit: b64u.enc(out.commit),
      ...(out.welcome && extra.welcomed?.length ? { welcome: b64u.enc(out.welcome), welcomeDeviceIds: extra.welcomed } : {}),
      ...(extra.removed ? { removedDeviceIds: extra.removed } : {}),
    };
    const path = `v1/conversations/${conv.conversationId}/mls/commits`;
    try {
      if (out.groupInfo) {
        try {
          await relay("POST", path, { ...body, groupInfo: b64u.enc(out.groupInfo) });
        } catch (e) {
          if (!(e instanceof MessagingApiError) || e.status !== 400) throw e;
          await relay("POST", path, body);
        }
      } else {
        await relay("POST", path, body);
      }
      await c.call("confirm");
    } catch (e) {
      await c.call("rollback").catch(() => {});
      throw e;
    }
  }

  private mlsState(conv: ServerConversation): Promise<MlsState> {
    return relay<MlsState>("GET", `v1/conversations/${conv.conversationId}/mls/state`);
  }

  /** A member's device list, cached for `maxAgeMs`. */
  /**
   * The security code of a private one-to-one chat (WhatsApp): 60 digits from both people's
   * active device keys. Both phones show the same code; if it matches when you compare it in
   * person, nobody swapped a key. It changes when either person adds or removes a device.
   */
  async securityCode(conversationId: string): Promise<{ code: string; logChecked: boolean } | null> {
    const conv = this.server.get(conversationId);
    const me = this.snapshot.identity?.principalId;
    const view = this.snapshot.conversations.find((c) => c.id === conversationId);
    if (!conv || !me || conv.securityProfile !== "PRIVATE_E2EE" || !view || view.members.length !== 2) return null;
    const people = [...view.members].sort();
    const parts = await Promise.all(people.map(async (p) => {
      const keys = (await this.principalDevices(p, 0)).filter((d) => d.status === "ACTIVE").map((d) => d.signaturePublicKey).sort();
      return `${p}:${keys.join(",")}`;
    }));
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-512", new TextEncoder().encode(`cfmeet-security-code-v1|${parts.join("|")}`)));
    const view32 = new DataView(digest.buffer);
    const blocks = Array.from({ length: 12 }, (_, i) => String(view32.getUint32(i * 4) % 100000).padStart(5, "0"));
    return { code: blocks.join(" "), logChecked: this.snapshot.ktPinned };
  }

  private async principalDevices(principalId: string, maxAgeMs: number): Promise<ServerDevice[]> {
    const hit = this.deviceLists.get(principalId);
    if (hit && Date.now() - hit.at < maxAgeMs) return hit.devices;
    const devices = await this.loadDeviceDirectory(principalId);
    this.deviceLists.set(principalId, { at: Date.now(), devices });
    return devices;
  }

  private conversationMembers(conversationId: string): Promise<{ principalId: string; role?: string; state?: string }[]> {
    return allMemberPages((cursor) => relay("GET", `v1/conversations/${conversationId}/members?limit=${PAGE}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`));
  }

  /**
   * Joins a chat's encryption group BY ITSELF (MLS external commit from the GroupInfo the
   * members published): this device never waits for anyone else to come online. A Welcome
   * that already adds it is used first. Returns whether this device is in the group.
   */
  private async joinBySelf(conv: ServerConversation, force = false): Promise<boolean> {
    const c = this.crypto;
    const id = conv.conversationId;
    if (!c || this.snapshot.phase !== "ready" || !this.snapshot.deviceId || conv.securityProfile !== "PRIVATE_E2EE" || !conv.encryption.mlsGroupId) return false;
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (await c.call<boolean>("hasGroup", { groupId: gid })) return true;
    if (!force && Date.now() - (this.joinAttempts.get(id) ?? 0) < JOIN_RETRY_MS) return false;
    this.joinAttempts.set(id, Date.now());
    await this.checkWelcomes().catch(() => {});
    const joined = await this.withMlsLock(async () => {
      if (await c.call<boolean>("hasGroup", { groupId: gid })) return true;
      const state = await this.mlsState(conv);
      if (!state.groupInfo) return false;
      const out = await c.call<GroupChange>("externalJoin", { groupId: gid, groupInfo: b64u.dec(state.groupInfo) });
      try {
        await this.submitChange(conv, out);
      } catch {
        // Another commit won this epoch (or a removal must be committed first): retried soon.
        return false;
      }
      return true;
    });
    if (joined) {
      this.joinAttempts.delete(id);
      this.waiting.delete(id);
      await this.renderConversations();
    }
    return joined;
  }

  /**
   * The current epoch has no GroupInfo (a chat created before devices could join by
   * themselves, or a commit of an older client): a member device publishes one with a
   * key-update commit, so the members' new devices can join without anyone online.
   */
  private async ensureGroupInfo(conv: ServerConversation, state: MlsState | null): Promise<void> {
    const c = this.crypto;
    const id = conv.conversationId;
    if (!c || !state || !("groupInfoEpoch" in state) || state.groupInfoEpoch === state.mlsEpoch || state.pendingRemovals.length) return;
    if (Date.now() - (this.groupInfoChecks.get(id) ?? 0) < GROUP_INFO_RETRY_MS) return;
    this.groupInfoChecks.set(id, Date.now());
    const gid = b64u.dec(conv.encryption.mlsGroupId!);
    await this.withMlsLock(async () => {
      if (Number(await c.call<number>("epoch", { groupId: gid })) !== state.mlsEpoch) return;
      const out = await c.call<GroupChange>("updateKeys", { groupId: gid });
      // Another member's device may publish first (its commit wins the epoch): fine.
      await this.submitChange(conv, out, { removed: [] }).catch(() => {});
    });
  }

  /**
   * Whether the device that joined by an external commit may be merged: a registered,
   * ACTIVE device of a principal who is an ACTIVE member, whose key is the one in the
   * directory and (with a pinned log) in the key-transparency log. `null`: refused.
   * Throws when it cannot be decided now (network): the commit is retried later.
   */
  private async verifyJoiner(conv: ServerConversation, joiner: PendingJoiner): Promise<string | null> {
    const members = await this.conversationMembers(conv.conversationId);
    if (!members.some((m) => m.principalId === joiner.senderPrincipalId && (!m.state || m.state === "ACTIVE"))) return null;
    let devices: ServerDevice[];
    try {
      devices = await this.principalDevices(joiner.senderPrincipalId, 0);
    } catch (e) {
      if (e instanceof MessagingApiError && (e.status === 404 || e.status === 403)) return null;
      throw e;
    }
    const d = devices.find((x) => x.deviceId === joiner.deviceId && x.status === "ACTIVE");
    if (!d) return null;
    const key = b64u.dec(d.signaturePublicKey);
    try {
      await this.verifyTransparency(joiner.deviceId, key);
    } catch (e) {
      if (e instanceof CryptoWorkerError) return null;
      throw e;
    }
    return joinerEntry(joiner.deviceId, key);
  }

  /** Removes this browser's device (server first) and erases its local keys and history. */
  async removeThisDevice(): Promise<void> {
    const id = this.snapshot.deviceId;
    if (!id || !this.crypto || !this.snapshot.identity) return;
    await identityApi.revokeDevice(id);
    await this.onDeviceRevoked("This device was removed. Its local keys and history were erased.");
  }

  /** Revoke existing client sessions without deleting any device's encryption keys. */
  async revokeAllSessions(): Promise<number> {
    if (this.stopped) return 0;
    this.set({ sessionsPaused: true, connection: "disconnected" });
    this.detachTransport();
    try {
      return (await identityApi.revokeAllSessions()).revokedSessions;
    } catch (error) {
      this.set({ sessionsPaused: false });
      this.connect();
      throw error;
    }
  }

  resumeSession(): void {
    if (this.stopped) return;
    this.set({ sessionsPaused: false });
    this.connect();
  }

  private async onDeviceRevoked(detail = "This browser's messaging device was revoked. Its local keys and history were erased."): Promise<void> {
    if (this.stopped) return;
    this.detachTransport();
    this.persistentInstallation = false;
    this.protectedLocalIdentity = false;
    clearMediaUrls();
    await this.crypto?.call("wipe", { account: this.snapshot.identity!.principalId });
    // A sign-out in progress erases everything itself: never reopen (a reopened worker
    // would create a fresh wrapping key after the sign-out's wipe).
    if (this.stopped) return;
    await this.reopenCrypto();
    this.set({ phase: "revoked", detail, deviceId: null, deviceRegisteredAt: null, messages: {}, connection: "disconnected" });
    this.managed.clear();
    this.loadedManaged.clear();
    await this.refreshConversations();
    // MANAGED conversations continue on a session device.
    this.connect();
  }

  /** Ordinary sign-out locks a protected installation; explicit security removal stays terminal. */
  async signOut(terminal = false): Promise<void> {
    // Background work stops first: the revocation below must not be "detected" by a poll
    // that would wipe and reopen the device store in parallel.
    this.stopped = true;
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    this.detachTransport();
    lockOtherMessagingTabs();
    // A transient bind/read failure cannot justify destroying already sealed history.
    // Retaining ciphertext grants no network authority; the next sign-in must still prove
    // the active server installation before it can open the protected identity.
    const retain = !terminal && this.protectedLocalIdentity;
    cancelLocalAuthenticatorPrompts();
    try {
      if (!retain && this.snapshot.deviceId) await identityApi.revokeDevice(this.snapshot.deviceId);
    } catch {
      // The Meeting Service also revokes every Messaging session of the account at logout.
    }
    if (!retain && this.snapshot.identity) await this.crypto?.call("wipe", { account: this.snapshot.identity.principalId }).catch(() => {});
    this.endLocalSession();
  }

  // -------------------------------------------------------------- realtime

  private connect(): void {
    if (this.snapshot.sessionsPaused || this.stopped) return;
    if (typeof navigator !== "undefined" && navigator.onLine === false) {
      this.set({ connection: "offline" });
      return;
    }
    // An E2EE device connects with its device-bound token; without one, a MANAGED-only
    // session device (ADR 0049 A1) — never presented as end-to-end encrypted.
    const deviceId = this.snapshot.phase === "ready" ? this.snapshot.deviceId : null;
    const mode: RealtimeMode = deviceId ? "device" : this.sessionDevicesUnavailable ? "none" : "session";
    if (!MESSAGING_URL || mode === "none") {
      this.detachTransport();
      this.set({ connection: "offline", realtime: "none" });
      return;
    }
    this.detachTransport();
    const transport = new MessagingRealtimeTransport({
      baseUrl: MESSAGING_URL,
      getAccessToken: deviceId
        ? async () => {
            // Renewed on the open socket before it expires (like session-device tokens),
            // so a healthy connection never drops at token expiry.
            const token = messagingClientAccessToken(await identityApi.clientToken(deviceId), deviceId);
            this.scheduleAuthRefresh(token.expiresAt);
            return token;
          }
        : () => this.sessionAccessToken(),
      onEvent: (event) => this.onRealtimeEvent(event),
      onSyncRequired: (conversationId) => {
        void this.syncAuthoritative(conversationId);
      },
      onSubscribed: (conversationId, after) => {
        // MSG-018: the server resumed from this device's stored ack cursor.
        const st = this.managed.get(conversationId);
        if (st && st.eventCursor === null) st.eventCursor = after;
        transport.queryReceipts(conversationId);
      },
      onPresence: (conversationId, members) => {
        const me = this.snapshot.identity?.principalId;
        const map: Record<string, { online: boolean; lastSeenAt: string | null }> = {};
        for (const m of members) if (m.principalId !== me) map[m.principalId] = { online: m.state === "AVAILABLE", lastSeenAt: m.lastSeenAt ?? null };
        this.set({ presence: { ...this.snapshot.presence, [conversationId]: map } });
      },
      onReceipts: (conversationId, members) => {
        for (const m of members) this.noteReceipt(conversationId, m.principalId, m.readSequence, m.deliveredSequence);
        this.refreshTicks(conversationId);
      },
      onTyping: (conversationId, principalId, state, ttlMs) => this.onTyping(conversationId, principalId, state, ttlMs),
      onReadCursor: (conversationId) => {
        const c = this.snapshot.conversations.find((x) => x.id === conversationId);
        if (c?.unreadCount) this.set({ conversations: this.snapshot.conversations.map((x) => (x.id === conversationId ? { ...x, unreadCount: 0, mentionCount: 0 } : x)) });
      },
      onSubscriptionRevoked: () => {
        void this.refreshConversations().catch(() => {});
      },
      onConnectionState: (state) => {
        if (this.transport !== transport) return;
        this.set({ connection: state });
        if (state === "connected") {
          this.reconnectDelay = 1000;
          // Missed while offline: catch up every conversation from the Delivery Service.
          for (const id of this.server.keys()) this.scheduleSync(id);
          this.outbox?.wake();
          void this.flush();
        } else if (!this.stopped) {
          this.scheduleReconnect(state === "auth-required");
        }
      },
    });
    this.transport = transport;
    this.set({ realtime: mode });
    for (const c of this.server.values()) this.subscribeConversation(c);
    this.set({ connection: "connecting" });
    transport.connect().catch((e) => {
      if (this.transport !== transport || this.stopped) return;
      if (e instanceof MessagingApiError && (e.code === "FEATURE_DISABLED" || e.status === 404)) {
        // Session devices are off in this environment: poll instead of retrying.
        this.sessionDevicesUnavailable = true;
        this.detachTransport();
        this.set({ connection: "offline", realtime: "none" });
        return;
      }
      this.scheduleReconnect(false);
    });
  }

  /** Subscribes from the server's per-device ack cursor (MSG-018); a session device only sees MANAGED. */
  private subscribeConversation(c: ServerConversation): void {
    const t = this.transport;
    if (!t) return;
    if (this.snapshot.realtime === "session" && c.securityProfile !== "MANAGED") return;
    try {
      t.subscribeResume(c.conversationId);
    } catch {
      // An invalid id never reaches the socket.
    }
  }

  /** A MANAGED-realtime session-device token; the device id is kept for this tab so the server's ack cursor survives reloads. */
  private async sessionAccessToken() {
    const principal = this.snapshot.identity?.principalId ?? "";
    const key = SESSION_DEVICE_KEY + principal;
    let stored: string | null = null;
    try {
      stored = typeof window !== "undefined" ? window.sessionStorage.getItem(key) : null;
    } catch {
      stored = null;
    }
    let body: unknown;
    try {
      body = await identityApi.sessionToken(stored);
    } catch (e) {
      // The stored session device ended (sign-in changed, revoked): start a new one.
      if (!stored || !(e instanceof MessagingApiError) || ![400, 403, 404, 409].includes(e.status)) throw e;
      stored = null;
      body = await identityApi.sessionToken(null);
    }
    const token = messagingSessionDeviceToken(body, stored);
    try {
      window.sessionStorage.setItem(key, token.deviceId);
    } catch {
      // Without storage the next page load starts a new session device.
    }
    this.scheduleAuthRefresh(token.expiresAt);
    return token;
  }

  private scheduleAuthRefresh(expiresAt: string): void {
    if (this.authTimer) clearTimeout(this.authTimer);
    const ms = Math.max(5_000, Date.parse(expiresAt) - Date.now() - 60_000);
    this.authTimer = setTimeout(() => {
      const t = this.transport;
      if (!t || this.stopped) return;
      // Renew before expiry on the open socket (`auth.refresh`); reconnect when that fails.
      t.refreshAuth().catch(() => {
        if (this.transport === t && !this.stopped) this.connect();
      });
    }, ms);
  }

  private scheduleReconnect(authRequired: boolean): void {
    const delay = this.reconnectDelay;
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30_000);
    setTimeout(async () => {
      if (this.stopped) return;
      if (authRequired && this.snapshot.phase === "ready" && this.snapshot.deviceId && !(await this.deviceIsActive(this.snapshot.deviceId).catch(() => true))) {
        await this.onDeviceRevoked();
        return;
      }
      if (authRequired && this.snapshot.realtime === "session") {
        // The session device was revoked with its sign-in: the next token creates a new one.
        try {
          window.sessionStorage.removeItem(SESSION_DEVICE_KEY + (this.snapshot.identity?.principalId ?? ""));
        } catch {
          // nothing stored
        }
      }
      this.connect();
    }, delay + Math.floor(Math.random() * 250));
  }

  /** A realtime event: MANAGED ones are applied directly (current representation), E2EE ones trigger a sync. */
  /** Keeps "online" / "last seen" fresh for the open chat (null: stop). */
  watchPresence(conversationId: string | null): void {
    if (this.presenceTimer) clearInterval(this.presenceTimer);
    this.presenceTimer = null;
    if (!conversationId) return;
    const ask = () => this.transport?.queryPresence(conversationId);
    ask();
    this.presenceTimer = setInterval(ask, 20_000);
  }

  private noteReceipt(conversationId: string, principalId: string, read?: number, delivered?: number): void {
    let byMember = this.receipts.get(conversationId);
    if (!byMember) this.receipts.set(conversationId, (byMember = new Map()));
    const cur = byMember.get(principalId) ?? { read: 0, delivered: 0 };
    const r = Math.max(cur.read, read ?? 0);
    // Reading a message implies it was delivered.
    byMember.set(principalId, { read: r, delivered: Math.max(cur.delivered, delivered ?? 0, r) });
  }

  /** Ticks for your messages: read when every other member read it, delivered when it reached all of them. */
  private withTicks(conversationId: string, views: MessageView[]): MessageView[] {
    const me = this.snapshot.identity?.principalId;
    const others = (this.server.get(conversationId)?.members ?? []).map((m) => m.principalId).filter((p) => p !== me);
    const byMember = this.receipts.get(conversationId);
    return views.map((v) => {
      if (!v.mine || v.sequence == null || (v.state !== "sent" && v.state !== "delivered")) return v;
      const seq = v.sequence;
      const all = (key: "read" | "delivered") => others.length > 0 && others.every((p) => (byMember?.get(p)?.[key] ?? 0) >= seq);
      const receipt = all("read") ? "read" : all("delivered") ? "delivered" : "sent";
      return v.receipt === receipt ? v : { ...v, receipt };
    });
  }

  private refreshTicks(conversationId: string): void {
    const views = this.snapshot.messages[conversationId];
    if (!views) return;
    const next = this.withTicks(conversationId, views);
    if (next.some((v, i) => v !== views[i])) this.set({ messages: { ...this.snapshot.messages, [conversationId]: next } });
  }

  private async onRealtimeEvent(event: MessagingRealtimeEvent): Promise<void> {
    const conv = this.server.get(event.conversationId);
    if (!conv) return;
    if (event.type === "read.cursor.updated" || event.type === "delivery.cursor.updated") {
      const d = ((event as { data?: Record<string, unknown> }).data ?? {}) as { principalId?: unknown; readSequence?: unknown; deliveredSequence?: unknown };
      if (typeof d.principalId === "string") {
        this.noteReceipt(event.conversationId, d.principalId,
          typeof d.readSequence === "number" ? d.readSequence : undefined,
          typeof d.deliveredSequence === "number" ? d.deliveredSequence : undefined);
        this.refreshTicks(event.conversationId);
      }
    }
    if (conv.securityProfile !== "MANAGED") {
      const type = typeof event.type === "string" ? event.type.replace(/^messaging\./, "") : "";
      if (type.startsWith("message.") && type !== "message.created") {
        const data = (event.data ?? {}) as { messageId?: unknown; message_id?: unknown; message?: { messageId?: unknown } };
        const target = data.messageId ?? data.message_id ?? data.message?.messageId;
        await this.refreshE2eeMeta(event.conversationId, typeof target === "string" ? target : undefined);
        await this.renderE2ee(event.conversationId);
      }
      if (type.startsWith("pin.")) await this.loadPins(event.conversationId);
      this.scheduleSync(event.conversationId);
      return;
    }
    const st = this.stateFor(event.conversationId);
    const refresh = st.applyEvents([event as unknown as ConversationEvent]);
    this.afterManagedChange(conv, refresh);
    const type = typeof event.type === "string" ? event.type : "";
    if (type === "message.created" || type === "message.edited" || type.startsWith("message.")) {
      // Keep the list's preview / order current.
      this.bumpConversation(event.conversationId, st);
    }
  }

  private async syncAuthoritative(conversationId: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (conv?.securityProfile === "MANAGED") {
      await this.replayManaged(conv).catch(() => {});
      return;
    }
    await this.sync(conversationId);
    await this.refreshE2eeMeta(conversationId);
    await this.renderE2ee(conversationId);
    // The event cursor the gateway resumes from: the head of the durable event log.
    let after = this.transport?.cursor(conversationId) ?? 0;
    for (;;) {
      const page = await relay<{ events: { eventSequence: number }[] }>(
        "GET",
        `v1/conversations/${conversationId}/events?after=${after}&limit=${PAGE}`,
      ).catch(() => ({ events: [] }));
      if (!page.events.length) break;
      after = Math.max(after, ...page.events.map((e) => e.eventSequence));
      if (page.events.length < PAGE) break;
    }
    this.transport?.synced(conversationId, after);
  }

  // --------------------------------------------------------- conversations

  async refreshConversations(): Promise<void> {
    const out: ServerConversation[] = [];
    let cursor: string | null = null;
    const seenCursors = new Set<string>();
    for (;;) {
      const page: { conversations: ServerConversation[]; nextCursor: string | null } = await relay(
        "GET",
        `v1/conversations?limit=${PAGE}&orderBy=lastActivity${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`,
      );
      out.push(...page.conversations);
      cursor = page.nextCursor;
      if (!cursor) break;
      if (seenCursors.has(cursor)) throw new MessagingApiError(0, "CONVERSATION_PAGE_INVALID", null);
      seenCursors.add(cursor);
    }
    // The list carries at most two members; groups are read in full.
    for (const c of out) {
      if (c.contextClass !== "DIRECT") {
        const members = await this.conversationMembers(c.conversationId);
        c.members = members.filter((x) => !x.state || x.state === "ACTIVE");
      }
    }
    const fresh = out.filter((c) => !this.server.has(c.conversationId));
    const gone = [...this.server.keys()].filter((id) => !out.some((c) => c.conversationId === id));
    for (const id of gone) {
      // No longer a member (removed / left): it disappears from this client.
      this.server.delete(id);
      this.transport?.unsubscribe(id);
      this.managed.delete(id);
      this.loadedManaged.delete(id);
      this.outbox?.dropConversation(id);
    }
    for (const c of out) this.server.set(c.conversationId, c);
    for (const c of fresh) this.subscribeConversation(c);
    await this.renderConversations();
    for (const c of fresh) this.scheduleSync(c.conversationId);
  }

  private async renderConversations(): Promise<void> {
    const me = this.snapshot.identity?.principalId;
    const views: ConversationView[] = [];
    const needNames = new Set<string>();
    for (const c of this.server.values()) {
      const members = (c.members ?? []).map((m) => m.principalId);
      for (const m of members) if (m !== me && !this.snapshot.names[m]) needNames.add(m);
      let groupReady = false;
      if (c.securityProfile === "PRIVATE_E2EE" && c.encryption.mlsGroupId && this.crypto && this.snapshot.deviceId) {
        groupReady = await this.crypto.call<boolean>("hasGroup", { groupId: b64u.dec(c.encryption.mlsGroupId) }).catch(() => false);
      }
      const roles: Record<string, MemberRole> = {};
      for (const m of c.members ?? []) if (m.role) roles[m.principalId] = m.role;
      const previous = this.snapshot.conversations.find((x) => x.id === c.conversationId);
      const last = c.inbox?.lastMessage ?? null;
      views.push({
        id: c.conversationId,
        contextClass: c.contextClass,
        securityProfile: c.securityProfile,
        serverCanReadContent: c.encryption.serverCanReadContent,
        lifecycleState: c.lifecycleState,
        mlsGroupId: c.encryption.mlsGroupId,
        groupReady,
        createdAt: c.createdAt,
        expiresAt: c.expiresAt ?? null,
        members,
        title: "",
        transparency: c.securityProfile === "PRIVATE_E2EE" ? (this.snapshot.ktPinned ? "VERIFIED" : "UNPINNED") : "NOT_APPLICABLE",
        version: c.version ?? 0,
        ownerPrincipalId: c.ownerPrincipalId ?? null,
        serverTitle: c.title ?? null,
        groupTitle: c.securityProfile === "PRIVATE_E2EE" ? (this.groupTitles.get(c.conversationId) ?? null) : null,
        avatarRef: c.avatarRef ?? null,
        unreadCount: c.inbox?.unreadCount ?? previous?.unreadCount ?? 0,
        mentionCount: c.inbox?.mentionCount ?? previous?.mentionCount ?? 0,
        lastActivityAt: c.inbox?.lastActivityAt ?? previous?.lastActivityAt ?? c.createdAt,
        lastMessage: last
          ? { sequence: last.sequence, senderPrincipalId: last.senderPrincipalId, previewText: last.previewText, attachmentCount: last.attachmentCount, lifecycle: last.lifecycle, createdAt: last.createdAt }
          : (previous?.lastMessage ?? null),
        memberRoles: roles,
        myRole: (me && roles[me]) || null,
        announcementOnly: !!c.announcementOnly,
        retentionSeconds: c.retentionSeconds ?? null,
        ...inboxFlags(c.inbox, previous),
      });
    }
    if (needNames.size) {
      try {
        const r = await identityApi.names([...needNames].slice(0, 256));
        const names = { ...this.snapshot.names };
        // Your own saved contact name (alias) first, as the contact list shows it.
        for (const p of r.principals) names[p.principalId] = p.alias?.trim() || p.displayName;
        this.set({ names });
      } catch {
        // Names are cosmetic; ids are shown instead.
      }
    }
    for (const v of views) v.title = this.titleOf(v);
    // Inbox order: latest activity first (MSG-007 `orderBy=lastActivity`).
    views.sort((a, b) => b.lastActivityAt.localeCompare(a.lastActivityAt) || b.createdAt.localeCompare(a.createdAt));
    this.set({ conversations: views });
  }

  /** Direct chat: the other person (alias, then name); group: its name, else its other members. */
  private titleOf(v: ConversationView): string {
    if (statusPostId(v.groupTitle)) {
      const owner = v.ownerPrincipalId;
      return owner === this.snapshot.identity?.principalId ? "Your status" : `Status from ${owner ? this.snapshot.names[owner] ?? "a contact" : "a contact"}`;
    }
    return conversationTitle({ contextClass: v.contextClass, serverTitle: v.serverTitle, groupTitle: v.groupTitle, members: v.members, me: this.snapshot.identity?.principalId, names: this.snapshot.names });
  }

  /** A private group's name changed on this device: the list, header and chat info follow. */
  private applyGroupTitle(conversationId: string, title: string | null): void {
    if (title) this.groupTitles.set(conversationId, title);
    else this.groupTitles.delete(conversationId);
    const v = this.snapshot.conversations.find((c) => c.id === conversationId);
    if (!v || v.securityProfile !== "PRIVATE_E2EE" || v.groupTitle === title) return;
    const next = { ...v, groupTitle: title };
    next.title = this.titleOf(next);
    this.set({ conversations: this.snapshot.conversations.map((c) => (c.id === conversationId ? next : c)) });
  }

  /**
   * Names a private group inside its end-to-end encrypted group: an encrypted message only
   * member devices can read (the server refuses to hold a PRIVATE_E2EE title). While nobody
   * else can decrypt yet it is held sealed on this device and sent with the held messages.
   */
  private async shareGroupTitle(conversationId: string, title: string): Promise<void> {
    const conv = this.server.get(conversationId);
    const c = this.crypto;
    if (!conv || conv.securityProfile !== "PRIVATE_E2EE" || conv.contextClass === "DIRECT" || !c || this.snapshot.phase !== "ready") return;
    const text = encodeGroupTitle(title);
    this.applyGroupTitle(conversationId, normalizeTitle(title));
    const gid = conv.encryption.mlsGroupId ? b64u.dec(conv.encryption.mlsGroupId) : null;
    const inGroup = !!gid && (await c.call<boolean>("hasGroup", { groupId: gid }).catch(() => false));
    if (!inGroup || (await this.updateAwaitingSetup(conv))) {
      await c.call("holdDraft", { conversationId, clientMessageId: uuid(), text, createdAt: new Date().toISOString() });
      this.deviceChecks.delete(conversationId);
      this.scheduleSync(conversationId);
      return;
    }
    await this.enqueue(conversationId, () => this.sendEncrypted(conv, { t: "text", text }, []));
  }

  /** A device or member was just added to a private group: it receives the group's name too. */
  private reshareGroupTitle(conversationId: string): void {
    const title = this.groupTitles.get(conversationId);
    if (title) void this.shareGroupTitle(conversationId, title).catch(() => {});
  }

  async directory(q?: string): Promise<Colleague[]> {
    return (await identityApi.directory(q)).people;
  }

  /**
   * Starts a conversation with colleagues. PRIVATE_E2EE builds the MLS group on this
   * device and adds every verified device of the others; MANAGED is server-readable
   * (the UI says so before it is chosen).
   */
  async createConversation(
    userIds: string[],
    profile: SecurityProfile,
    options: { cfMeetIds?: string[]; title?: string; expiresAt?: string; retentionSeconds?: number; announcementOnly?: boolean } = {},
  ): Promise<string> {
    const people = userIds.length ? (await identityApi.resolve(userIds)).people.map((p) => p.principalId) : [];
    const contacts = options.cfMeetIds ?? [];
    if (profile === "PRIVATE_E2EE" && this.snapshot.phase !== "ready") throw new MessagingApiError(0, "DEVICE_REQUIRED", null);
    const title = options.title?.trim() || undefined;
    const direct = people.length + contacts.length === 1 && !title;
    const disclosure = profile === "MANAGED" ? { managedDisclosureAcknowledged: true } : {};
    const body = direct
      ? people.length
        ? { contextClass: "DIRECT", counterpartPrincipalId: people[0], securityProfile: profile, ...disclosure }
        : { contextClass: "DIRECT", counterpartCfMeetId: contacts[0], securityProfile: profile, ...disclosure }
      : { contextClass: "GROUP", securityProfile: profile, ...disclosure, ...(title && profile === "MANAGED" ? { title: title.slice(0, 120) } : {}),
          ...(options.expiresAt ? { expiresAt: options.expiresAt, expiryGraceSeconds: 0 } : {}),
          ...(options.retentionSeconds ? { retentionSeconds: options.retentionSeconds } : {}) };
    const conv = await relay<ServerConversation>("POST", "v1/conversations", body, `web-${uuid()}`);
    if (conv.securityProfile !== profile) {
      // Never silently accept another profile than the one the user chose.
      throw new MessagingApiError(0, "PROFILE_NOT_GRANTED", conv.securityProfile);
    }
    this.server.set(conv.conversationId, conv);
    // Lock a status before adding its audience: recipients may read but never post.
    if (options.announcementOnly) await this.setAnnouncementOnly(conv.conversationId, true);
    let principals = people;
    if (!direct) {
      for (const p of people) await relay("POST", `v1/conversations/${conv.conversationId}/members`, { principalId: p });
      for (const cfMeetId of contacts) await relay("POST", `v1/conversations/${conv.conversationId}/members`, { cfMeetId });
      if (contacts.length && profile === "PRIVATE_E2EE") {
        const members = await this.conversationMembers(conv.conversationId);
        const me = this.snapshot.identity?.principalId;
        principals = members.map((x) => x.principalId).filter((x) => x !== me);
      }
    } else if (contacts.length && profile === "PRIVATE_E2EE") {
      const me = this.snapshot.identity?.principalId;
      principals = (conv.members ?? []).map((x) => x.principalId).filter((x) => x !== me);
    }
    if (profile === "PRIVATE_E2EE") {
      await this.establishGroup(conv, principals);
      // The name travels inside the encrypted group, after the members' devices were added.
      if (title && !direct) {
        const shared = this.shareGroupTitle(conv.conversationId, title.slice(0, 120));
        if (statusPostId(title)) await shared;
        else await shared.catch(() => {});
      }
    }
    this.subscribeConversation(conv);
    await this.refreshConversations();
    return conv.conversationId;
  }

  /** Opens (or returns the existing) MANAGED direct chat with a connected contact. */
  startDirectWithContact(cfMeetId: string): Promise<string> {
    return this.createConversation([], "MANAGED", { cfMeetIds: [cfMeetId] });
  }

  /**
   * Claims, verifies and adds the devices of `principals` with ONE commit. A new group
   * publishes its GroupInfo at once (with the add, or a key-update commit when nobody else
   * has a device yet), so every member's device can join it by itself later.
   */
  private addPrincipalsToGroup(conv: ServerConversation, principals: string[], create: boolean): Promise<void> {
    return this.withMlsLock(async () => {
      const c = this.crypto!;
      const gid = b64u.dec(conv.encryption.mlsGroupId!);
      const deviceId = this.snapshot.deviceId!;
      if (create) await c.call("createGroup", { groupId: gid });
      try {
        const packages: Uint8Array[] = [];
        const welcomed: string[] = [];
        if (principals.length) {
          const claim = await relay<{ keyPackages: { principalId: string; deviceId: string; keyPackage: string }[] }>(
            "POST",
            `v1/conversations/${conv.conversationId}/mls/key-package-claims`,
            { principalIds: principals },
          );
          const now = Math.floor(Date.now() / 1000);
          for (const k of claim.keyPackages) {
            if (k.deviceId === deviceId) continue;
            const kp = b64u.dec(k.keyPackage);
            const info = await c.call<{ deviceId: string; signatureKey: Uint8Array }>("inspectKeyPackage", { keyPackage: kp, now });
            if (info.deviceId !== k.deviceId) throw new CryptoWorkerError("KEY_PACKAGE_DEVICE_MISMATCH");
            await this.verifyTransparency(k.deviceId, info.signatureKey);
            packages.push(kp);
            welcomed.push(k.deviceId);
          }
          // A member without a published KeyPackage (no device yet, or none left) is not
          // added here: its devices join by themselves from the GroupInfo, and senders add
          // new devices before they send (addMissingDevices).
        }
        let out: GroupChange | null = null;
        if (packages.length) out = await c.call<GroupChange>("addDevices", { groupId: gid, keyPackages: packages });
        else if (create) out = await c.call<GroupChange>("updateKeys", { groupId: gid });
        if (out) await this.submitChange(conv, out, { welcomed, removed: [] });
        else await c.call("confirm");
      } catch (e) {
        await c.call("rollback").catch(() => {});
        throw e;
      }
    });
  }

  private async establishGroup(conv: ServerConversation, others: string[]): Promise<void> {
    await this.addPrincipalsToGroup(conv, others, true);
    await this.updateAwaitingSetup(conv);
  }

  /** This account's ACTIVE messaging devices (other browsers included), cached for 30 s. */
  private async ownDeviceIds(): Promise<Set<string>> {
    if (this.ownDeviceCache && Date.now() - this.ownDeviceCache.at < 30_000) return this.ownDeviceCache.ids;
    const ids = new Set((await this.ownDevices()).filter((d) => d.status === "ACTIVE").map((d) => d.deviceId));
    this.ownDeviceCache = { at: Date.now(), ids };
    return ids;
  }

  /**
   * Whether an encrypted conversation can reach anyone else: true when at least one device
   * of ANOTHER member is in this device's MLS group. Otherwise the conversation is pending
   * setup and messages are held (sealed) instead of being sent to nobody.
   */
  private async othersInGroup(conv: ServerConversation): Promise<boolean> {
    if (!this.crypto || !conv.encryption.mlsGroupId) return false;
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (!(await this.crypto.call<boolean>("hasGroup", { groupId: gid }))) return false;
    const inTree = await this.crypto.call<string[]>("members", { groupId: gid });
    const mine = await this.ownDeviceIds();
    return inTree.some((d) => d !== this.snapshot.deviceId && !mine.has(d));
  }

  private async updateAwaitingSetup(conv: ServerConversation): Promise<boolean> {
    if (conv.securityProfile !== "PRIVATE_E2EE" || this.snapshot.phase !== "ready") return false;
    const reachable = await this.othersInGroup(conv).catch(() => true);
    const id = conv.conversationId;
    const waiting = !reachable;
    if (Boolean(this.snapshot.awaitingSetup[id]) !== waiting) {
      const next = { ...this.snapshot.awaitingSetup };
      if (waiting) next[id] = true;
      else delete next[id];
      this.set({ awaitingSetup: next });
    }
    return waiting;
  }

  /** Sends the messages held while the conversation was pending setup, oldest first. */
  private async flushDrafts(conversationId: string): Promise<void> {
    const conv = this.server.get(conversationId);
    const c = this.crypto;
    if (!conv || !c || this.snapshot.phase !== "ready") return;
    if (await this.updateAwaitingSetup(conv)) return;
    await this.enqueue(conversationId, async () => {
      const drafts = await c.call<{ clientMessageId: string; text: string; createdAt: string }[]>("drafts", { conversationId }).catch(() => []);
      for (const d of drafts) {
        await this.sendEncrypted(conv, { t: "text", text: d.text }, []);
        await c.call("dropDraft", { conversationId, clientMessageId: d.clientMessageId });
        this.settlePending(conversationId, `d${d.clientMessageId}`, null);
      }
      if (drafts.length) await this.sync(conversationId);
    }).catch(() => {
      // retried on the next poll; the drafts stay sealed on this device
    });
  }

  private async verifyTransparency(deviceId: string, signatureKey: Uint8Array): Promise<void> {
    const known = await this.crypto!.call<number | null>("knownTreeSize");
    const proof = await relay<unknown>(
      "GET",
      `v1/messaging/key-transparency/proofs?deviceId=${deviceId}${known ? `&fromTreeSize=${known}` : ""}`,
    );
    await this.crypto!.call("verifyTransparency", { proof, logKey: KT_LOG_KEY || null, logId: KT_LOG_ID || null, expectedDeviceId: deviceId, expectedKey: signatureKey });
  }

  async addMember(conversationId: string, userId: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) return;
    const [p] = (await identityApi.resolve([userId])).people;
    await relay("POST", `v1/conversations/${conversationId}/members`, { principalId: p.principalId });
    if (conv.securityProfile === "PRIVATE_E2EE") {
      await this.sync(conversationId);
      await this.addPrincipalsToGroup(conv, [p.principalId], false);
      this.reshareGroupTitle(conversationId);
    }
    await this.refreshConversations();
  }

  async removeMember(conversationId: string, principalId: string): Promise<void> {
    await relay("DELETE", `v1/conversations/${conversationId}/members/${principalId}`);
    await this.sync(conversationId);
    await this.refreshConversations();
  }

  /** Adds a connected contact by CF Meet ID (Messaging `AddMemberRequest.cfMeetId`). */
  async addMemberByCfMeetId(conversationId: string, cfMeetId: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) return;
    const member = await relay<{ principalId: string }>("POST", `v1/conversations/${conversationId}/members`, { cfMeetId });
    if (conv.securityProfile === "PRIVATE_E2EE") {
      await this.sync(conversationId);
      await this.addPrincipalsToGroup(conv, [member.principalId], false);
      this.reshareGroupTitle(conversationId);
    }
    await this.refreshConversations();
  }

  /** Leaves a conversation (acting = the member). */
  async leaveConversation(conversationId: string): Promise<void> {
    const me = this.snapshot.identity?.principalId;
    if (!me) return;
    await relay("DELETE", `v1/conversations/${conversationId}/members/${me}`);
    await this.refreshConversations();
  }

  async setMemberRole(conversationId: string, principalId: string, role: MemberRole): Promise<void> {
    await relay("PATCH", `v1/conversations/${conversationId}/members/${principalId}`, { role });
    await this.refreshConversations();
  }

  /** Renames a MANAGED group (compare-and-swap on the conversation version). */
  /** S13: only owners and admins can send in this group. */
  /**
   * F4-F6: send a poll, a vote, a location or a contact card. Private chats only: it travels
   * inside the end-to-end encrypted content (the server sees an ordinary encrypted message).
   */
  async sendRich(conversationId: string, payload: RichPayload): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) throw new MessagingApiError(0, "NOT_FOUND", "That chat is no longer available.");
    if (conv.securityProfile !== "PRIVATE_E2EE") throw new MessagingApiError(0, "PRIVATE_CHATS_ONLY", null);
    await this.enqueue(conversationId, async () => {
      await this.sendEncrypted(conv, payload, []);
      // A read outage after publication must not encourage duplicate broadcast sends.
      await this.sync(conversationId).catch(() => {});
    });
  }

  /** F1: archive / pin this chat for me (WhatsApp); the list updates straight away. */
  async setInboxSettings(conversationId: string, change: { archived?: boolean; pinned?: boolean }): Promise<void> {
    const r = await relay<InboxFlags>("PUT", `v1/conversations/${conversationId}/inbox-settings`, change);
    const flags = inboxFlags(r);
    const c = this.server.get(conversationId);
    if (c?.inbox) c.inbox = { ...c.inbox, ...flags };
    this.set({ conversations: this.snapshot.conversations.map((x) => (x.id === conversationId ? { ...x, ...flags } : x)) });
  }

  /** B4: turn disappearing messages on, change them, or turn them off (null); applies to new messages. */
  async setDisappearing(conversationId: string, seconds: number | null): Promise<void> {
    const current = await relay<ServerConversation>("GET", `v1/conversations/${conversationId}`);
    await relay("PATCH", `v1/conversations/${conversationId}`, { expectedVersion: current.version ?? 0, retentionSeconds: seconds });
    await this.refreshConversations();
  }

  async setAnnouncementOnly(conversationId: string, on: boolean): Promise<void> {
    const current = await relay<ServerConversation>("GET", `v1/conversations/${conversationId}`);
    await relay("PATCH", `v1/conversations/${conversationId}`, { expectedVersion: current.version ?? 0, announcementOnly: on });
    await this.refreshConversations();
  }

  /**
   * S11: forward a message. MANAGED → MANAGED goes through the server (it copies the message);
   * anything private is re-sent from this device as a new message (the server can't read it).
   */
  async forwardMessage(sourceId: string, messageId: string | null, text: string | null, targetId: string): Promise<void> {
    const source = this.server.get(sourceId);
    const target = this.server.get(targetId);
    if (!source || !target) throw new MessagingApiError(0, "NOT_FOUND", "That chat isn't available.");
    if (source.securityProfile === "MANAGED" && target.securityProfile === "MANAGED" && messageId) {
      await relay("POST", `v1/conversations/${sourceId}/messages/${messageId}/forward`, { targetConversationId: targetId, clientMessageId: uuid() });
      this.scheduleSync(targetId);
      return;
    }
    if (!text?.trim()) throw new MessagingApiError(0, "CONTENT_REJECTED", "Only text messages can be forwarded.");
    await this.send(targetId, text);
  }

  /** S12: invite links for a group (admins). */
  inviteLinks(conversationId: string) {
    return relay<{ inviteLinks: InviteLink[] }>("GET", `v1/conversations/${conversationId}/invite-links`).then((r) => r.inviteLinks ?? []);
  }
  createInviteLink(conversationId: string, opts: { expiresInSeconds?: number; maxUses?: number } = {}) {
    return relay<InviteLink & { token: string }>("POST", `v1/conversations/${conversationId}/invite-links`, opts);
  }
  revokeInviteLink(conversationId: string, linkId: string) {
    return relay("POST", `v1/conversations/${conversationId}/invite-links/${linkId}/revoke`, {});
  }
  async joinByInviteLink(token: string): Promise<string | null> {
    const r = await relay<{ conversationId?: string; conversation?: { conversationId?: string } }>("POST", "v1/messaging/invite-links/join", { token });
    await this.refreshConversations().catch(() => {});
    return r.conversationId ?? r.conversation?.conversationId ?? null;
  }

  /** S3: hand the group to another member (owner only). */
  async transferOwnership(conversationId: string, principalId: string): Promise<void> {
    await relay("POST", `v1/conversations/${conversationId}/owner`, { principalId });
    await this.refreshConversations();
  }

  /** S2: report messages. Private chats disclose the reported text from this device. */
  async reportMessages(conversationId: string, messages: { messageId: string; text: string | null }[], category: string, note?: string): Promise<void> {
    const conv = this.server.get(conversationId);
    const disclosure = conv?.securityProfile === "PRIVATE_E2EE" ? messages.filter((m) => m.text).map((m) => ({ messageId: m.messageId, text: m.text! })) : [];
    await relay("POST", `v1/conversations/${conversationId}/reports`, { category, messageIds: messages.map((m) => m.messageId), ...(note ? { note } : {}), ...(disclosure.length ? { disclosure } : {}) });
  }

  async renameConversation(conversationId: string, title: string | null): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) return;
    if (conv.securityProfile === "PRIVATE_E2EE") {
      // A private group's name is never sent to the server in the clear.
      const name = normalizeTitle(title?.slice(0, 120));
      if (!name) throw new MessagingApiError(0, "VALIDATION_ERROR", "Enter a group name.");
      await this.shareGroupTitle(conversationId, name);
      return;
    }
    const current = await relay<ServerConversation>("GET", `v1/conversations/${conversationId}`);
    await relay("PATCH", `v1/conversations/${conversationId}`, { expectedVersion: current.version ?? conv.version ?? 0, title: title?.trim() ? title.trim().slice(0, 120) : null });
    await this.refreshConversations();
  }

  /**
   * Cryptographic removal: a member device commits the removals the server requires. A
   * pending device that is not in the tree (revoked before it ever joined) is attested
   * too — with a key-update commit when no leaf has to go — so it never blocks the chat.
   */
  private async commitPendingRemovals(conv: ServerConversation, state: MlsState | null): Promise<void> {
    const c = this.crypto;
    if (!c || !state || conv.securityProfile !== "PRIVATE_E2EE" || !conv.encryption.mlsGroupId) return;
    const pending = state.pendingRemovals.map((r) => r.deviceId);
    if (!pending.length) return;
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    await this.withMlsLock(async () => {
      if (!(await c.call<boolean>("hasGroup", { groupId: gid }))) return;
      if (Number(await c.call<number>("epoch", { groupId: gid })) !== state.mlsEpoch) return;
      const plan = removalPlan(pending, new Set(await c.call<string[]>("members", { groupId: gid })), this.snapshot.deviceId);
      if (!plan) return;
      const out = plan.remove.length
        ? await c.call<GroupChange>("removeDevices", { groupId: gid, deviceIds: plan.remove })
        : await c.call<GroupChange>("updateKeys", { groupId: gid });
      // Another member's device may commit first (epoch moved): ours is discarded.
      await this.submitChange(conv, out, { removed: plan.attest }).catch(() => {});
    });
  }

  /**
   * Multi-device (WhatsApp): an ACTIVE device of a member that is not yet in the MLS tree
   * (a new browser, a phone) is claimed, verified and added by a member device. Before
   * every send (`urgent`) the sender does it itself, so what it sends reaches every device
   * the others registered; otherwise one designated member device does it (others step
   * in after a grace period). The new device does not depend on either: it joins by
   * itself (joinBySelf). Concurrent attempts are resolved by the Delivery Service's epoch
   * order (the loser rolls back).
   */
  private async addMissingDevices(conv: ServerConversation, urgent = false): Promise<void> {
    const id = conv.conversationId;
    const checks = urgent ? this.sendDeviceChecks : this.deviceChecks;
    if ((!urgent && Date.now() - (checks.get(id) ?? 0) < DEVICE_CHECK_MS) || !this.crypto || !conv.encryption.mlsGroupId) return;
    checks.set(id, Date.now());
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (!(await this.crypto.call<boolean>("hasGroup", { groupId: gid }))) return;
    const inTree = new Set(await this.crypto.call<string[]>("members", { groupId: gid }));
    const active = (await this.conversationMembers(id)).filter((m) => !m.state || m.state === "ACTIVE");
    let unavailable = false;
    const lookups = new Map(
      await Promise.all(active.map(async (m) => [m.principalId, await this.principalDevices(m.principalId, 0).catch(() => {
        unavailable = true;
        return [] as ServerDevice[];
      })] as const)),
    );
    const { principals: missing, deviceIds, activeInTree } = missingDevices(active, (p) => lookups.get(p) ?? [], inTree, this.snapshot.deviceId);
    const listed = [...lookups.values()].flat();
    if (unavailable) {
      checks.delete(id);
      this.set({ detail: "Some devices are waiting for synchronization. Device discovery will retry." });
    }
    // A device without published KeyPackages (e.g. a browser whose storage was cleared) is
    // not claimed again for a while; if it is alive it joins by itself.
    const newDevices = deviceIds
      .filter((d) => Date.now() - (this.unclaimable.get(d) ?? 0) > UNCLAIMABLE_RETRY_MS)
      .map((d) => listed.find((x) => x.deviceId === d)!);
    if (!missing.length) {
      this.missingSince.delete(id);
      return;
    }
    const designated = activeInTree.sort()[0];
    if (!urgent && designated !== this.snapshot.deviceId) {
      const seen = this.missingSince.get(id) ?? Date.now();
      this.missingSince.set(id, seen);
      if (Date.now() - seen < 2 * DEVICE_CHECK_MS) return;
    }
    // Only devices whose listed key verifies (against the transparency log, when pinned)
    // are claimed; one the log does not include yet is retried on a later check.
    const verified: string[] = [];
    for (const d of newDevices) {
      const ok = await this.verifyTransparency(d.deviceId, b64u.dec(d.signaturePublicKey)).then(
        () => true,
        () => false,
      );
      if (ok) verified.push(d.deviceId);
    }
    if (!verified.length) return;
    let added = false;
    try {
      added = await this.addDevicesOf(conv, missing, verified);
    } catch (e) {
      // The group moved on (another commit won the epoch): catch up once and add again.
      // (Only before a send: during a sync the next sync catches up.)
      if (!urgent || !(e instanceof MessagingApiError && e.status === 409)) throw e;
      await this.sync(id);
      added = await this.addDevicesOf(conv, missing, verified);
    }
    // A private group's name travels inside the group: new devices get it again.
    if (added) this.reshareGroupTitle(id);
    if (this.snapshot.awaitingSetup[id]) void this.flushDrafts(id);
  }

  /**
   * Adds the given new devices of `principals` with one commit: the claim takes only
   * their KeyPackages (never one of a device already in the tree).
   */
  private addDevicesOf(conv: ServerConversation, principals: string[], deviceIds: string[]): Promise<boolean> {
    return this.withMlsLock(async () => {
      const c = this.crypto!;
      const gid = b64u.dec(conv.encryption.mlsGroupId!);
      const inTree = new Set(await c.call<string[]>("members", { groupId: gid }));
      const wanted = deviceIds.filter((d) => !inTree.has(d) && d !== this.snapshot.deviceId);
      if (!wanted.length) return false;
      const path = `v1/conversations/${conv.conversationId}/mls/key-package-claims`;
      type Claim = { keyPackages: { principalId: string; deviceId: string; keyPackage: string }[] };
      const claim = await relay<Claim>("POST", path, { principalIds: principals, deviceIds: wanted }).catch((e) =>
        // An older server does not take deviceIds.
        e instanceof MessagingApiError && e.status === 400 ? relay<Claim>("POST", path, { principalIds: principals }) : Promise.reject(e),
      );
      for (const d of wanted) if (!claim.keyPackages.some((k) => k.deviceId === d)) this.unclaimable.set(d, Date.now());
      const now = Math.floor(Date.now() / 1000);
      const packages: Uint8Array[] = [];
      const welcomed: string[] = [];
      for (const k of claim.keyPackages) {
        if (inTree.has(k.deviceId) || k.deviceId === this.snapshot.deviceId) continue;
        const kp = b64u.dec(k.keyPackage);
        const info = await c.call<{ deviceId: string; signatureKey: Uint8Array }>("inspectKeyPackage", { keyPackage: kp, now });
        if (info.deviceId !== k.deviceId) continue;
        await this.verifyTransparency(k.deviceId, info.signatureKey);
        packages.push(kp);
        welcomed.push(k.deviceId);
      }
      if (!packages.length) return false;
      const out = await c.call<GroupChange>("addDevices", { groupId: gid, keyPackages: packages });
      await this.submitChange(conv, out, { welcomed, removed: [] });
      return true;
    });
  }

  private async checkWelcomes(): Promise<void> {
    const deviceId = this.snapshot.deviceId;
    if (!deviceId || !this.crypto || this.snapshot.phase !== "ready") return;
    const page = await relay<{ welcomes: { welcomeId: string; conversationId: string; welcome: string }[] }>(
      "GET",
      `v1/messaging/devices/${deviceId}/welcomes`,
    );
    let joined = 0;
    for (const w of page.welcomes) {
      let conv = this.server.get(w.conversationId);
      if (!conv) {
        conv = await relay<ServerConversation>("GET", `v1/conversations/${w.conversationId}`);
        this.server.set(conv.conversationId, conv);
        this.transport?.subscribe(conv.conversationId, 0);
      }
      if (conv.securityProfile !== "PRIVATE_E2EE" || !conv.encryption.mlsGroupId) continue;
      const gid = b64u.dec(conv.encryption.mlsGroupId);
      if (!(await this.crypto.call<boolean>("hasGroup", { groupId: gid }))) {
        try {
          const c = this.crypto;
          await this.withMlsLock(() => c.call("join", { groupId: gid, welcome: b64u.dec(w.welcome) }));
          joined++;
        } catch {
          // A transient failure must not consume the only Welcome.
          continue;
        }
      }
      await relay("POST", `v1/messaging/devices/${deviceId}/welcomes/${w.welcomeId}/consume`, {});
      this.scheduleSync(conv.conversationId);
    }
    if (joined) {
      await this.ensureKeyPackageStock(true).catch(() => {});
      await this.renderConversations();
    }
  }

  // ------------------------------------------------------------------ sync

  private scheduleSync(conversationId: string): void {
    if (this.syncing.has(conversationId)) {
      this.resync.add(conversationId);
      return;
    }
    void this.sync(conversationId);
  }

  /** Serialized per conversation: fetch after the device's cursor, process in order. */
  sync(conversationId: string): Promise<void> {
    const running = this.syncing.get(conversationId);
    if (running) {
      this.resync.add(conversationId);
      return running;
    }
    const run = this.doSync(conversationId)
      .catch(() => {})
      .finally(() => {
        this.syncing.delete(conversationId);
        if (this.resync.delete(conversationId)) void this.sync(conversationId);
      });
    this.syncing.set(conversationId, run);
    return run;
  }

  private async doSync(conversationId: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) return;
    if (conv.securityProfile === "MANAGED") return this.syncManaged(conv);
    const c = this.crypto;
    if (!c || !this.snapshot.deviceId || !conv.encryption.mlsGroupId) {
      await this.loadWaiting(conv);
      await this.renderE2ee(conversationId);
      return;
    }
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    let inGroup = await c.call<boolean>("hasGroup", { groupId: gid });
    // Not in this chat's group yet: join it by itself (nobody else has to be online).
    if (!inGroup && this.snapshot.phase === "ready") inGroup = await this.joinBySelf(conv).catch(() => false);
    if (inGroup) {
      // External commits (members' devices that joined by themselves) are merged only
      // after their device is verified; one that fails verification is refused.
      const allowedJoiners: string[] = [];
      const rejectedJoiners: string[] = [];
      const retryAfter = await c.call<number | null>("retryCursor", { conversationId });
      let retrying = retryAfter !== null;
      for (let round = 0; round < 200; round++) {
        const after = retrying ? retryAfter! : await c.call<number>("cursor", { conversationId });
        const page = await relay<{ messages: ServerMessage[]; visibleFromSequence?: number }>("GET", `v1/conversations/${conversationId}/messages?afterSequence=${after}&limit=${PAGE}`);
        this.notePrivateVisibility(conversationId, page.visibleFromSequence);
        if (!page.messages.length) {
          if (page.visibleFromSequence !== undefined) await this.withMlsLock(() => c.call("processMessages", { conversationId, groupId: gid, visibleFromSequence: page.visibleFromSequence, messages: [] }));
          break;
        }
        this.noteMeta(conversationId, page.messages);
        const processed = await this.withMlsLock(() => c.call<{ records: StoredRecord[]; joiner?: PendingJoiner | null; blocked?: string | null }>("processMessages", {
          conversationId,
          groupId: gid,
          visibleFromSequence: page.visibleFromSequence,
          allowedJoiners,
          rejectedJoiners,
          messages: page.messages.map((m) => ({
            sequence: m.sequence,
            messageId: m.messageId,
            kind: m.kind,
            replyToMessageId: m.replyToMessageId, targetMessageId: m.targetMessageId,
            lifecycle: m.lifecycle,
            senderPrincipalId: m.senderPrincipalId,
            senderDeviceId: m.senderDeviceId ?? null,
            clientMessageId: m.clientMessageId,
            createdAt: m.createdAt,
            epoch: m.e2ee?.epoch ?? 0,
            ciphertext: m.e2ee?.ciphertext ?? "",
            attachmentIds: m.attachmentIds,
          })),
        }));
        this.privateWindows.get(conversationId)?.updateKnown(processed.records);
        if (processed.joiner) {
          const verdict = await this.verifyJoiner(conv, processed.joiner);
          if (verdict) allowedJoiners.push(verdict);
          else rejectedJoiners.push(processed.joiner.deviceId);
          continue;
        }
        if (processed.blocked) { await this.renderE2ee(conversationId); return; }
        if (retrying) { retrying = false; continue; }
        // Another device's commit may have used one of this device's KeyPackages: keep the stock up.
        if (processed.records.some((r) => r.status === "COMMIT" && r.senderDeviceId !== this.snapshot.deviceId)) void this.ensureKeyPackageStock(true).catch(() => {});
        if (page.messages.length < PAGE) break;
      }
      this.waiting.delete(conversationId);
      const state = await this.mlsState(conv).catch(() => null);
      await this.commitPendingRemovals(conv, state);
      await this.ensureGroupInfo(conv, state);
      await this.addMissingDevices(conv);
    } else {
      await this.loadWaiting(conv);
    }
    await this.renderE2ee(conversationId);
  }

  /**
   * A chat this device is not in yet: its latest messages are listed (metadata only) and
   * shown as "Waiting for this message" rather than an empty chat.
   */
  private async loadWaiting(conv: ServerConversation): Promise<void> {
    const id = conv.conversationId;
    const last = conv.inbox?.lastMessage?.sequence ?? 0;
    if (!last) return;
    const page = await relay<{ messages: ServerMessage[]; visibleFromSequence?: number }>("GET", `v1/conversations/${id}/messages?afterSequence=${Math.max(0, last - PAGE)}&limit=${PAGE}`).catch(() => null);
    if (!page) return;
    this.notePrivateVisibility(id, page.visibleFromSequence);
    this.noteMeta(id, page.messages);
    this.waiting.set(id, page.messages.filter((m) => m.kind === "APPLICATION"));
  }

  private async readPrivatePage(conversationId: string, beforeSequence?: number): Promise<PrivateHistoryWindow | null> {
    const crypto = this.crypto, generation = this.generation;
    if (!crypto || !this.snapshot.deviceId || this.stopped) return null;
    const window = this.privateWindows.get(conversationId) ?? new PrivateHistoryWindow();
    this.privateWindows.set(conversationId, window);
    const page = await crypto.call<PrivateHistoryPage>("historyPage", { conversationId, limit: 200, ...(beforeSequence === undefined ? {} : { beforeSequence }) });
    if (this.stopped || crypto !== this.crypto || generation !== this.generation || this.privateWindows.get(conversationId) !== window) return null;
    window.prune(this.privateVisibility.get(conversationId) ?? 1);
    const visibleFrom = this.privateVisibility.get(conversationId) ?? 1;
    window.merge({ records: page.records.filter(record => record.sequence >= visibleFrom), nextBeforeSequence: page.nextBeforeSequence !== null && page.nextBeforeSequence > visibleFrom ? page.nextBeforeSequence : null }, beforeSequence);
    this.set({ hasOlder: { ...this.snapshot.hasOlder, [conversationId]: window.cursor !== null } });
    return window;
  }

  private notePrivateVisibility(conversationId: string, first: number | undefined): void {
    if (first === undefined) return;
    if (!Number.isSafeInteger(first) || first < 1) throw new MessagingApiError(0, "HISTORY_VISIBILITY_REQUIRED", null);
    const bound = Math.max(first, this.privateVisibility.get(conversationId) ?? 1);
    this.privateVisibility.set(conversationId, bound);
    this.privateWindows.get(conversationId)?.prune(bound);
  }

  private async renderE2ee(conversationId: string): Promise<void> {
    const me = this.snapshot.identity?.principalId ?? null;
    const hadCrypto = !!this.crypto && !!this.snapshot.deviceId;
    const window = hadCrypto ? await this.readPrivatePage(conversationId) : null;
    if (this.stopped || (hadCrypto && !window)) return;
    const records = window?.ordered ?? [];
    const views: MessageView[] = [];
    const meta = this.metaFor(conversationId);
    // Older history comes from another device of this account (history transfer), if any.
    let olderNote = beforeJoinNote(false);
    // Registration time is not MLS membership evidence. Only verified worker
    // provenance may classify a message as predating device membership.
    const waitingHere = this.waiting.get(conversationId) ?? [];
    if (records.some((r) => (r.status === "BEFORE_JOIN" && r.beforeJoinVerified))) {
      const own = await this.ownDeviceIds().catch(() => new Set<string>());
      olderNote = beforeJoinNote([...own].some((d) => d !== this.snapshot.deviceId));
    }
    const reactionTargets = new Map<string, number>();
    const reactionControls: PrivateReactionRecord[] = [];
    for (const record of records) {
      const lifecycle = record.messageId ? meta.get(record.messageId)?.lifecycle ?? record.lifecycle ?? "ACTIVE" : "ACTIVE";
      if (record.kind !== "APPLICATION" || !["SENT", "DECRYPTED"].includes(record.status) || lifecycle !== "ACTIVE") continue;
      if (record.payload?.t === "reaction") {
        if (record.senderPrincipalId) reactionControls.push({ ...record.payload, sender: record.senderPrincipalId, sequence: record.sequence });
      } else if (record.messageId && record.payload?.t !== "vote" && !(record.payload?.t === "text" && isGroupTitleText(record.payload.text))) reactionTargets.set(record.messageId, record.sequence);
    }
    const reactions = privateReactionState(reactionControls, reactionTargets, me);
    const pinned = new Set((this.snapshot.pins[conversationId] ?? []).map((p) => p.messageId));
    // Edits are encrypted messages that point at the original (the link is server metadata).
    const edits = new Map<string, string>();
    for (const r of records) {
      const target = r.messageId ? meta.get(r.messageId)?.target ?? r.targetMessageId : null;
      if (r.kind === "EDIT" && target && (r.status === "DECRYPTED" || r.status === "SENT") && r.payload?.t === "text") edits.set(target, r.payload.text);
    }
    // A private group's name travels as an encrypted control message: it renames the chat and
    // shows as a note only when it changes the name (re-shares for new devices stay silent).
    const titleRecords = records
      .filter((r) => r.kind !== "EDIT" && (r.status === "DECRYPTED" || r.status === "SENT") && r.payload?.t === "text" && isGroupTitleText(r.payload.text))
      .filter((r) => { const l = r.messageId ? meta.get(r.messageId)?.lifecycle : undefined; return !l || l === "ACTIVE" || l === "HIDDEN_FOR_YOU"; })
      .map((r) => ({ sequence: r.sequence, senderPrincipalId: r.senderPrincipalId ?? null, text: r.payload?.t === "text" ? r.payload.text : null }));
    const named = groupTitleFromRecords(titleRecords);
    // F4: votes are encrypted messages of their own, counted on the poll they point at.
    const votes: VoteRecord[] = [];
    for (const r of records) {
      if ((r.status === "DECRYPTED" || r.status === "SENT") && r.payload?.t === "vote") votes.push({ poll: r.payload.poll, choices: r.payload.choices, sender: r.senderPrincipalId ?? null, sequence: r.sequence });
    }
    for (const r of records) {
      if (r.status === "COMMIT" || r.kind === "EDIT" || r.payload?.t === "vote" || r.payload?.t === "reaction" || r.privateControl === "REACTION") continue;
      if ((r.status === "DECRYPTED" || r.status === "SENT") && r.payload?.t === "text" && isGroupTitleText(r.payload.text)) {
        const t = named.changes.get(r.sequence);
        if (t) {
          const mine = r.senderPrincipalId === me;
          const who = mine ? "You" : (r.senderPrincipalId && this.snapshot.names[r.senderPrincipalId]) || "Someone";
          views.push({ key: `s${r.sequence}`, sequence: r.sequence, senderPrincipalId: r.senderPrincipalId ?? null, mine, createdAt: r.createdAt ?? null, profile: "PRIVATE_E2EE", state: "system", text: null, file: null, note: `${who} named the group “${t}”`, transferred: Boolean(r.transferred), messageId: r.messageId ?? null });
        }
        continue;
      }
      const m = r.messageId ? meta.get(r.messageId) ?? { lifecycle: r.lifecycle ?? "ACTIVE", replyTo: r.replyToMessageId ?? null, target: r.targetMessageId ?? null } : undefined;
      const base: MessageView = {
        key: `s${r.sequence}`,
        sequence: r.sequence,
        senderPrincipalId: r.senderPrincipalId ?? null,
        mine: r.senderPrincipalId === me,
        createdAt: r.createdAt ?? null,
        profile: "PRIVATE_E2EE",
        state: "delivered",
        text: null,
        file: null,
        note: null,
        transferred: Boolean(r.transferred),
        messageId: r.messageId ?? null,
        reactions: r.messageId ? reactions?.get(r.messageId) ?? [] : [],
        pinned: !!r.messageId && pinned.has(r.messageId),
      };
      if (m && m.lifecycle !== "ACTIVE") {
        base.state = "removed";
        base.reactions = [];
        base.note = m.lifecycle === "HIDDEN_FOR_YOU" ? "Hidden for you" : m.lifecycle === "MODERATED" ? "Removed by a moderator" : "Message deleted";
        views.push(base);
        continue;
      }
      if (r.status === "DECRYPTED" || r.status === "SENT") {
        base.state = r.status === "SENT" ? "sent" : "delivered";
        if (r.payload?.t === "text") base.text = r.payload.text;
        const rich = richContent(r.payload, r.messageId ?? null, votes, me ?? null);
        if (rich) {
          base.rich = rich;
          base.text = richSummary(r.payload) ?? base.text;
        }
        if (r.messageId && edits.has(r.messageId)) {
          base.text = edits.get(r.messageId) ?? base.text;
          base.editCount = 1;
        }
        else if (r.payload?.t === "file") {
          base.text = r.payload.caption;
          base.file = { name: r.fileName ?? r.payload.name ?? "attachment", mediaType: r.mediaType ?? r.payload.mediaType ?? "application/octet-stream", size: r.size ?? null, attachmentId: r.attachmentId ?? r.payload.attachmentId };
        }
      } else if (r.status === "SENT_ELSEWHERE") {
        base.state = "system";
        base.note = "Sent from another of your devices";
      } else if (r.status === "BEFORE_JOIN" && r.beforeJoinVerified) {
        base.state = "before-join";
        base.note = olderNote;
      } else if (r.status === "REMOVED") {
        base.state = "removed";
        base.note = "Message removed";
      } else {
        base.state = "unreadable";
        base.note = `This message could not be decrypted on this device (${r.error ?? "FAILED"})`;
      }
      views.push(base);
    }
    // Not in this chat's group yet: its latest messages wait (placeholders, never nothing).
    const local = new Set(records.map((r) => r.sequence));
    for (const m of waitingHere) {
      if (local.has(m.sequence)) continue;
      const removed = m.lifecycle !== "ACTIVE";
      views.push({
        key: `s${m.sequence}`,
        sequence: m.sequence,
        senderPrincipalId: m.senderPrincipalId,
        mine: m.senderPrincipalId === me,
        createdAt: m.createdAt,
        profile: "PRIVATE_E2EE",
        state: removed ? "removed" : "unreadable",
        text: null,
        file: null,
        note: removed ? "Message deleted" : WAITING_NOTE,
        transferred: false,
        messageId: m.messageId,
        reactions: [],
        pinned: false,
      });
    }
    views.sort((a, b) => (a.sequence ?? 0) - (b.sequence ?? 0));
    // Replies: the quoted message is read from this device's own history.
    const byId = new Map(views.filter((v) => v.messageId).map((v) => [v.messageId as string, v]));
    for (const v of views) {
      const to = v.messageId ? meta.get(v.messageId)?.replyTo : null;
      if (!to) continue;
      const q = byId.get(to);
      v.replyTo = { messageId: to, senderPrincipalId: q?.senderPrincipalId ?? null, text: q && q.state !== "removed" ? q.text ?? (q.file ? q.file.name : null) : null, removed: q?.state === "removed" };
    }
    const pending = this.pendingSends.get(conversationId) ?? [];
    // Held drafts survive reloads (sealed in the worker): show them as pending setup.
    if (this.crypto && this.snapshot.deviceId) {
      const drafts = await this.crypto.call<{ clientMessageId: string; text: string; createdAt: string }[]>("drafts", { conversationId }).catch(() => []);
      for (const d of drafts) {
        const key = `d${d.clientMessageId}`;
        // A held group name is not a message: it names the chat here and is sent with the held messages.
        const heldTitle = decodeGroupTitle(d.text);
        if (heldTitle || isGroupTitleText(d.text)) {
          if (heldTitle) named.title = heldTitle;
          continue;
        }
        if (pending.some((m) => m.key === key)) continue;
        pending.push({ key, sequence: null, senderPrincipalId: me, mine: true, createdAt: d.createdAt, profile: "PRIVATE_E2EE", state: "pending-setup", text: d.text, file: null, note: null, transferred: false });
      }
      this.pendingSends.set(conversationId, pending);
      const conv = this.server.get(conversationId);
      if (conv && this.snapshot.phase === "ready") {
        const waiting = await this.updateAwaitingSetup(conv);
        if (!waiting && drafts.length) void this.flushDrafts(conversationId);
      }
    }
    if (named.title && this.server.get(conversationId)?.contextClass !== "DIRECT") this.applyGroupTitle(conversationId, named.title);
    this.set({ messages: { ...this.snapshot.messages, [conversationId]: this.withTicks(conversationId, [...views, ...pending]) }, processedSequences: { ...this.snapshot.processedSequences, [conversationId]: records.reduce((head, record) => Math.max(head, record.sequence), 0) } });
  }

  private stateFor(conversationId: string): ManagedConversationState {
    let st = this.managed.get(conversationId);
    if (!st) {
      st = new ManagedConversationState();
      this.managed.set(conversationId, st);
    }
    return st;
  }

  /**
   * MANAGED synchronization: the newest page on first load (older pages on demand), then
   * forward by sequence. Without live events, the newest loaded window is re-read so edits
   * and deletions show up (MSG-008); with them, events update messages in place.
   */
  private async syncManaged(conv: ServerConversation): Promise<void> {
    const id = conv.conversationId;
    const st = this.stateFor(id);
    const changed = new Set<string>();
    const add = (ids: Set<string>) => {
      for (const x of ids) changed.add(x);
    };
    if (!this.loadedManaged.has(id)) {
      const head = await relay<{ lastSequence: number; readSequence: number }>("GET", `v1/conversations/${id}/unread`).catch(() => null);
      if (head && head.lastSequence > 0) {
        const page = await relay<{ messages: ManagedServerMessage[]; nextBeforeSequence?: number | null }>(
          "GET",
          `v1/conversations/${id}/messages?beforeSequence=${head.lastSequence + 1}&limit=${INITIAL_PAGE}`,
        );
        add(st.applyOlderPage(page.messages, page.nextBeforeSequence));
        st.maxSequence = Math.max(st.maxSequence, head.lastSequence);
        this.lastReadSent.set(id, Math.max(this.lastReadSent.get(id) ?? 0, head.readSequence));
      }
      this.loadedManaged.add(id);
    } else if (!this.transport?.connected && st.size) {
      const from = Math.max((st.minSequence ?? 1) - 1, st.maxSequence - WINDOW);
      const page = await relay<{ messages: ManagedServerMessage[] }>("GET", `v1/conversations/${id}/messages?afterSequence=${from}&limit=${WINDOW}`);
      add(st.applyMessages(page.messages));
    }
    for (let i = 0; i < 20; i++) {
      const page = await relay<{ messages: ManagedServerMessage[] }>("GET", `v1/conversations/${id}/messages?afterSequence=${st.maxSequence}&limit=${PAGE}`);
      add(st.applyMessages(page.messages));
      if (page.messages.length < PAGE) break;
    }
    this.afterManagedChange(conv, { ...emptyRefresh(), messages: changed });
    if (changed.size) this.bumpConversation(id, st);
  }

  /** REST replay of MANAGED events after a gap / `sync.required` (and the forward sync). */
  private async replayManaged(conv: ServerConversation): Promise<void> {
    const id = conv.conversationId;
    const st = this.stateFor(id);
    let after = st.eventCursor ?? this.transport?.cursor(id) ?? 0;
    if (st.eventCursor === null) st.eventCursor = after;
    for (let i = 0; i < 50; i++) {
      const page = await relay<{ events: ConversationEvent[]; nextAfter: number | null; hasMore: boolean; syncRequired: boolean }>(
        "GET",
        `v1/conversations/${id}/events?after=${after}&limit=${PAGE}`,
      );
      if (page.syncRequired) {
        // The event window was pruned: re-read every loaded message (current representation).
        await this.resnapshotManaged(conv);
      }
      const refresh = st.applyEvents(page.events);
      this.afterManagedChange(conv, refresh);
      after = Math.max(after, page.nextAfter ?? after, st.eventCursor ?? 0);
      if (!page.hasMore || !page.events.length) break;
    }
    await this.syncManaged(conv);
    this.transport?.synced(id, after);
  }

  private async resnapshotManaged(conv: ServerConversation): Promise<void> {
    const id = conv.conversationId;
    const st = this.stateFor(id);
    if (st.minSequence === null) return;
    let from = st.minSequence - 1;
    for (let i = 0; i < 50 && from < st.maxSequence; i++) {
      const page = await relay<{ messages: ManagedServerMessage[] }>("GET", `v1/conversations/${id}/messages?afterSequence=${from}&limit=${PAGE}`);
      st.applyMessages(page.messages);
      if (page.messages.length < PAGE) break;
      from = page.messages[page.messages.length - 1].sequence;
    }
    await this.loadPins(id).catch(() => {});
  }

  private afterManagedChange(conv: ServerConversation, refresh: Refresh): void {
    const id = conv.conversationId;
    const st = this.stateFor(id);
    if (this.outbox && refresh.messages.size) {
      const ids: string[] = [];
      for (const mid of refresh.messages) {
        const r = st.get(mid);
        if (r) ids.push(r.clientMessageId);
      }
      this.outbox.confirm(id, ids);
    }
    if (refresh.reactions.size) void this.loadReactions(id, [...refresh.reactions]).catch(() => {});
    if (refresh.pins) void this.loadPins(id).catch(() => {});
    if (refresh.conversation) void this.refreshConversations().catch(() => {});
    this.renderManaged(id);
  }

  /** Moves the conversation's list preview forward from what was just synchronized. */
  private bumpConversation(conversationId: string, st: ManagedConversationState): void {
    const latest = st.ordered().at(-1);
    if (!latest) return;
    const conversations = this.snapshot.conversations.map((c) => {
      if (c.id !== conversationId || (c.lastMessage && c.lastMessage.sequence > latest.sequence)) return c;
      return {
        ...c,
        lastActivityAt: latest.createdAt > c.lastActivityAt ? latest.createdAt : c.lastActivityAt,
        lastMessage: { sequence: latest.sequence, senderPrincipalId: latest.senderPrincipalId, previewText: latest.text, attachmentCount: latest.attachmentIds.length, lifecycle: latest.lifecycle, createdAt: latest.createdAt },
      };
    });
    conversations.sort((a, b) => b.lastActivityAt.localeCompare(a.lastActivityAt) || b.createdAt.localeCompare(a.createdAt));
    this.set({ conversations });
  }

  private managedView(r: ManagedRecord, st: ManagedConversationState, me: string | null, reactions: Map<string, ReactionView[]> | undefined, pinned: Set<string>): MessageView {
    const aid = r.attachmentIds[0] ?? null;
    const meta = aid ? this.attachmentMeta.get(aid) : undefined;
    if (aid && !meta) void this.loadAttachmentMeta(st, r);
    const reply = r.replyToMessageId ? st.get(r.replyToMessageId) : undefined;
    const removed = r.lifecycle !== "ACTIVE";
    return {
      key: `m:${r.messageId}`,
      sequence: r.sequence,
      senderPrincipalId: r.senderPrincipalId,
      mine: r.senderPrincipalId === me,
      createdAt: r.createdAt,
      profile: "MANAGED",
      state: removed ? "removed" : "delivered",
      text: removed ? null : r.text,
      file: aid ? { name: meta?.name ?? "attachment", mediaType: meta?.mediaType ?? "application/octet-stream", size: meta?.size ?? null, attachmentId: aid, state: meta?.state } : null,
      note: removed ? (r.lifecycle === "HIDDEN_FOR_YOU" ? "Hidden for you" : r.lifecycle === "MODERATED" ? "Removed by a moderator" : "Message deleted") : null,
      transferred: false,
      messageId: r.messageId,
      outboxKey: null,
      editCount: r.editCount,
      replyTo: r.replyToMessageId
        ? { messageId: r.replyToMessageId, senderPrincipalId: reply?.senderPrincipalId ?? null, text: reply && reply.lifecycle === "ACTIVE" ? reply.text : null, removed: !!reply && reply.lifecycle !== "ACTIVE" }
        : null,
      reactions: reactions?.get(r.messageId) ?? [],
      pinned: pinned.has(r.messageId),
      seenBy: [],
      announcement: r.announcement,
    };
  }

  private renderManaged(conversationId: string): void {
    const st = this.managed.get(conversationId);
    if (!st) return;
    const me = this.snapshot.identity?.principalId ?? null;
    const reactions = this.reactions.get(conversationId);
    const pinned = new Set((this.snapshot.pins[conversationId] ?? []).map((p) => p.messageId));
    const views = st.ordered().map((r) => this.managedView(r, st, me, reactions, pinned));
    // Receipts: who has read up to your latest message.
    const lastMine = [...views].reverse().find((v) => v.mine && v.state === "delivered");
    if (lastMine?.sequence != null) lastMine.seenBy = st.seenBy(lastMine.sequence, me);
    const queued: MessageView[] = (this.outbox?.all(conversationId) ?? [])
      .filter((item) => !st.byClientMessageId(item.clientMessageId))
      .map((item: OutboxItem) => ({
        key: `q:${item.key}`,
        sequence: null,
        senderPrincipalId: me,
        mine: true,
        createdAt: item.createdAt,
        profile: "MANAGED",
        state: item.state === "failed" ? "failed" : item.state === "sending" ? "sending" : "queued",
        text: item.text,
        file: null,
        note: item.state === "failed" ? `Not sent (${item.error ?? "error"})` : item.error ? "Waiting to send — will retry" : null,
        transferred: false,
        messageId: null,
        outboxKey: item.key,
        replyTo: item.replyToMessageId ? { messageId: item.replyToMessageId, senderPrincipalId: st.get(item.replyToMessageId)?.senderPrincipalId ?? null, text: st.get(item.replyToMessageId)?.text ?? null, removed: false } : null,
      }));
    const pending = this.pendingSends.get(conversationId) ?? [];
    this.set({
      messages: { ...this.snapshot.messages, [conversationId]: this.withTicks(conversationId, [...views, ...queued, ...pending]) },
      hasOlder: { ...this.snapshot.hasOlder, [conversationId]: st.hasOlder },
    });
  }

  private async loadAttachmentMeta(st: ManagedConversationState, r: ManagedRecord): Promise<void> {
    const aid = r.attachmentIds[0];
    const conversationId = [...this.managed.entries()].find(([, v]) => v === st)?.[0];
    if (!aid || !conversationId || this.attachmentLoading.has(aid)) return;
    this.attachmentLoading.add(aid);
    try {
      const a = await relay<{ fileName: string | null; mediaType: string; size: number; state: string; kind: string }>("GET", `v1/conversations/${conversationId}/attachments/${aid}`);
      this.attachmentMeta.set(aid, { name: a.fileName ?? r.text ?? "attachment", mediaType: a.mediaType, size: a.size, state: a.state, kind: a.kind });
      this.renderManaged(conversationId);
    } catch {
      // Shown with a generic name; download still goes through a fresh grant.
    } finally {
      this.attachmentLoading.delete(aid);
    }
  }

  /** Back-pagination of MANAGED history (`beforeSequence`). */
  async loadOlder(conversationId: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (conv?.securityProfile === "PRIVATE_E2EE") {
      const existing = this.privatePaging.get(conversationId);
      if (existing) return existing;
      const generation = this.generation;
      const job = (async () => {
        const window = this.privateWindows.get(conversationId) ?? await this.readPrivatePage(conversationId);
        const before = window?.cursor;
        if (before === null || before === undefined || this.stopped || generation !== this.generation) return;
        const loaded = await this.readPrivatePage(conversationId, before);
        if (!loaded || this.stopped || generation !== this.generation) return;
        await this.refreshE2eeMeta(conversationId);
        await this.renderE2ee(conversationId);
      })().finally(() => { if (this.privatePaging.get(conversationId) === job) this.privatePaging.delete(conversationId); });
      this.privatePaging.set(conversationId, job);
      return job;
    }
    const st = this.managed.get(conversationId);
    if (!conv || conv.securityProfile !== "MANAGED" || !st?.hasOlder || st.minSequence === null) return;
    const page = await relay<{ messages: ManagedServerMessage[]; nextBeforeSequence?: number | null }>(
      "GET",
      `v1/conversations/${conversationId}/messages?beforeSequence=${st.minSequence}&limit=${PAGE}`,
    );
    st.applyOlderPage(page.messages, page.nextBeforeSequence);
    this.renderManaged(conversationId);
  }

  /** Opening a conversation: sync, pins and the newest messages' reactions (MANAGED). */
  async openConversation(conversationId: string): Promise<void> {
    await this.sync(conversationId);
    const conv = this.server.get(conversationId);
    if (conv?.securityProfile === "PRIVATE_E2EE") {
      await this.refreshE2eeMeta(conversationId);
      const ids = (this.snapshot.messages[conversationId] ?? []).map((v) => v.messageId).filter((x): x is string => !!x).slice(-30);
      await Promise.all([this.loadPins(conversationId).catch(() => {}), this.loadReactions(conversationId, ids).catch(() => {})]);
      await this.renderE2ee(conversationId);
      return;
    }
    if (conv?.securityProfile !== "MANAGED") return;
    const st = this.stateFor(conversationId);
    await Promise.all([
      this.loadPins(conversationId).catch(() => {}),
      this.loadReactions(conversationId, st.ordered().filter((r) => r.lifecycle === "ACTIVE").slice(-30).map((r) => r.messageId)).catch(() => {}),
    ]);
  }

  async loadReactions(conversationId: string, messageIds: string[]): Promise<void> {
    if (this.server.get(conversationId)?.securityProfile === "PRIVATE_E2EE") { await this.renderE2ee(conversationId); return; }
    const map = this.reactions.get(conversationId) ?? new Map<string, ReactionView[]>();
    this.reactions.set(conversationId, map);
    const queue = [...new Set(messageIds)];
    const worker = async () => {
      for (let id = queue.shift(); id; id = queue.shift()) {
        const r = await relay<{ reactions: ReactionView[] }>("GET", `v1/conversations/${conversationId}/messages/${id}/reactions`).catch(() => null);
        if (r) map.set(id, r.reactions);
      }
    };
    await Promise.all([worker(), worker(), worker(), worker()]);
    await this.rerender(conversationId);
  }

  private rerender(conversationId: string): Promise<void> {
    if (this.server.get(conversationId)?.securityProfile === "PRIVATE_E2EE") return this.renderE2ee(conversationId);
    this.renderManaged(conversationId);
    return Promise.resolve();
  }

  private metaFor(conversationId: string): Map<string, E2eeMeta> {
    let m = this.e2eeMeta.get(conversationId);
    if (!m) this.e2eeMeta.set(conversationId, (m = new Map()));
    return m;
  }

  private noteMeta(conversationId: string, messages: ServerMessage[]): void {
    const m = this.metaFor(conversationId);
    for (const x of messages) m.set(x.messageId, { lifecycle: x.lifecycle, replyTo: x.replyToMessageId ?? null, target: x.targetMessageId ?? null });
  }

  /** Reconcile only the locally loaded window, in bounded authoritative pages. */
  private async refreshE2eeMeta(conversationId: string, messageId?: string): Promise<void> {
    const crypto = this.crypto;
    if (!crypto || !this.snapshot.deviceId) return;
    const generation = this.generation;
    const window = this.privateWindows.get(conversationId) ?? await this.readPrivatePage(conversationId);
    if (!window || this.stopped || crypto !== this.crypto || generation !== this.generation) return;
    const records = window.ordered;
    if (!records.length) return;
    const ids = new Set(records.flatMap(record => record.messageId ? [record.messageId] : []));
    const reconcile = async (messages: ServerMessage[]) => {
      const loaded = messages.filter(message => ids.has(message.messageId));
      if (this.stopped || crypto !== this.crypto || generation !== this.generation) return;
      this.noteMeta(conversationId, loaded);
      this.privateWindows.get(conversationId)?.reconcile(loaded);
      if (loaded.length) await this.withMlsLock(() => crypto.call("reconcileHistoryMetadata", { conversationId, messages: loaded.map(message => ({ sequence: message.sequence, messageId: message.messageId, lifecycle: message.lifecycle, replyToMessageId: message.replyToMessageId, targetMessageId: message.targetMessageId })) }));
    };
    if (messageId) {
      if (!ids.has(messageId)) return;
      const result = await relay<{ message: ServerMessage }>("GET", `v1/conversations/${conversationId}/messages/${messageId}`);
      if (!result.message || result.message.messageId !== messageId) throw new MessagingApiError(0, "MESSAGE_METADATA_INVALID", null);
      await reconcile([result.message]);
      return;
    }
    const first = records.reduce((value, record) => Math.min(value, record.sequence), Number.MAX_SAFE_INTEGER);
    let before = records.reduce((value, record) => Math.max(value, record.sequence), 0) + 1;
    for (;;) {
      const page = await relay<{ messages: ServerMessage[]; nextBeforeSequence: number | null }>("GET", `v1/conversations/${conversationId}/messages?beforeSequence=${before}&limit=500`);
      await reconcile(page.messages);
      const oldest = page.messages.reduce((value, message) => Math.min(value, message.sequence), before);
      if (page.nextBeforeSequence === null || oldest <= first) break;
      if (!page.messages.length || page.nextBeforeSequence >= before) throw new MessagingApiError(0, "MESSAGE_METADATA_PAGE_INVALID", null);
      before = page.nextBeforeSequence;
    }
  }

  async loadPins(conversationId: string): Promise<void> {
    const r = await relay<{ pins: PinView[] }>("GET", `v1/conversations/${conversationId}/pins`);
    this.set({ pins: { ...this.snapshot.pins, [conversationId]: r.pins } });
    await this.rerender(conversationId);
  }

  async react(conversationId: string, messageId: string, reaction: string, add: boolean): Promise<void> {
    const conv = this.server.get(conversationId);
    if (conv?.securityProfile === "PRIVATE_E2EE") {
      const target = this.snapshot.messages[conversationId]?.find(message => message.messageId === messageId);
      if (!target || !["sent", "delivered"].includes(target.state)) throw new MessagingApiError(0, "MESSAGE_NOT_ACTIVE", null);
      if (add && !target.reactions?.some(item => item.reaction === reaction && item.reactedByMe) && (target.reactions?.filter(item => item.reactedByMe).length ?? 0) >= 20) throw new MessagingApiError(0, "REACTION_LIMIT_REACHED", null);
      await this.sendRich(conversationId, { t: "reaction", target: messageId, reaction, add });
      return;
    }
    await relay(add ? "PUT" : "DELETE", `v1/conversations/${conversationId}/messages/${messageId}/reactions/${reactionSegment(reaction)}`, add ? {} : undefined);
    await this.loadReactions(conversationId, [messageId]);
  }

  async pin(conversationId: string, messageId: string, pinned: boolean): Promise<void> {
    await relay(pinned ? "PUT" : "DELETE", `v1/conversations/${conversationId}/pins/${messageId}`, pinned ? {} : undefined);
    await this.loadPins(conversationId);
  }

  /** MANAGED edit (`PATCH`, compare-and-swap on the edit count); PRIVATE_E2EE sends an encrypted EDIT. */
  async editMessage(conversationId: string, messageId: string, text: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (conv?.securityProfile === "PRIVATE_E2EE") {
      await this.sendEncrypted(conv, { t: "text", text }, [], { kind: "EDIT", targetMessageId: messageId });
      await this.sync(conversationId);
      return;
    }
    const st = this.stateFor(conversationId);
    const current = st.get(messageId);
    const msg = await relay<ManagedServerMessage>("PATCH", `v1/conversations/${conversationId}/messages/${messageId}`, {
      text,
      ...(current ? { expectedEditCount: current.editCount } : {}),
    });
    st.applyMessages([msg]);
    this.renderManaged(conversationId);
  }

  /** Deletes for everyone (sender) or hides for you; the tombstone replaces the message. */
  async deleteMessage(conversationId: string, messageId: string, scope: "EVERYONE" | "ME"): Promise<void> {
    await relay("DELETE", `v1/conversations/${conversationId}/messages/${messageId}?scope=${scope}`);
    if (this.server.get(conversationId)?.securityProfile === "PRIVATE_E2EE") {
      this.metaFor(conversationId).set(messageId, { ...(this.metaFor(conversationId).get(messageId) ?? { replyTo: null, target: null }), lifecycle: scope === "ME" ? "HIDDEN_FOR_YOU" : "DELETED" });
      await this.renderE2ee(conversationId);
      return;
    }
    const st = this.stateFor(conversationId);
    const r = await relay<{ message: ManagedServerMessage }>("GET", `v1/conversations/${conversationId}/messages/${messageId}`).catch(() => null);
    if (r?.message) st.applyMessages([r.message]);
    this.renderManaged(conversationId);
  }

  /** MANAGED server search; PRIVATE_E2EE content is searched on this device only. */
  async search(conversationId: string, q: string): Promise<SearchHit[]> {
    const conv = this.server.get(conversationId);
    const query = q.trim();
    if (!conv || !query) return [];
    if (conv.securityProfile === "PRIVATE_E2EE") {
      const needle = query.toLocaleLowerCase();
      return (this.snapshot.messages[conversationId] ?? [])
        .filter((m) => m.sequence !== null && m.text?.toLocaleLowerCase().includes(needle))
        .map((m) => ({ messageId: m.key, sequence: m.sequence!, senderPrincipalId: m.senderPrincipalId ?? "", createdAt: m.createdAt ?? "", text: m.text }));
    }
    const r = await relay<{ results: ManagedServerMessage[] }>("GET", `v1/conversations/${conversationId}/search?q=${encodeURIComponent(query.slice(0, 200))}&limit=25`);
    return r.results.map((m) => ({ messageId: m.messageId, sequence: m.sequence, senderPrincipalId: m.senderPrincipalId, createdAt: m.createdAt, text: m.managed?.text ?? null }));
  }

  /** Typing indicator over the realtime connection (throttled; ephemeral). */
  typing(conversationId: string, started: boolean): void {
    const t = this.transport;
    if (!t?.connected) return;
    const now = Date.now();
    if (started && now - (this.lastTypingSent.get(conversationId) ?? 0) < TYPING_THROTTLE_MS) return;
    this.lastTypingSent.set(conversationId, started ? now : 0);
    try {
      t.typing(conversationId, started);
    } catch {
      // Not subscribed (e.g. an E2EE conversation on a session device).
    }
  }

  private onTyping(conversationId: string, principalId: string, state: "started" | "stopped", ttlMs: number): void {
    if (principalId === this.snapshot.identity?.principalId) return;
    const key = `${conversationId}:${principalId}`;
    const existing = this.typingTimers.get(key);
    if (existing) clearTimeout(existing);
    this.typingTimers.delete(key);
    const current = new Set(this.snapshot.typing[conversationId] ?? []);
    if (state === "started") {
      current.add(principalId);
      this.typingTimers.set(key, setTimeout(() => this.onTyping(conversationId, principalId, "stopped", 0), Math.min(Math.max(ttlMs, 1000), 15_000)));
    } else {
      current.delete(principalId);
    }
    this.set({ typing: { ...this.snapshot.typing, [conversationId]: [...current] } });
  }

  async markRead(conversationId: string): Promise<void> {
    // Ask for everyone's latest read / delivered positions (ticks) whenever the chat is looked at,
    // not only when the live connection starts: a missed update can't leave ticks stuck.
    try { this.transport?.queryReceipts(conversationId); } catch { /* not connected: the next connect asks */ }
    const list = this.snapshot.messages[conversationId] ?? [];
    const visibleLast = [...list].reverse().find((m) => m.sequence !== null)?.sequence ?? 0;
    const last = Math.max(visibleLast, this.snapshot.processedSequences?.[conversationId] ?? 0);
    // Only a read position that moves is written (no duplicate receipt events, MSG-018).
    if (!last || last <= (this.lastReadSent.get(conversationId) ?? 0)) {
      this.clearUnread(conversationId);
      return;
    }
    this.lastReadSent.set(conversationId, last);
    await relay("PUT", `v1/conversations/${conversationId}/read-cursor`, { sequence: last }).catch(() => {
      this.lastReadSent.delete(conversationId);
    });
    this.clearUnread(conversationId);
  }

  private clearUnread(conversationId: string): void {
    const c = this.snapshot.conversations.find((x) => x.id === conversationId);
    if (c && (c.unreadCount || c.mentionCount)) {
      this.set({ conversations: this.snapshot.conversations.map((x) => (x.id === conversationId ? { ...x, unreadCount: 0, mentionCount: 0 } : x)) });
    }
  }

  /** Retries a failed MANAGED send with its original clientMessageId (no duplicate). */
  retrySend(outboxKey: string): void {
    this.outbox?.retry(outboxKey);
    void this.flush();
  }

  discardSend(outboxKey: string): void {
    this.outbox?.discard(outboxKey);
  }

  /** Sends every due MANAGED outbox item (serialized; offline: waits for `online`). */
  private flush(): Promise<number> {
    if (!this.outbox || (typeof navigator !== "undefined" && navigator.onLine === false)) return Promise.resolve(0);
    if (this.flushing) return this.flushing;
    const outbox = this.outbox;
    const run = flushOutbox(outbox, async (item) => {
      const msg = await relay<ManagedServerMessage>("POST", `v1/conversations/${item.conversationId}/messages`, {
        clientMessageId: item.clientMessageId,
        text: item.text,
        ...(item.replyToMessageId ? { replyToMessageId: item.replyToMessageId } : {}),
        ...(item.mentionedPrincipalIds.length ? { mentionedPrincipalIds: item.mentionedPrincipalIds } : {}),
      });
      const st = this.stateFor(item.conversationId);
      st.applyMessages([msg]);
      this.renderManaged(item.conversationId);
      this.bumpConversation(item.conversationId, st);
    }).finally(() => {
      if (this.flushing === run) this.flushing = null;
    });
    this.flushing = run;
    return run;
  }

  // ------------------------------------------------------------------- send

  private addPending(conversationId: string, view: MessageView): void {
    const list = [...(this.pendingSends.get(conversationId) ?? []), view];
    this.pendingSends.set(conversationId, list);
    const shown = (this.snapshot.messages[conversationId] ?? []).filter((m) => m.key !== view.key);
    this.set({ messages: { ...this.snapshot.messages, [conversationId]: this.withTicks(conversationId, [...shown, view]) } });
  }
  private settlePending(conversationId: string, key: string, failed: string | null): void {
    const list = (this.pendingSends.get(conversationId) ?? []).flatMap((m) => (m.key !== key ? [m] : failed ? [{ ...m, state: "failed" as const, note: failed }] : []));
    this.pendingSends.set(conversationId, list);
  }

  /**
   * Sends text. PRIVATE_E2EE: encrypted on this device, or refused — never in the clear.
   * MANAGED: queued in the durable outbox and sent with a stable clientMessageId.
   */
  async send(conversationId: string, text: string, options: { replyToMessageId?: string | null; mentionedPrincipalIds?: string[] } = {}): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv || !text.trim()) return;
    if (conv.securityProfile === "MANAGED" && this.outbox) {
      this.stateFor(conversationId);
      this.loadedManaged.add(conversationId);
      this.outbox.enqueue({
        key: uuid(),
        conversationId,
        clientMessageId: uuid(),
        text,
        replyToMessageId: options.replyToMessageId ?? null,
        mentionedPrincipalIds: options.mentionedPrincipalIds ?? [],
      });
      void this.flush();
      return;
    }
    const key = `p${uuid()}`;
    this.addPending(conversationId, { key, sequence: null, senderPrincipalId: this.snapshot.identity?.principalId ?? null, mine: true, createdAt: new Date().toISOString(), profile: conv.securityProfile, state: "sending", text, file: null, note: null, transferred: false });
    // This browser's private messaging is still being set up (silently): the message shows
    // as sending and goes out as soon as setup finishes.
    if (conv.securityProfile === "PRIVATE_E2EE" && this.snapshot.phase !== "ready" && MessagingEngine.settingUp(this.snapshot)) {
      this.beforeReady.push({ conversationId, key, text, options });
      return;
    }
    // Not in the chat's encryption group yet: join it by itself first (nobody else has to be online).
    let notInGroup = conv.securityProfile === "PRIVATE_E2EE" && !this.snapshot.conversations.find((c) => c.id === conversationId)?.groupReady;
    if (notInGroup && this.crypto && this.snapshot.phase === "ready") notInGroup = !(await this.joinBySelf(conv, true).catch(() => false));
    // The sender adds the others' newly registered devices before it encrypts, so the
    // message reaches every device they have (from the KeyPackages they published).
    if (conv.securityProfile === "PRIVATE_E2EE" && !notInGroup && this.crypto && this.snapshot.phase === "ready") await this.addMissingDevices(conv, true).catch(() => {
      this.sendDeviceChecks.delete(conversationId);
      this.set({ detail: "Some devices are waiting for synchronization. Device enrollment will retry." });
    });
    // Still not sendable (nobody else has a device yet, or the join must wait for a removal):
    // hold it sealed on this device and send it automatically once it can (WhatsApp).
    if (conv.securityProfile === "PRIVATE_E2EE" && this.crypto && this.snapshot.phase === "ready" && (notInGroup || (await this.updateAwaitingSetup(conv)))) {
      // Nobody else can decrypt yet: hold it sealed on this device, never report it sent.
      this.settlePending(conversationId, key, null);
      const clientMessageId = uuid();
      const createdAt = new Date().toISOString();
      await this.crypto.call("holdDraft", { conversationId, clientMessageId, text, createdAt });
      this.addPending(conversationId, { key: `d${clientMessageId}`, sequence: null, senderPrincipalId: this.snapshot.identity?.principalId ?? null, mine: true, createdAt, profile: conv.securityProfile, state: "pending-setup", text, file: null, note: null, transferred: false });
      this.deviceChecks.delete(conversationId);
      this.scheduleSync(conversationId);
      return;
    }
    return this.enqueue(conversationId, () => this.deliver(conv, conversationId, key, text, options.replyToMessageId ?? null, options.mentionedPrincipalIds ?? []));
  }

  /**
   * Whether this browser's private messaging is only being set up (no user action needed):
   * private messages written meanwhile are sent once it is ready.
   */
  static settingUp(s: Pick<EngineSnapshot, "phase" | "detail" | "identity">): boolean {
    return !!s.identity && (s.phase === "registering" || s.phase === "revoked" || (s.phase === "needs-device" && !s.detail));
  }

  /** Sends the private messages written while this browser was being set up. */
  private flushBeforeReady(): void {
    const queued = this.beforeReady.splice(0);
    if (!queued.length) return;
    const messages = { ...this.snapshot.messages };
    for (const q of queued) {
      this.settlePending(q.conversationId, q.key, null);
      messages[q.conversationId] = (messages[q.conversationId] ?? []).filter((m) => m.key !== q.key);
    }
    this.set({ messages });
    for (const q of queued) void this.send(q.conversationId, q.text, q.options).catch(() => {});
  }

  private async deliver(conv: ServerConversation, conversationId: string, key: string, text: string, replyToMessageId: string | null = null, mentionedPrincipalIds: string[] = []): Promise<void> {
    try {
      if (conv.securityProfile === "PRIVATE_E2EE") {
        // F7: the preview is built here from the link itself (nothing is fetched).
        const preview = linkPreviewFor(text);
        await this.sendEncrypted(conv, preview ? { t: "text", text, preview } : { t: "text", text }, [], { replyToMessageId, mentionedPrincipalIds });
      }
      else await relay("POST", `v1/conversations/${conversationId}/messages`, { clientMessageId: uuid(), text });
      this.settlePending(conversationId, key, null);
    } catch (e) {
      this.settlePending(conversationId, key, `Not sent (${sendFailureText(e)})`);
    }
    await this.sync(conversationId);
  }

  private async sendEncrypted(conv: ServerConversation, payload: Record<string, unknown>, attachmentIds: string[], extra: { replyToMessageId?: string | null; kind?: "EDIT"; targetMessageId?: string; mentionedPrincipalIds?: string[] } = {}): Promise<void> {
    const c = this.crypto;
    const deviceId = this.snapshot.deviceId;
    if (!c || !deviceId || !conv.encryption.mlsGroupId) throw new MessagingApiError(0, "DEVICE_REQUIRED", null);
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (!(await c.call<boolean>("hasGroup", { groupId: gid }))) throw new MessagingApiError(0, "NOT_IN_GROUP", null);
    for (let attempt = 0; attempt < 2; attempt++) {
      // Realtime keeps the group current, so send straight away. Only when the server says the
      // group moved on (409: new epoch, or a removal to commit) process everything and retry.
      if (attempt > 0) await this.sync(conv.conversationId);
      const clientMessageId = uuid();
      const out = await this.withMlsLock(() => c.call<{ ciphertext: Uint8Array; epoch: number }>("encrypt", { conversationId: conv.conversationId, groupId: gid, clientMessageId, payload }));
      try {
        await relay("POST", `v1/conversations/${conv.conversationId}/messages`, {
          clientMessageId,
          deviceId,
          epoch: out.epoch,
          ciphertext: b64u.enc(out.ciphertext),
          ...(attachmentIds.length ? { attachmentIds } : {}),
          ...(extra.replyToMessageId ? { replyToMessageId: extra.replyToMessageId } : {}),
          ...(extra.kind ? { kind: extra.kind, targetMessageId: extra.targetMessageId } : {}),
          // B3: ids only (deduplicated, at most 50); the mention text stays encrypted.
          ...(extra.mentionedPrincipalIds?.length && !extra.kind ? { mentionedPrincipalIds: [...new Set(extra.mentionedPrincipalIds)].slice(0, 50) } : {}),
        });
        this.scheduleSync(conv.conversationId);
        return;
      } catch (e) {
        const stale = e instanceof MessagingApiError && e.status === 409;
        if (!stale || attempt === 1) throw e;
      }
    }
  }

  /**
   * Sends a file: PRIVATE_E2EE encrypts it here (only ciphertext is uploaded). Voice notes pass
   * their recorded length; other audio and video files are measured here.
   */
  async sendFile(conversationId: string, file: File, durationMs?: number, caption?: string): Promise<void> {
    caption = caption?.trim() || undefined;
    const conv = this.server.get(conversationId);
    if (!conv) return;
    const key = `p${uuid()}`;
    this.addPending(conversationId, { key, sequence: null, senderPrincipalId: this.snapshot.identity?.principalId ?? null, mine: true, createdAt: new Date().toISOString(), profile: conv.securityProfile, state: "sending", text: caption ?? null, file: { name: file.name, mediaType: file.type || "application/octet-stream", size: file.size, attachmentId: null }, note: null, transferred: false });
    return this.enqueue(conversationId, () => this.deliverFile(conv, conversationId, key, file, durationMs, caption));
  }

  /** Sends a failed file again (it stays on this page until sent or discarded). */
  retryFile(conversationId: string, key: string): void {
    const failed = this.failedFiles.get(key);
    const conv = this.server.get(conversationId);
    if (!failed || !conv) return;
    this.failedFiles.delete(key);
    const view = (this.pendingSends.get(conversationId) ?? []).find((m) => m.key === key);
    if (view) this.addPending(conversationId, { ...view, state: "sending", note: null });
    void this.enqueue(conversationId, () => this.deliverFile(conv, conversationId, key, failed.file, failed.durationMs, failed.caption));
  }

  discardFile(conversationId: string, key: string): void {
    this.failedFiles.delete(key);
    this.pendingSends.set(conversationId, (this.pendingSends.get(conversationId) ?? []).filter((m) => m.key !== key));
    this.set({ messages: { ...this.snapshot.messages, [conversationId]: (this.snapshot.messages[conversationId] ?? []).filter((m) => m.key !== key) } });
  }

  /** A failed file that can be sent again. */
  canRetryFile(key: string): boolean {
    return this.failedFiles.has(key);
  }
  private failedFiles = new Map<string, { file: File; durationMs?: number; caption?: string }>();

  private async deliverFile(conv: ServerConversation, conversationId: string, key: string, file: File, durationMs?: number, caption?: string): Promise<void> {
    try {
      const data = new Uint8Array(await file.arrayBuffer());
      const mediaType = file.type || "application/octet-stream";
      if (conv.securityProfile === "PRIVATE_E2EE") {
        const enc = await this.crypto!.call<{ handle: string; ciphertext: Uint8Array; sha256: string }>("encryptFile", { data, mediaType, fileName: file.name });
        const intent = await relay<{ attachmentId: string; upload: { url: string; headers: { name: string; value: string }[] } }>("POST", `v1/conversations/${conversationId}/attachments`, {
          kind: attachmentKind(mediaType),
          size: enc.ciphertext.length,
          sha256: enc.sha256,
          deviceId: this.snapshot.deviceId,
          cryptoFormat: ATTACHMENT_FORMAT,
        });
        await this.upload(intent.upload, enc.ciphertext);
        await relay("POST", `v1/conversations/${conversationId}/attachments/${intent.attachmentId}/finalize`, {});
        await this.sendEncrypted(conv, { t: "file", handle: enc.handle, attachmentId: intent.attachmentId, ...(caption ? { caption } : {}) }, [intent.attachmentId]);
        rememberSentMedia(intent.attachmentId, file, mediaType);
      } else {
        const kind = attachmentKind(mediaType);
        // The server needs the length of voice notes and videos (and refuses them without it).
        const length = kind === "VOICE_NOTE" || kind === "VIDEO" ? (durationMs ?? (await mediaDurationMs(file))) : null;
        if ((kind === "VOICE_NOTE" || kind === "VIDEO") && !length) throw new MessagingApiError(0, "DURATION_UNKNOWN", null, "This browser can't read how long this file is.");
        const intent = await relay<{ attachmentId: string; upload: { url: string; headers: { name: string; value: string }[] } }>("POST", `v1/conversations/${conversationId}/attachments`, {
          kind,
          size: data.length,
          sha256: await sha256Hex(data),
          mediaType,
          fileName: file.name.slice(0, 255),
          ...(length ? { durationMs: length } : {}),
        });
        await this.uploadWithRetry(intent.upload, data);
        let fin = await relay<{ state: string; stateReason?: string | null }>("POST", `v1/conversations/${conversationId}/attachments/${intent.attachmentId}/finalize`, {});
        // The server scans MANAGED uploads: wait (bounded) while it verifies.
        for (let i = 0; i < 20 && (fin.state === "VERIFYING" || fin.state === "INTENT_CREATED"); i++) {
          await new Promise((r) => setTimeout(r, Math.min(500 * 2 ** i, 4000)));
          fin = await relay<{ state: string; stateReason?: string | null }>("GET", `v1/conversations/${conversationId}/attachments/${intent.attachmentId}`);
        }
        if (fin.state !== "READY") throw new MessagingApiError(0, `ATTACHMENT_${fin.state}`, fin.stateReason ?? null, fin.state === "QUARANTINED" || fin.state === "REJECTED" ? "The file was rejected by the security scan." : `The file is not ready (${fin.state}).`);
        this.attachmentMeta.set(intent.attachmentId, { name: file.name, mediaType, size: data.length, state: "READY", kind: attachmentKind(mediaType) });
        rememberSentMedia(intent.attachmentId, file, mediaType);
        const clientMessageId = uuid();
        const send = () => relay<ManagedServerMessage>("POST", `v1/conversations/${conversationId}/messages`, { clientMessageId, text: caption ?? file.name.slice(0, 255), attachmentIds: [intent.attachmentId] });
        // A lost response is retried with the same clientMessageId (answered as a replay).
        const msg = await send().catch((e) => (e instanceof MessagingApiError && (e.status === 0 || e.status >= 500) ? send() : Promise.reject(e)));
        this.stateFor(conversationId).applyMessages([msg]);
      }
      this.settlePending(conversationId, key, null);
    } catch (e) {
      this.failedFiles.set(key, { file, durationMs, caption });
      this.settlePending(conversationId, key, `Not sent (${sendFailureText(e)})`);
    }
    await this.sync(conversationId);
  }

  private async uploadWithRetry(target: { url: string; headers: { name: string; value: string }[] }, body: Uint8Array): Promise<void> {
    for (let attempt = 0; ; attempt++) {
      try {
        await this.upload(target, body);
        return;
      } catch (e) {
        const transient = e instanceof MessagingApiError && (e.code === "UPLOAD_UNREACHABLE" || e.status >= 500);
        if (!transient || attempt >= 2) throw e;
        await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
      }
    }
  }

  private async upload(target: { url: string; headers: { name: string; value: string }[] }, body: Uint8Array): Promise<void> {
    const headers = new Headers();
    for (const h of target.headers) if (h.name.toLowerCase() !== "content-length") headers.set(h.name, h.value);
    let r: Response;
    try {
      r = await fetch(target.url, { method: "PUT", headers, body: body as BodyInit, credentials: "omit", redirect: "error" });
    } catch (e) {
      throw new MessagingApiError(0, "UPLOAD_UNREACHABLE", e instanceof Error ? e.message : null);
    }
    if (!r.ok) throw new MessagingApiError(r.status, "UPLOAD_FAILED", null);
  }

  /** Downloads an attachment; PRIVATE_E2EE is decrypted on this device. */
  /**
   * A photo shown in the chat (WhatsApp): only plain raster types are rendered (never SVG,
   * which can carry script); everything else stays a download. Cached per attachment.
   */
  async imageUrl(conversationId: string, message: MessageView): Promise<string | null> {
    const aid = message.file?.attachmentId;
    if (!aid || !(isInlineImage(message.file?.mediaType) || isPlayableAudio(message.file?.mediaType))) return null;
    const cached = mediaUrls.get(aid);
    if (cached) return cached;
    const { blob } = await this.download(conversationId, message);
    const url = URL.createObjectURL(new Blob([blob], { type: message.file!.mediaType }));
    mediaUrls.set(aid, url);
    return url;
  }

  /** A photo or voice note already loaded on this visit, so it shows at once (no placeholder). */
  cachedMediaUrl(message: MessageView): string | null {
    return (message.file?.attachmentId && mediaUrls.get(message.file.attachmentId)) || null;
  }

  async download(conversationId: string, message: MessageView): Promise<{ blob: Blob; name: string }> {
    const conv = this.server.get(conversationId);
    const aid = message.file?.attachmentId;
    if (!conv || !aid) throw new MessagingApiError(0, "NO_ATTACHMENT", null);
    const grant = await relay<{ url: string }>("POST", `v1/conversations/${conversationId}/attachments/${aid}/download-grants`, {});
    const r = await fetch(grant.url, { credentials: "omit", redirect: "error" });
    if (!r.ok) throw new MessagingApiError(r.status, "DOWNLOAD_FAILED", null);
    const bytes = new Uint8Array(await r.arrayBuffer());
    if (conv.securityProfile === "PRIVATE_E2EE") {
      const f = await this.crypto!.call<{ data: Uint8Array; mediaType: string; fileName: string }>("decryptAttachment", { conversationId, sequence: message.sequence, ciphertext: bytes });
      // Saved, never rendered by the browser as a document of this origin.
      return { blob: new Blob([f.data as BlobPart], { type: "application/octet-stream" }), name: f.fileName };
    }
    return { blob: new Blob([bytes as BlobPart], { type: "application/octet-stream" }), name: message.file?.name ?? "attachment" };
  }

  // ------------------------------------------------------ history transfer

  private transferContext(transferId: string, source: string, target: string): string[] {
    const i = this.snapshot.identity!;
    return [transferId, i.tenantId, i.applicationId, i.environmentId, i.principalId, source, target];
  }

  /** Other ACTIVE devices of this account (possible history sources). */
  async otherDevices(): Promise<ServerDevice[]> {
    return (await this.ownDevices()).filter((d) => d.status === "ACTIVE" && d.deviceId !== this.snapshot.deviceId);
  }

  private historyV2Ids = new Set<string>();
  private historyRecoveryBusy = false;
  private historyDiscoveryAt = 0;
  private historySourceIndex = 0;

  private async supportsHistoryV2(): Promise<boolean> {
    const caps = await relay<{ historyTransferVersions: number[] }>("GET", "v1/messaging/history-transfer-capabilities").catch(() => null);
    return caps?.historyTransferVersions.includes(2) ?? false;
  }

  /** Authoritative visibility and tombstones are reconciled before any plaintext copy. */
  private async historyVisibility(): Promise<{ conversationId: string; firstSequence: number; lastSequence: number }[]> {
    const windows = [];
    for (const [conversationId, conv] of this.server) {
      if (conv.securityProfile !== "PRIVATE_E2EE") continue;
      let after = 0, firstSequence = 1, lastSequence = 0;
      for (;;) {
        const page = await relay<{ messages: ServerMessage[]; visibleFromSequence?: number; nextAfterSequence: number | null }>("GET", `v1/conversations/${conversationId}/messages?afterSequence=${after}&limit=500`);
        if (page.visibleFromSequence === undefined) throw new MessagingApiError(0, "HISTORY_VISIBILITY_REQUIRED", null);
        firstSequence = page.visibleFromSequence;
        this.notePrivateVisibility(conversationId, firstSequence);
        this.noteMeta(conversationId, page.messages);
        await this.crypto!.call("reconcileHistoryMetadata", { conversationId, messages: page.messages.map(m => ({ sequence: m.sequence, messageId: m.messageId, lifecycle: m.lifecycle, replyToMessageId: m.replyToMessageId, targetMessageId: m.targetMessageId })) });
        for (const m of page.messages) lastSequence = Math.max(lastSequence, m.sequence);
        if (page.nextAfterSequence == null) break;
        if (page.nextAfterSequence <= after) throw new MessagingApiError(0, "HISTORY_PAGE_INVALID", null);
        after = page.nextAfterSequence;
      }
      windows.push({ conversationId, firstSequence, lastSequence: Math.max(firstSequence - 1, lastSequence) });
    }
    return windows;
  }

  private async uploadHistoryV2(transferId: string): Promise<void> {
    const t = await relay<{ status: string; sourceDeviceId: string; targetDeviceId: string; transferPublicKey: string; requestSignature: string; expiresAt: string }>("GET", `v1/messaging/history-transfers-v2/${transferId}`);
    if (!["REQUESTED", "SEALED"].includes(t.status)) {
      if (["CANCELLED", "COMPLETED", "EXPIRED"].includes(t.status)) await this.crypto!.call("forgetHistoryTransferV2", { transferId });
      return;
    }
    const context = this.transferContext(transferId, t.sourceDeviceId, t.targetDeviceId);
    const target = (await this.ownDevices()).find(d => d.deviceId === t.targetDeviceId && d.status === "ACTIVE");
    if (!target || t.sourceDeviceId !== this.snapshot.deviceId) throw new MessagingApiError(0, "DEVICE_NOT_ACTIVE", null);
    await this.verifyTransparency(target.deviceId, b64u.dec(target.signaturePublicKey));
    if (!(await this.crypto!.call<boolean>("verifyTransferRequestV2", { targetKey: b64u.dec(target.signaturePublicKey), context, transferKey: b64u.dec(t.transferPublicKey), expiresAt: Math.floor(Date.parse(t.expiresAt) / 1000), signature: b64u.dec(t.requestSignature) }))) throw new MessagingApiError(0, "TRANSFER_REQUEST_REFUSED", null);
    const snapshot = await this.crypto!.call<{ descriptor: { snapshotId: string; expiresAt: number; manifestDigest: string; chunkCount: number; totalPlaintextBytes: number }; coverage: string }>("prepareHistoryV2", { context, transferKey: b64u.dec(t.transferPublicKey), expiresAt: Math.floor(Date.parse(t.expiresAt) / 1000), eligibleConversations: await this.historyVisibility() });
    const manifest = await this.crypto!.call<Uint8Array>("historyObjectV2", { transferId, index: 0xffffffff });
    const digest = Array.from(b64u.dec(snapshot.descriptor.manifestDigest), b => b.toString(16).padStart(2, "0")).join("");
    await relay("PUT", `v1/messaging/history-transfers-v2/${transferId}/manifest`, { snapshotId: snapshot.descriptor.snapshotId, manifestDigest: digest, chunkCount: snapshot.descriptor.chunkCount, totalPlaintextBytes: snapshot.descriptor.totalPlaintextBytes, sealedManifest: b64u.enc(manifest) });
    for (let index = 0; index < snapshot.descriptor.chunkCount; index++) {
      if (this.stopped || !this.crypto) return;
      const sealed = await this.crypto.call<Uint8Array>("historyObjectV2", { transferId, index });
      await relay("PUT", `v1/messaging/history-transfers-v2/${transferId}/chunks/${index}`, { sealedChunk: b64u.enc(sealed) });
    }
    if (snapshot.coverage === "PARTIAL") this.set({ detail: "Available history was shared, but this device does not have a complete history copy." });
  }

  private async receiveHistoryV2(transferId: string, timeoutMs: number): Promise<number> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (this.stopped || !this.crypto) throw new MessagingApiError(0, "HISTORY_LOCKED", null);
      const t = await relay<{ status: string; sourceDeviceId: string; targetDeviceId: string; snapshotId: string; expiresAt: string; manifestDigest: string; chunkCount: number; totalPlaintextBytes: number }>("GET", `v1/messaging/history-transfers-v2/${transferId}`);
      const progress = await this.crypto.call<{ nextIndex: number; records: number; coverage: string | null }>("historyProgressV2", { transferId });
      if (t.status === "COMPLETED" && progress.nextIndex === t.chunkCount) { await this.crypto.call("forgetHistoryTransferV2", { transferId }); return progress.records; }
      if (["CANCELLED", "EXPIRED"].includes(t.status)) {
        await this.crypto.call("forgetHistoryTransferV2", { transferId });
        throw new MessagingApiError(0, `TRANSFER_${t.status}`, null);
      }
      if (t.status === "SEALED") {
        const devices = await this.ownDevices();
        const source = devices.find(d => d.deviceId === t.sourceDeviceId && d.status === "ACTIVE");
        const target = devices.find(d => d.deviceId === t.targetDeviceId && d.status === "ACTIVE");
        if (!source || !target || target.deviceId !== this.snapshot.deviceId) throw new MessagingApiError(0, "DEVICE_NOT_ACTIVE", null);
        await this.verifyTransparency(source.deviceId, b64u.dec(source.signaturePublicKey));
        const manifest = await relay<{ sealedObject: string }>("GET", `v1/messaging/history-transfers-v2/${transferId}/manifest`);
        const manifestDigest = b64u.enc(Uint8Array.from(t.manifestDigest.match(/../g) ?? [], h => Number.parseInt(h, 16)));
        await this.crypto.call("acceptHistoryManifestV2", { context: this.transferContext(transferId, source.deviceId, target.deviceId), descriptor: { snapshotId: t.snapshotId, expiresAt: Math.floor(Date.parse(t.expiresAt) / 1000), manifestDigest, chunkCount: t.chunkCount, totalPlaintextBytes: t.totalPlaintextBytes }, sourceKey: b64u.dec(source.signaturePublicKey), sealed: b64u.dec(manifest.sealedObject) });
        const eligibleConversations = await this.historyVisibility();
        let missing = false;
        for (let index = progress.nextIndex; index < t.chunkCount; index++) {
          const object = await relay<{ sealedObject: string }>("GET", `v1/messaging/history-transfers-v2/${transferId}/chunks/${index}`).catch(e => { if (e instanceof MessagingApiError && e.status === 404) return null; throw e; });
          if (!object) { missing = true; break; }
          const received = await this.crypto.call<{ nextIndex: number; records: number; signature: Uint8Array; coverage: string }>("importHistoryChunkV2", { transferId, index, sealed: b64u.dec(object.sealedObject), eligibleConversations });
          await relay("POST", `v1/messaging/history-transfers-v2/${transferId}/checkpoint`, { nextIndex: received.nextIndex, signature: b64u.enc(received.signature) });
        }
        if (!missing) {
          // A lost network receipt is repaired from the worker's durable checkpoint.
          const done = await this.crypto.call<{ nextIndex: number; records: number; signature: Uint8Array; coverage: string }>("historyProgressV2", { transferId });
          await relay("POST", `v1/messaging/history-transfers-v2/${transferId}/checkpoint`, { nextIndex: done.nextIndex, signature: b64u.enc(done.signature) });
          await this.historyVisibility();
          this.privateWindows.clear();
          for (const id of this.server.keys()) { await this.renderOrSync(id); await this.loadReactions(id, (this.snapshot.messages[id] ?? []).flatMap(m => m.messageId ? [m.messageId] : [])); }
          await relay("POST", `v1/messaging/history-transfers-v2/${transferId}/complete`, {});
          await this.crypto.call("forgetHistoryTransferV2", { transferId });
          if (done.coverage === "PARTIAL") this.set({ detail: "Available history recovered. The source reported incomplete history coverage." });
          return done.records;
        }
      }
      if (Date.now() >= deadline) throw new MessagingApiError(0, "TRANSFER_TIMEOUT", null);
      await new Promise(resolve => setTimeout(resolve, 1000));
    }
  }

  /** Automatically resumes already approved transfers; first approval remains explicit. */
  private async recoverHistoryInBackground(): Promise<void> {
    if (this.historyRecoveryBusy || this.snapshot.phase !== "ready" || !this.crypto || !(await this.supportsHistoryV2())) return;
    this.historyRecoveryBusy = true;
    try {
      const transfers = await this.crypto.call<{ transferId: string; role: string }[]>("listHistoryTransfersV2");
      for (const t of transfers) {
        this.historyV2Ids.add(t.transferId);
        if (t.role === "source") await this.uploadHistoryV2(t.transferId);
        else await this.receiveHistoryV2(t.transferId, 1).catch(e => { if (!(e instanceof MessagingApiError && e.code === "TRANSFER_TIMEOUT")) throw e; });
      }
      const unreadable = Object.values(this.snapshot.messages).some(messages => messages.some(m => m.state === "unreadable" || m.state === "before-join")) || await this.crypto.call<boolean>("historyRecoveryNeeded");
      if (unreadable && !transfers.some(t => t.role === "target") && Date.now() > this.historyDiscoveryAt) {
        this.historyDiscoveryAt = Date.now() + 60_000;
        const sources = await this.otherDevices();
        const source = sources.length ? sources[this.historySourceIndex++ % sources.length] : undefined;
        if (source) { const request = await this.requestHistory(source.deviceId); this.set({ detail: `History synchronization requested. Compare ${request.comparisonCode} on your existing device to authorize recovery.` }); }
      }
    } finally { this.historyRecoveryBusy = false; }
  }

  /** TARGET: asks `sourceDeviceId` for its history. Returns the comparison code to show. */
  async requestHistory(sourceDeviceId: string): Promise<{ transferId: string; comparisonCode: string }> {
    const c = this.crypto!;
    const target = this.snapshot.deviceId!;
    const devices = await this.ownDevices();
    const source = devices.find((d) => d.deviceId === sourceDeviceId && d.status === "ACTIVE");
    const me = devices.find((d) => d.deviceId === target);
    if (!source || !me) throw new MessagingApiError(0, "DEVICE_NOT_ACTIVE", null);
    const transferId = uuid();
    const ctx = this.transferContext(transferId, sourceDeviceId, target);
    const v2 = await this.supportsHistoryV2();
    const expires = Math.floor(Date.now() / 1000) + 10 * 60;
    const transferKey = await c.call<Uint8Array>(v2 ? "beginTransferV2" : "beginTransfer", { context: ctx, expiresAt: expires });
    const signature = await c.call<Uint8Array>(v2 ? "signTransferRequestV2" : "signTransferRequest", { context: ctx, expiresAt: expires });
    if (v2) this.historyV2Ids.add(transferId);
    await relay("POST", v2 ? "v1/messaging/history-transfers-v2" : "v1/messaging/history-transfers", {
      transferId,
      sourceDeviceId,
      targetDeviceId: target,
      transferPublicKey: b64u.enc(transferKey),
      expiresAt: new Date(expires * 1000).toISOString().replace(/\.\d{3}Z$/, "Z"),
      requestSignature: b64u.enc(signature),
    });
    const comparisonCode = await c.call<string>("comparisonCode", { context: ctx, transferKey, sourceKey: b64u.dec(source.signaturePublicKey), targetKey: b64u.dec(me.signaturePublicKey) });
    return { transferId, comparisonCode };
  }

  /** TARGET: waits for the sealed history, verifies the source and stores it. */
  async receiveHistory(transferId: string, timeoutMs = 120_000): Promise<number> {
    if (this.historyV2Ids.has(transferId)) return this.receiveHistoryV2(transferId, timeoutMs);
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const t = await relay<{ status: string; sourceDeviceId: string }>("GET", `v1/messaging/history-transfers/${transferId}`);
      if (t.status === "SEALED") {
        const sealed = await relay<{ sealedHistory: string; sourceDeviceId: string }>("GET", `v1/messaging/history-transfers/${transferId}/sealed-history`);
        const source = (await this.ownDevices()).find((d) => d.deviceId === sealed.sourceDeviceId);
        if (!source) throw new MessagingApiError(0, "SOURCE_DEVICE_UNKNOWN", null);
        const ctx = this.transferContext(transferId, sealed.sourceDeviceId, this.snapshot.deviceId!);
        const r = await this.crypto!.call<{ records: number }>("openHistory", { sourceKey: b64u.dec(source.signaturePublicKey), context: ctx, sealed: b64u.dec(sealed.sealedHistory) });
        await relay("POST", `v1/messaging/history-transfers/${transferId}/complete`, {});
        this.privateWindows.clear();
        for (const id of this.server.keys()) await this.renderOrSync(id);
        return r.records;
      }
      if (["CANCELLED", "EXPIRED", "COMPLETED"].includes(t.status)) throw new MessagingApiError(0, `TRANSFER_${t.status}`, null);
      if (Date.now() > deadline) throw new MessagingApiError(0, "TRANSFER_TIMEOUT", null);
      await new Promise((r) => setTimeout(r, 1500));
    }
  }

  private async renderOrSync(id: string): Promise<void> {
    const conv = this.server.get(id);
    if (conv?.securityProfile === "PRIVATE_E2EE") await this.renderE2ee(id);
  }

  // ------------------------------------------------- account device (ADR 0060, ADR 0067)

  /**
   * Declares this browser's account device for the current sign-in. Signing in is enough
   * (ADR 0067): the Account authority trusts a browser declared from a fresh sign-in, so
   * there is no verification step. Anything else is retried quietly.
   */
  private async ensureTrustedAccountDevice(label: string): Promise<boolean> {
    const token = await accountSession.getAccessToken();
    if (!token) throw new MessagingApiError(401, "AUTHENTICATION_REQUIRED", null);
    let view;
    try {
      view = await accountDevice.declare(token, label);
    } catch (e) {
      const code = accountErrorCode(e);
      if (code === "DEVICE_DECLARATION_REFUSED") {
        // This sign-in predates the device flow (or this browser's key was reset): only a
        // fresh sign-in can register the browser. The page hands over to the sign-in flow.
        this.set({ phase: "needs-signin", detail: "For your security, sign in again to finish setting up private messaging on this browser." });
        return false;
      }
      if (code === "WEB_DEVICE_LIMIT_REACHED") {
        // Older servers only; the Account authority now makes room on its own.
        this.set({ phase: "needs-device", detail: "Your account is signed in on too many browsers. Sign out of one you no longer use, then reload this page." });
        return false;
      }
      throw e;
    }
    if (!view) {
      this.set({ phase: "needs-device", detail: "This browser can't keep a secure key for private messages. Use an up-to-date browser." });
      return false;
    }
    if (view.trust === "TRUSTED" && view.state === "ACTIVE") return true;
    // Not trusted yet (an older server, or a sign-in being replaced): try again shortly,
    // silently. Never a verification step on the web.
    this.nextEnrollAt = Date.now() + this.enrollBackoff;
    this.enrollBackoff = Math.min(this.enrollBackoff * 2, 5 * 60_000);
    this.set({ phase: "needs-device", detail: null });
    return false;
  }

  /** TRUSTED browser: new devices of the account waiting for approval. */
  async checkIncomingLinks(): Promise<void> {
    if (this.snapshot.phase !== "ready") return;
    try {
      const links = await accountDevice.pendingLinks();
      this.set({
        incomingLinks: links.map((l: LinkView) => ({ linkId: l.linkId, platform: l.platform ?? "WEB", label: l.label ?? null, expiresAt: l.expiresAt, keyThumbprint: l.keyThumbprint ?? "" })),
      });
    } catch {
      // Untrusted or unclassified sign-ins cannot approve devices: nothing to show.
      if (this.snapshot.incomingLinks.length) this.set({ incomingLinks: [] });
    }
  }

  /** TRUSTED browser: the user typed the code the new device shows. */
  async approveLink(linkId: string, comparisonCode: string): Promise<void> {
    const link = this.snapshot.incomingLinks.find((l) => l.linkId === linkId);
    if (!link) return;
    await accountDevice.approveLink({ linkId, deviceId: "", status: "PENDING", expiresAt: link.expiresAt, keyThumbprint: link.keyThumbprint }, comparisonCode);
    this.set({ incomingLinks: this.snapshot.incomingLinks.filter((l) => l.linkId !== linkId) });
  }

  async rejectLink(linkId: string): Promise<void> {
    await accountDevice.rejectLink(linkId);
    this.set({ incomingLinks: this.snapshot.incomingLinks.filter((l) => l.linkId !== linkId) });
  }

  /** SOURCE: requests addressed to this device, each with the code the user compares. */
  async checkIncomingTransfers(): Promise<void> {
    const me = this.snapshot.deviceId;
    if (!me || !this.crypto || this.snapshot.phase !== "ready") return;
    const page = await relay<{ transfers: { transferId: string; sourceDeviceId: string; targetDeviceId: string; transferPublicKey: string; requestSignature: string; status: string; expiresAt: string }[] }>(
      "GET",
      `v1/messaging/history-transfers?deviceId=${me}`,
    );
    const v2 = await this.supportsHistoryV2();
    if (v2) {
      const next = await relay<{ transfers: typeof page.transfers }>("GET", `v1/messaging/history-transfers-v2?deviceId=${me}`);
      for (const t of next.transfers) { this.historyV2Ids.add(t.transferId); page.transfers.push(t); }
    }
    const devices = await this.ownDevices();
    const incoming: IncomingTransfer[] = [];
    for (const t of page.transfers ?? []) {
      if (t.sourceDeviceId !== me || t.status !== "REQUESTED") continue;
      const target = devices.find((d) => d.deviceId === t.targetDeviceId && d.status === "ACTIVE");
      const self = devices.find((d) => d.deviceId === me);
      if (!target || !self) continue;
      const ctx = this.transferContext(t.transferId, me, t.targetDeviceId);
      const expires = Math.floor(Date.parse(t.expiresAt) / 1000);
      const ok = await this.crypto.call<boolean>(this.historyV2Ids.has(t.transferId) ? "verifyTransferRequestV2" : "verifyTransferRequest", { targetKey: b64u.dec(target.signaturePublicKey), context: ctx, transferKey: b64u.dec(t.transferPublicKey), expiresAt: expires, signature: b64u.dec(t.requestSignature) });
      if (!ok) continue;
      const code = await this.crypto.call<string>("comparisonCode", { context: ctx, transferKey: b64u.dec(t.transferPublicKey), sourceKey: b64u.dec(self.signaturePublicKey), targetKey: b64u.dec(target.signaturePublicKey) });
      incoming.push({ transferId: t.transferId, targetDeviceId: t.targetDeviceId, comparisonCode: code, expiresAt: t.expiresAt });
    }
    this.set({ incomingTransfers: incoming });
  }

  /** SOURCE: the user confirmed the codes match — seal this device's history to the target. */
  async approveTransfer(transferId: string): Promise<void> {
    if (this.historyV2Ids.has(transferId)) { await this.uploadHistoryV2(transferId); this.set({ incomingTransfers: this.snapshot.incomingTransfers.filter(x => x.transferId !== transferId) }); return; }
    const me = this.snapshot.deviceId!;
    const t = await relay<{ transferId: string; targetDeviceId: string; transferPublicKey: string; status: string }>("GET", `v1/messaging/history-transfers/${transferId}`);
    if (t.status !== "REQUESTED") throw new MessagingApiError(0, `TRANSFER_${t.status}`, null);
    const ctx = this.transferContext(transferId, me, t.targetDeviceId);
    const r = await this.crypto!.call<{ sealed: Uint8Array }>("sealHistory", { context: ctx, transferKey: b64u.dec(t.transferPublicKey) });
    await relay("PUT", `v1/messaging/history-transfers/${transferId}/sealed-history`, { sealedHistory: b64u.enc(r.sealed) });
    this.set({ incomingTransfers: this.snapshot.incomingTransfers.filter((x) => x.transferId !== transferId) });
  }

  async declineTransfer(transferId: string): Promise<void> {
    await relay("POST", `v1/messaging/${this.historyV2Ids.has(transferId) ? "history-transfers-v2" : "history-transfers"}/${transferId}/cancel`, {});
    if (this.historyV2Ids.has(transferId)) await this.crypto!.call("forgetHistoryTransferV2", { transferId });
    this.set({ incomingTransfers: this.snapshot.incomingTransfers.filter((x) => x.transferId !== transferId) });
  }
}

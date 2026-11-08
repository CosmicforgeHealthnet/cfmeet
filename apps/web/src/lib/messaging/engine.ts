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
import type { Colleague, MessagingIdentity } from "@/types/messaging";
import { CryptoClient, CryptoWorkerError, type StoredRecord } from "./crypto-client";
import { ManagedConversationState, emptyRefresh, type ConversationEvent, type ManagedRecord, type Refresh, type ServerMessage as ManagedServerMessage } from "./managed-store";
import { MessageOutbox, browserOutboxStorage, flushOutbox, type OutboxItem } from "./outbox";
import { reactionSegment } from "./routes";

export type SecurityProfile = "MANAGED" | "PRIVATE_E2EE";
export type Phase = "loading" | "needs-device" | "registering" | "ready" | "locked" | "revoked" | "unavailable" | "error";
export type Connection = "connected" | "connecting" | "disconnected" | "auth-required" | "offline";
/** How live updates arrive: an E2EE device token, a MANAGED-only session device, or polling. */
export type RealtimeMode = "device" | "session" | "none";
export type ReactionView = { reaction: string; count: number; reactedByMe: boolean };
export type PinView = { messageId: string; sequence: number; pinnedByPrincipalId: string; pinnedAt: string; expiresAt: string | null };
export type MemberRole = "OWNER" | "ADMIN" | "MODERATOR" | "MEMBER" | "READ_ONLY" | "BOT_SERVICE" | string;
export type AttachmentMeta = { name: string; mediaType: string; size: number | null; state: string; kind: string };
export type SearchHit = { messageId: string; sequence: number; senderPrincipalId: string; createdAt: string; text: string | null };

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
  title: string;
  transparency: "VERIFIED" | "UNPINNED" | "NOT_APPLICABLE";
  version: number;
  ownerPrincipalId: string | null;
  /** The MANAGED conversation title set by a member (null: derived from the members). */
  serverTitle: string | null;
  avatarRef: string | null;
  unreadCount: number;
  mentionCount: number;
  lastActivityAt: string;
  lastMessage: { sequence: number; senderPrincipalId: string; previewText: string | null; attachmentCount: number; lifecycle: string; createdAt: string } | null;
  memberRoles: Record<string, MemberRole>;
  myRole: MemberRole | null;
};

export type MessageView = {
  key: string;
  sequence: number | null;
  senderPrincipalId: string | null;
  mine: boolean;
  createdAt: string | null;
  profile: SecurityProfile;
  state: "queued" | "sending" | "sent" | "delivered" | "failed" | "unreadable" | "removed" | "before-join" | "system";
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
  /** Members whose read receipt reached this message (only computed for your latest message). */
  seenBy?: string[];
  announcement?: boolean;
};

export type IncomingTransfer = {
  transferId: string;
  targetDeviceId: string;
  comparisonCode: string;
  expiresAt: string;
};

export type EngineSnapshot = {
  phase: Phase;
  detail: string | null;
  connection: Connection;
  identity: MessagingIdentity | null;
  deviceId: string | null;
  conversations: ConversationView[];
  messages: Record<string, MessageView[]>;
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
  encryption: { mlsGroupId: string | null; serverCanReadContent: boolean };
  members?: { principalId: string; role?: string; state?: string }[];
  version?: number;
  ownerPrincipalId?: string;
  title?: string | null;
  avatarRef?: string | null;
  inbox?: {
    lastActivityAt: string;
    lastMessage: { messageId: string; sequence: number; senderPrincipalId: string; createdAt: string; lifecycle: string; attachmentCount: number; previewText: string | null } | null;
    unreadCount: number;
    mentionCount: number;
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
};

type ServerDevice = { deviceId: string; status: string; signaturePublicKey: string; displayLabel: string | null };

const PAGE = 100;
/** Newest MANAGED messages loaded when a conversation is first synchronized. */
const INITIAL_PAGE = 50;
/** Newest MANAGED messages re-read per poll when no realtime events arrive (edits / tombstones). */
const WINDOW = 100;
const SESSION_DEVICE_KEY = "cfmeet.messaging.session-device.";
const TYPING_THROTTLE_MS = 3000;
const KEY_PACKAGE_STOCK = 10;
const POLL_MS = Math.max(1000, Number(process.env.NEXT_PUBLIC_MESSAGING_POLL_MS) || 15_000);
/** How often a member device checks an encrypted group for members' new devices. */
const DEVICE_CHECK_MS = Math.max(1000, Number(process.env.NEXT_PUBLIC_MESSAGING_DEVICE_CHECK_MS) || 30_000);
const MESSAGING_URL = process.env.NEXT_PUBLIC_MESSAGING_URL ?? "";
const KT_LOG_KEY = process.env.NEXT_PUBLIC_MESSAGING_KT_LOG_KEY ?? "";
const KT_LOG_ID = process.env.NEXT_PUBLIC_MESSAGING_KT_LOG_ID ?? "";
const ATTACHMENT_FORMAT = "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1";

const uuid = () => crypto.randomUUID();

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

export class MessagingEngine {
  static active: MessagingEngine | null = null;
  private snapshot: EngineSnapshot = {
    phase: "loading",
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
  private server = new Map<string, ServerConversation>();
  /** MANAGED message stores (current representation per message, MSG-008). */
  private managed = new Map<string, ManagedConversationState>();
  private loadedManaged = new Set<string>();
  private reactions = new Map<string, Map<string, ReactionView[]>>();
  private attachmentMeta = new Map<string, AttachmentMeta>();
  private attachmentLoading = new Set<string>();
  private outbox: MessageOutbox | null = null;
  private flushing: Promise<number> | null = null;
  private lastReadSent = new Map<string, number>();
  private lastTypingSent = new Map<string, number>();
  private typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private authTimer: ReturnType<typeof setTimeout> | null = null;
  /** Session devices are switched off (FEATURE_DISABLED): MANAGED falls back to polling. */
  private sessionDevicesUnavailable = false;
  private syncing = new Map<string, Promise<void>>();
  private resync = new Set<string>();
  private timers: ReturnType<typeof setInterval>[] = [];
  private reconnectDelay = 1000;
  private stopped = false;
  private pendingSends = new Map<string, MessageView[]>();
  private deviceChecks = new Map<string, number>();
  private lastWaitingTopUp = 0;
  private missingSince = new Map<string, number>();
  /** Sends of one conversation leave in the order the user wrote them. */
  private sendQueues = new Map<string, Promise<void>>();

  private enqueue(conversationId: string, job: () => Promise<void>): Promise<void> {
    const previous = this.sendQueues.get(conversationId) ?? Promise.resolve();
    const next = previous.then(job, job);
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
    this.snapshot = { ...this.snapshot, ...patch };
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
    MessagingEngine.active = this;
    if (typeof window !== "undefined") {
      window.addEventListener("offline", this.onOffline);
      window.addEventListener("online", this.onOnline);
    }
    try {
      const identity = await identityApi.me();
      this.set({ identity });
      this.outbox = new MessageOutbox(browserOutboxStorage(identity.principalId));
      this.outbox.subscribe(() => {
        for (const id of this.loadedManaged) this.renderManaged(id);
      });
      this.crypto = cryptoFactory ? cryptoFactory() : new CryptoClient();
      const opened = await this.crypto.call<{ status: string; deviceId?: string }>("open", {
        account: identity.principalId,
        scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId },
      });
      if (opened.status === "LOCKED") {
        this.set({ phase: "locked", detail: "Messaging is open in another tab of this browser." });
        return;
      }
      if (opened.status === "READY" && opened.deviceId) {
        const active = await this.deviceIsActive(opened.deviceId);
        if (!active) {
          await this.crypto.call("wipe", { account: identity.principalId });
          this.set({ phase: "revoked", detail: "This browser's messaging device was removed. Its local keys and history were erased." });
          await this.reopenCrypto();
          await this.refreshConversations();
          this.connect();
          this.timers.push(setInterval(() => void this.poll(), POLL_MS));
          return;
        }
        this.set({ phase: "ready", deviceId: opened.deviceId });
        await this.topUpKeyPackages();
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
    } catch (e) {
      this.fail(e);
    }
  }

  stop(): void {
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
    for (const t of this.typingTimers.values()) clearTimeout(t);
    this.typingTimers.clear();
    this.detachTransport();
    this.crypto?.terminate();
    this.crypto = null;
  }

  private async reopenCrypto(): Promise<void> {
    const identity = this.snapshot.identity!;
    this.crypto?.terminate();
    this.crypto = new CryptoClient();
    await this.crypto.call("open", {
      account: identity.principalId,
      scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId },
    });
  }

  private fail(e: unknown): void {
    const code = e instanceof MessagingApiError || e instanceof CryptoWorkerError ? e.code : "UNEXPECTED_ERROR";
    if (code === "FEATURE_DISABLED") {
      this.set({ phase: "unavailable", detail: "Messaging is not enabled for this CF Meet deployment." });
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
      // Realtime is the fast path; a periodic catch-up also lets member devices add new
      // devices and commit pending removals of encrypted groups.
      for (const id of this.server.keys()) this.scheduleSync(id);
      void this.flush();
      await this.topUpWhileWaiting();
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
    const r = await relay<{ devices: ServerDevice[] }>("GET", `v1/messaging/principals/${me}/devices`);
    return r.devices;
  }

  private async deviceIsActive(deviceId: string): Promise<boolean> {
    return (await this.ownDevices()).some((d) => d.deviceId === deviceId && d.status === "ACTIVE");
  }

  /** Creates this browser's device: keys generated in the worker, proof of possession. */
  async registerDevice(label = "CF Meet web browser"): Promise<void> {
    if (!this.crypto || !this.snapshot.identity) return;
    this.set({ phase: "registering", detail: null });
    try {
      const challenge = await identityApi.registrationChallenge();
      await this.crypto.call("generate", { deviceId: challenge.deviceId });
      const kp = await this.crypto.call<Uint8Array>("registrationKeyPackage", {
        challengeId: challenge.challengeId,
        nonce: fromHex(challenge.nonce),
      });
      const device = await identityApi.registerDevice(challenge.challengeId, b64u.enc(kp), label.slice(0, 64));
      await this.crypto.call("adopt");
      this.set({ phase: "ready", deviceId: device.deviceId });
      await this.topUpKeyPackages(KEY_PACKAGE_STOCK);
      this.connect();
      await this.refreshConversations();
      await this.checkWelcomes();
    } catch (e) {
      if (e instanceof MessagingApiError && (e.code === "E2EE_DEVICES_DISABLED" || e.reason === "E2EE_DEVICES_DISABLED")) {
        this.set({ phase: "unavailable", detail: "End-to-end encrypted messaging is not enabled in this environment. Managed conversations remain available." });
        return;
      }
      await this.reopenCrypto().catch(() => {});
      this.fail(e);
    }
  }

  /**
   * A device not yet in an encrypted group it belongs to depends on member devices
   * claiming its KeyPackages; one claimed by an attempt that did not complete is spent.
   * While waiting, publish one more per device-check period (the server caps the stock
   * per device).
   */
  private async topUpWhileWaiting(): Promise<void> {
    const c = this.crypto;
    if (!c || this.snapshot.phase !== "ready" || Date.now() - this.lastWaitingTopUp < DEVICE_CHECK_MS) return;
    for (const conv of this.server.values()) {
      if (conv.securityProfile !== "PRIVATE_E2EE" || !conv.encryption.mlsGroupId) continue;
      if (await c.call<boolean>("hasGroup", { groupId: b64u.dec(conv.encryption.mlsGroupId) })) continue;
      this.lastWaitingTopUp = Date.now();
      await this.topUpKeyPackages(1).catch(() => {});
      return;
    }
  }

  private async topUpKeyPackages(count = 0): Promise<void> {
    if (!this.crypto || !this.snapshot.deviceId || count < 1) return;
    const kps = await this.crypto.call<Uint8Array[]>("keyPackages", { count });
    await identityApi.publishKeyPackages(this.snapshot.deviceId, kps.map((k) => b64u.enc(k)));
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
    await this.crypto?.call("wipe", { account: this.snapshot.identity!.principalId });
    // A sign-out in progress erases everything itself: never reopen (a reopened worker
    // would create a fresh wrapping key after the sign-out's wipe).
    if (this.stopped) return;
    await this.reopenCrypto();
    this.set({ phase: "revoked", detail, deviceId: null, messages: {}, connection: "disconnected" });
    this.managed.clear();
    this.loadedManaged.clear();
    await this.refreshConversations();
    // MANAGED conversations continue on a session device.
    this.connect();
  }

  /** Logout: the local device and its history are erased and the device revoked. */
  async signOut(): Promise<void> {
    // Background work stops first: the revocation below must not be "detected" by a poll
    // that would wipe and reopen the device store in parallel.
    this.stopped = true;
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    this.detachTransport();
    try {
      if (this.snapshot.deviceId) await identityApi.revokeDevice(this.snapshot.deviceId);
    } catch {
      // The Meeting Service also revokes every Messaging session of the account at logout.
    }
    if (this.snapshot.identity) await this.crypto?.call("wipe", { account: this.snapshot.identity.principalId }).catch(() => {});
    this.stop();
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
        ? async () => messagingClientAccessToken(await identityApi.clientToken(deviceId), deviceId)
        : () => this.sessionAccessToken(),
      onEvent: (event) => this.onRealtimeEvent(event),
      onSyncRequired: (conversationId) => {
        void this.syncAuthoritative(conversationId);
      },
      onSubscribed: (conversationId, after) => {
        // MSG-018: the server resumed from this device's stored ack cursor.
        const st = this.managed.get(conversationId);
        if (st && st.eventCursor === null) st.eventCursor = after;
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
  private onRealtimeEvent(event: MessagingRealtimeEvent): void {
    const conv = this.server.get(event.conversationId);
    if (!conv) return;
    if (conv.securityProfile !== "MANAGED") {
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
    for (let i = 0; i < 10; i++) {
      const page: { conversations: ServerConversation[]; nextCursor: string | null } = await relay(
        "GET",
        `v1/conversations?limit=${PAGE}&orderBy=lastActivity${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`,
      );
      out.push(...page.conversations);
      cursor = page.nextCursor;
      if (!cursor) break;
    }
    // The list carries at most two members; groups are read in full.
    for (const c of out) {
      if (c.contextClass !== "DIRECT") {
        const m = await relay<{ members: { principalId: string; role?: string; state?: string }[] }>("GET", `v1/conversations/${c.conversationId}/members?limit=${PAGE}`).catch(() => null);
        if (m) c.members = m.members.filter((x) => !x.state || x.state === "ACTIVE").map((x) => ({ principalId: x.principalId, role: x.role, state: x.state }));
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
        members,
        title: "",
        transparency: c.securityProfile === "PRIVATE_E2EE" ? (this.snapshot.ktPinned ? "VERIFIED" : "UNPINNED") : "NOT_APPLICABLE",
        version: c.version ?? 0,
        ownerPrincipalId: c.ownerPrincipalId ?? null,
        serverTitle: c.title ?? null,
        avatarRef: c.avatarRef ?? null,
        unreadCount: c.inbox?.unreadCount ?? previous?.unreadCount ?? 0,
        mentionCount: c.inbox?.mentionCount ?? previous?.mentionCount ?? 0,
        lastActivityAt: c.inbox?.lastActivityAt ?? previous?.lastActivityAt ?? c.createdAt,
        lastMessage: last
          ? { sequence: last.sequence, senderPrincipalId: last.senderPrincipalId, previewText: last.previewText, attachmentCount: last.attachmentCount, lifecycle: last.lifecycle, createdAt: last.createdAt }
          : (previous?.lastMessage ?? null),
        memberRoles: roles,
        myRole: (me && roles[me]) || null,
      });
    }
    if (needNames.size) {
      try {
        const r = await identityApi.names([...needNames].slice(0, 256));
        const names = { ...this.snapshot.names };
        for (const p of r.principals) names[p.principalId] = p.displayName;
        this.set({ names });
      } catch {
        // Names are cosmetic; ids are shown instead.
      }
    }
    for (const v of views) {
      const others = v.members.filter((m) => m !== me);
      v.title = v.serverTitle?.trim()
        ? v.serverTitle.trim()
        : others.length ? others.map((m) => this.snapshot.names[m] ?? "CF Meet user").join(", ") : v.contextClass === "GROUP" ? "Group conversation" : "Conversation";
    }
    // Inbox order: latest activity first (MSG-007 `orderBy=lastActivity`).
    views.sort((a, b) => b.lastActivityAt.localeCompare(a.lastActivityAt) || b.createdAt.localeCompare(a.createdAt));
    this.set({ conversations: views });
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
    options: { cfMeetIds?: string[]; title?: string } = {},
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
      : { contextClass: "GROUP", securityProfile: profile, ...disclosure, ...(title && profile === "MANAGED" ? { title: title.slice(0, 120) } : {}) };
    const conv = await relay<ServerConversation>("POST", "v1/conversations", body, `web-${uuid()}`);
    if (conv.securityProfile !== profile) {
      // Never silently accept another profile than the one the user chose.
      throw new MessagingApiError(0, "PROFILE_NOT_GRANTED", conv.securityProfile);
    }
    this.server.set(conv.conversationId, conv);
    let principals = people;
    if (!direct) {
      for (const p of people) await relay("POST", `v1/conversations/${conv.conversationId}/members`, { principalId: p });
      for (const cfMeetId of contacts) await relay("POST", `v1/conversations/${conv.conversationId}/members`, { cfMeetId });
      if (contacts.length && profile === "PRIVATE_E2EE") {
        const m = await relay<{ members: { principalId: string }[] }>("GET", `v1/conversations/${conv.conversationId}/members?limit=${PAGE}`);
        const me = this.snapshot.identity?.principalId;
        principals = m.members.map((x) => x.principalId).filter((x) => x !== me);
      }
    } else if (contacts.length && profile === "PRIVATE_E2EE") {
      const me = this.snapshot.identity?.principalId;
      principals = (conv.members ?? []).map((x) => x.principalId).filter((x) => x !== me);
    }
    if (profile === "PRIVATE_E2EE") await this.establishGroup(conv, principals);
    this.subscribeConversation(conv);
    await this.refreshConversations();
    return conv.conversationId;
  }

  /** Opens (or returns the existing) MANAGED direct chat with a connected contact. */
  startDirectWithContact(cfMeetId: string): Promise<string> {
    return this.createConversation([], "MANAGED", { cfMeetIds: [cfMeetId] });
  }

  /** Claims, verifies and adds the devices of `principals` with ONE commit. */
  private async addPrincipalsToGroup(conv: ServerConversation, principals: string[], create: boolean): Promise<void> {
    const c = this.crypto!;
    const gid = b64u.dec(conv.encryption.mlsGroupId!);
    const deviceId = this.snapshot.deviceId!;
    if (create) await c.call("createGroup", { groupId: gid });
    try {
      let packages: Uint8Array[] = [];
      let welcomed: string[] = [];
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
        if (!packages.length) throw new MessagingApiError(409, "NO_DEVICE_TO_ADD", "The other people have no messaging device yet.");
      }
      if (packages.length) {
        const out = await c.call<{ commit: Uint8Array; welcome: Uint8Array | null; epoch: number }>("addDevices", { groupId: gid, keyPackages: packages });
        await relay("POST", `v1/conversations/${conv.conversationId}/mls/commits`, {
          clientMessageId: uuid(),
          deviceId,
          epoch: out.epoch,
          commit: b64u.enc(out.commit),
          ...(out.welcome ? { welcome: b64u.enc(out.welcome), welcomeDeviceIds: welcomed } : {}),
          removedDeviceIds: [],
        });
        packages = [];
        welcomed = [];
      }
      await c.call("confirm");
    } catch (e) {
      await c.call("rollback").catch(() => {});
      throw e;
    }
  }

  private establishGroup(conv: ServerConversation, others: string[]): Promise<void> {
    return this.addPrincipalsToGroup(conv, others, true);
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
  async renameConversation(conversationId: string, title: string | null): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) return;
    const current = await relay<ServerConversation>("GET", `v1/conversations/${conversationId}`);
    await relay("PATCH", `v1/conversations/${conversationId}`, { expectedVersion: current.version ?? conv.version ?? 0, title: title?.trim() ? title.trim().slice(0, 120) : null });
    await this.refreshConversations();
  }

  /** Cryptographic removal: a member device commits the removals the server requires. */
  private async commitPendingRemovals(conv: ServerConversation): Promise<void> {
    const c = this.crypto;
    if (!c || conv.securityProfile !== "PRIVATE_E2EE" || !conv.encryption.mlsGroupId) return;
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (!(await c.call<boolean>("hasGroup", { groupId: gid }))) return;
    const state = await relay<{ pendingRemovals: { deviceId: string }[] }>("GET", `v1/conversations/${conv.conversationId}/mls/state`);
    const inTree = new Set(await c.call<string[]>("members", { groupId: gid }));
    const remove = state.pendingRemovals.map((r) => r.deviceId).filter((d) => inTree.has(d) && d !== this.snapshot.deviceId);
    if (!remove.length) return;
    try {
      const out = await c.call<{ commit: Uint8Array; epoch: number }>("removeDevices", { groupId: gid, deviceIds: remove });
      await relay("POST", `v1/conversations/${conv.conversationId}/mls/commits`, {
        clientMessageId: uuid(),
        deviceId: this.snapshot.deviceId,
        epoch: out.epoch,
        commit: b64u.enc(out.commit),
        removedDeviceIds: remove,
      });
      await c.call("confirm");
    } catch {
      // Another member's device committed first (epoch moved): ours is discarded.
      await c.call("rollback").catch(() => {});
    }
  }

  /**
   * Multi-device: an ACTIVE device of a member that is not yet in the MLS tree (a new
   * browser, a phone) is claimed, verified and added by any member device. Concurrent
   * attempts are resolved by the Delivery Service's epoch order (the loser rolls back).
   */
  private async addMissingDevices(conv: ServerConversation): Promise<void> {
    const last = this.deviceChecks.get(conv.conversationId) ?? 0;
    if (Date.now() - last < DEVICE_CHECK_MS || !this.crypto || !conv.encryption.mlsGroupId) return;
    this.deviceChecks.set(conv.conversationId, Date.now());
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    const inTree = new Set(await this.crypto.call<string[]>("members", { groupId: gid }));
    const members = await relay<{ members: { principalId: string; state?: string }[] }>("GET", `v1/conversations/${conv.conversationId}/members?limit=${PAGE}`);
    const missing: string[] = [];
    const newDevices: ServerDevice[] = [];
    const activeInTree: string[] = [];
    for (const m of members.members ?? []) {
      if (m.state && m.state !== "ACTIVE") continue;
      const d = await relay<{ devices: ServerDevice[] }>("GET", `v1/messaging/principals/${m.principalId}/devices`).catch(() => ({ devices: [] as ServerDevice[] }));
      for (const x of d.devices) if (x.status === "ACTIVE" && inTree.has(x.deviceId)) activeInTree.push(x.deviceId);
      const fresh = d.devices.filter((x) => x.status === "ACTIVE" && !inTree.has(x.deviceId));
      if (fresh.length) {
        missing.push(m.principalId);
        newDevices.push(...fresh);
      }
    }
    if (!missing.length) {
      this.missingSince.delete(conv.conversationId);
      return;
    }
    // One member device adds new devices (a claim consumes a KeyPackage of EVERY device
    // of the principal, so racing members would exhaust the newcomer's stock): the ACTIVE
    // device with the smallest id in the tree. Others step in only after a grace period
    // (the designated device may be offline).
    const designated = activeInTree.sort()[0];
    if (designated !== this.snapshot.deviceId) {
      const seen = this.missingSince.get(conv.conversationId) ?? Date.now();
      this.missingSince.set(conv.conversationId, seen);
      if (Date.now() - seen < 10 * DEVICE_CHECK_MS) return;
    }
    // A claim consumes one KeyPackage of every device of the principal, so nothing is
    // claimed until each new device's listed key verifies against the transparency log
    // (a device the log does not yet include is retried on a later check without
    // spending the newcomer's KeyPackages).
    for (const d of newDevices) {
      const ok = await this.verifyTransparency(d.deviceId, b64u.dec(d.signaturePublicKey)).then(
        () => true,
        () => false,
      );
      if (!ok) return;
    }
    await this.addDevicesOf(conv, missing, inTree).catch(() => {});
  }

  /** Adds the not-yet-present devices of `principals` (claims skip those in the tree). */
  private async addDevicesOf(conv: ServerConversation, principals: string[], inTree: Set<string>): Promise<void> {
    const c = this.crypto!;
    const gid = b64u.dec(conv.encryption.mlsGroupId!);
    const claim = await relay<{ keyPackages: { principalId: string; deviceId: string; keyPackage: string }[] }>(
      "POST",
      `v1/conversations/${conv.conversationId}/mls/key-package-claims`,
      { principalIds: principals },
    );
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
    if (!packages.length) return;
    try {
      const out = await c.call<{ commit: Uint8Array; welcome: Uint8Array | null; epoch: number }>("addDevices", { groupId: gid, keyPackages: packages });
      await relay("POST", `v1/conversations/${conv.conversationId}/mls/commits`, {
        clientMessageId: uuid(),
        deviceId: this.snapshot.deviceId,
        epoch: out.epoch,
        commit: b64u.enc(out.commit),
        ...(out.welcome ? { welcome: b64u.enc(out.welcome), welcomeDeviceIds: welcomed } : {}),
        removedDeviceIds: [],
      });
      await c.call("confirm");
    } catch (e) {
      await c.call("rollback").catch(() => {});
      throw e;
    }
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
          await this.crypto.call("join", { groupId: gid, welcome: b64u.dec(w.welcome) });
          joined++;
        } catch {
          // A Welcome this device cannot use (e.g. superseded): it is still consumed.
        }
      }
      await relay("POST", `v1/messaging/devices/${deviceId}/welcomes/${w.welcomeId}/consume`, {});
      this.scheduleSync(conv.conversationId);
    }
    if (joined) {
      await this.topUpKeyPackages(joined);
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
      await this.renderE2ee(conversationId);
      return;
    }
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (await c.call<boolean>("hasGroup", { groupId: gid })) {
      for (;;) {
        const after = await c.call<number>("cursor", { conversationId });
        const page = await relay<{ messages: ServerMessage[] }>("GET", `v1/conversations/${conversationId}/messages?afterSequence=${after}&limit=${PAGE}`);
        if (!page.messages.length) break;
        const processed = await c.call<{ records: StoredRecord[] }>("processMessages", {
          conversationId,
          groupId: gid,
          messages: page.messages.map((m) => ({
            sequence: m.sequence,
            messageId: m.messageId,
            kind: m.kind,
            lifecycle: m.lifecycle,
            senderPrincipalId: m.senderPrincipalId,
            senderDeviceId: m.senderDeviceId ?? null,
            clientMessageId: m.clientMessageId,
            createdAt: m.createdAt,
            epoch: m.e2ee?.epoch ?? 0,
            ciphertext: m.e2ee?.ciphertext ?? "",
            attachmentIds: m.attachmentIds,
          })),
        });
        // Another device's commit may have used one of this device's KeyPackages (a
        // claim takes one per device of a principal): keep the stock up.
        const foreignCommits = processed.records.filter((r) => r.status === "COMMIT" && r.senderDeviceId !== this.snapshot.deviceId).length;
        if (foreignCommits) await this.topUpKeyPackages(Math.min(foreignCommits, 5)).catch(() => {});
        if (page.messages.length < PAGE) break;
      }
      await this.commitPendingRemovals(conv);
      await this.addMissingDevices(conv);
    }
    await this.renderE2ee(conversationId);
  }

  private async renderE2ee(conversationId: string): Promise<void> {
    const me = this.snapshot.identity?.principalId ?? null;
    const records = this.crypto && this.snapshot.deviceId ? await this.crypto.call<StoredRecord[]>("history", { conversationId }).catch(() => []) : [];
    const views: MessageView[] = [];
    for (const r of records) {
      if (r.status === "COMMIT") continue;
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
      };
      if (r.status === "DECRYPTED" || r.status === "SENT") {
        base.state = r.status === "SENT" ? "sent" : "delivered";
        if (r.payload?.t === "text") base.text = r.payload.text;
        else if (r.payload?.t === "file") {
          base.text = r.payload.caption;
          base.file = { name: r.fileName ?? r.payload.name ?? "attachment", mediaType: r.mediaType ?? r.payload.mediaType ?? "application/octet-stream", size: r.size ?? null, attachmentId: r.attachmentId ?? r.payload.attachmentId };
        }
      } else if (r.status === "SENT_ELSEWHERE") {
        base.state = "system";
        base.note = "Sent from another of your devices";
      } else if (r.status === "BEFORE_JOIN") {
        base.state = "before-join";
        base.note = "Sent before this device joined; recover history from another device to read it";
      } else if (r.status === "REMOVED") {
        base.state = "removed";
        base.note = "Message removed";
      } else {
        base.state = "unreadable";
        base.note = `This message could not be decrypted on this device (${r.error ?? "FAILED"})`;
      }
      views.push(base);
    }
    const pending = this.pendingSends.get(conversationId) ?? [];
    this.set({ messages: { ...this.snapshot.messages, [conversationId]: [...views, ...pending] } });
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
      messages: { ...this.snapshot.messages, [conversationId]: [...views, ...queued, ...pending] },
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
    if (conv?.securityProfile !== "MANAGED") return;
    const st = this.stateFor(conversationId);
    await Promise.all([
      this.loadPins(conversationId).catch(() => {}),
      this.loadReactions(conversationId, st.ordered().filter((r) => r.lifecycle === "ACTIVE").slice(-30).map((r) => r.messageId)).catch(() => {}),
    ]);
  }

  async loadReactions(conversationId: string, messageIds: string[]): Promise<void> {
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
    this.renderManaged(conversationId);
  }

  async loadPins(conversationId: string): Promise<void> {
    const r = await relay<{ pins: PinView[] }>("GET", `v1/conversations/${conversationId}/pins`);
    this.set({ pins: { ...this.snapshot.pins, [conversationId]: r.pins } });
    this.renderManaged(conversationId);
  }

  async react(conversationId: string, messageId: string, reaction: string, add: boolean): Promise<void> {
    await relay(add ? "PUT" : "DELETE", `v1/conversations/${conversationId}/messages/${messageId}/reactions/${reactionSegment(reaction)}`, add ? {} : undefined);
    await this.loadReactions(conversationId, [messageId]);
  }

  async pin(conversationId: string, messageId: string, pinned: boolean): Promise<void> {
    await relay(pinned ? "PUT" : "DELETE", `v1/conversations/${conversationId}/pins/${messageId}`, pinned ? {} : undefined);
    await this.loadPins(conversationId);
  }

  /** MANAGED edit (`PATCH`, compare-and-swap on the edit count). */
  async editMessage(conversationId: string, messageId: string, text: string): Promise<void> {
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
    const list = this.snapshot.messages[conversationId] ?? [];
    const last = [...list].reverse().find((m) => m.sequence !== null)?.sequence;
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
    this.set({ messages: { ...this.snapshot.messages, [conversationId]: [...shown, view] } });
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
    return this.enqueue(conversationId, () => this.deliver(conv, conversationId, key, text));
  }

  private async deliver(conv: ServerConversation, conversationId: string, key: string, text: string): Promise<void> {
    try {
      if (conv.securityProfile === "PRIVATE_E2EE") await this.sendEncrypted(conv, { t: "text", text }, []);
      else await relay("POST", `v1/conversations/${conversationId}/messages`, { clientMessageId: uuid(), text });
      this.settlePending(conversationId, key, null);
    } catch (e) {
      this.settlePending(conversationId, key, `Not sent (${e instanceof Error ? e.message : "error"})`);
    }
    await this.sync(conversationId);
  }

  private async sendEncrypted(conv: ServerConversation, payload: Record<string, unknown>, attachmentIds: string[]): Promise<void> {
    const c = this.crypto;
    const deviceId = this.snapshot.deviceId;
    if (!c || !deviceId || !conv.encryption.mlsGroupId) throw new MessagingApiError(0, "DEVICE_REQUIRED", null);
    const gid = b64u.dec(conv.encryption.mlsGroupId);
    if (!(await c.call<boolean>("hasGroup", { groupId: gid }))) throw new MessagingApiError(0, "NOT_IN_GROUP", null);
    for (let attempt = 0; attempt < 2; attempt++) {
      // Process everything first: a message must be sent in the current epoch.
      await this.sync(conv.conversationId);
      const clientMessageId = uuid();
      const out = await c.call<{ ciphertext: Uint8Array; epoch: number }>("encrypt", { conversationId: conv.conversationId, groupId: gid, clientMessageId, payload });
      try {
        await relay("POST", `v1/conversations/${conv.conversationId}/messages`, {
          clientMessageId,
          deviceId,
          epoch: out.epoch,
          ciphertext: b64u.enc(out.ciphertext),
          ...(attachmentIds.length ? { attachmentIds } : {}),
        });
        return;
      } catch (e) {
        const stale = e instanceof MessagingApiError && e.status === 409;
        if (!stale || attempt === 1) throw e;
      }
    }
  }

  /** Sends a file: PRIVATE_E2EE encrypts it here (only ciphertext is uploaded). */
  async sendFile(conversationId: string, file: File): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv) return;
    const key = `p${uuid()}`;
    this.addPending(conversationId, { key, sequence: null, senderPrincipalId: this.snapshot.identity?.principalId ?? null, mine: true, createdAt: new Date().toISOString(), profile: conv.securityProfile, state: "sending", text: null, file: { name: file.name, mediaType: file.type || "application/octet-stream", size: file.size, attachmentId: null }, note: null, transferred: false });
    return this.enqueue(conversationId, () => this.deliverFile(conv, conversationId, key, file));
  }

  private async deliverFile(conv: ServerConversation, conversationId: string, key: string, file: File): Promise<void> {
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
        await this.sendEncrypted(conv, { t: "file", handle: enc.handle, attachmentId: intent.attachmentId }, [intent.attachmentId]);
      } else {
        const intent = await relay<{ attachmentId: string; upload: { url: string; headers: { name: string; value: string }[] } }>("POST", `v1/conversations/${conversationId}/attachments`, {
          kind: attachmentKind(mediaType),
          size: data.length,
          sha256: await sha256Hex(data),
          mediaType,
          fileName: file.name.slice(0, 255),
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
        const clientMessageId = uuid();
        const send = () => relay<ManagedServerMessage>("POST", `v1/conversations/${conversationId}/messages`, { clientMessageId, text: file.name.slice(0, 255), attachmentIds: [intent.attachmentId] });
        // A lost response is retried with the same clientMessageId (answered as a replay).
        const msg = await send().catch((e) => (e instanceof MessagingApiError && (e.status === 0 || e.status >= 500) ? send() : Promise.reject(e)));
        this.stateFor(conversationId).applyMessages([msg]);
      }
      this.settlePending(conversationId, key, null);
    } catch (e) {
      this.settlePending(conversationId, key, `Not sent (${e instanceof Error ? e.message : "error"})`);
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
    const transferKey = await c.call<Uint8Array>("beginTransfer");
    const expires = Math.floor(Date.now() / 1000) + 10 * 60;
    const signature = await c.call<Uint8Array>("signTransferRequest", { context: ctx, expiresAt: expires });
    await relay("POST", "v1/messaging/history-transfers", {
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

  /** SOURCE: requests addressed to this device, each with the code the user compares. */
  async checkIncomingTransfers(): Promise<void> {
    const me = this.snapshot.deviceId;
    if (!me || !this.crypto || this.snapshot.phase !== "ready") return;
    const page = await relay<{ transfers: { transferId: string; sourceDeviceId: string; targetDeviceId: string; transferPublicKey: string; requestSignature: string; status: string; expiresAt: string }[] }>(
      "GET",
      `v1/messaging/history-transfers?deviceId=${me}`,
    );
    const devices = await this.ownDevices();
    const incoming: IncomingTransfer[] = [];
    for (const t of page.transfers ?? []) {
      if (t.sourceDeviceId !== me || t.status !== "REQUESTED") continue;
      const target = devices.find((d) => d.deviceId === t.targetDeviceId && d.status === "ACTIVE");
      const self = devices.find((d) => d.deviceId === me);
      if (!target || !self) continue;
      const ctx = this.transferContext(t.transferId, me, t.targetDeviceId);
      const expires = Math.floor(Date.parse(t.expiresAt) / 1000);
      const ok = await this.crypto.call<boolean>("verifyTransferRequest", { targetKey: b64u.dec(target.signaturePublicKey), context: ctx, transferKey: b64u.dec(t.transferPublicKey), expiresAt: expires, signature: b64u.dec(t.requestSignature) });
      if (!ok) continue;
      const code = await this.crypto.call<string>("comparisonCode", { context: ctx, transferKey: b64u.dec(t.transferPublicKey), sourceKey: b64u.dec(self.signaturePublicKey), targetKey: b64u.dec(target.signaturePublicKey) });
      incoming.push({ transferId: t.transferId, targetDeviceId: t.targetDeviceId, comparisonCode: code, expiresAt: t.expiresAt });
    }
    this.set({ incomingTransfers: incoming });
  }

  /** SOURCE: the user confirmed the codes match — seal this device's history to the target. */
  async approveTransfer(transferId: string): Promise<void> {
    const me = this.snapshot.deviceId!;
    const t = await relay<{ transferId: string; targetDeviceId: string; transferPublicKey: string; status: string }>("GET", `v1/messaging/history-transfers/${transferId}`);
    if (t.status !== "REQUESTED") throw new MessagingApiError(0, `TRANSFER_${t.status}`, null);
    const ctx = this.transferContext(transferId, me, t.targetDeviceId);
    const r = await this.crypto!.call<{ sealed: Uint8Array }>("sealHistory", { context: ctx, transferKey: b64u.dec(t.transferPublicKey) });
    await relay("PUT", `v1/messaging/history-transfers/${transferId}/sealed-history`, { sealedHistory: b64u.enc(r.sealed) });
    this.set({ incomingTransfers: this.snapshot.incomingTransfers.filter((x) => x.transferId !== transferId) });
  }

  async declineTransfer(transferId: string): Promise<void> {
    await relay("POST", `v1/messaging/history-transfers/${transferId}/cancel`, {});
    this.set({ incomingTransfers: this.snapshot.incomingTransfers.filter((x) => x.transferId !== transferId) });
  }
}

/**
 * The CF Meet web Messaging client (Part V Milestone B): one engine per signed-in account
 * and tab, driving
 *
 * * the device worker (`crypto-client.ts`) — registration, MLS groups, encryption,
 *   encrypted local history, key transparency, history transfer;
 * * the Meeting Service relay (`api.ts`) — every REST operation, acting for the
 *   account's own principal;
 * * the realtime gateway (`services/messaging-realtime.ts`) — device-bound client token,
 *   single-use ticket, cursor resume. Events only trigger an authoritative REST sync, so
 *   duplicates, gaps and reconnects all converge on the Delivery Service's order.
 *
 * Security rules this file keeps (and the browser tests check): a PRIVATE_E2EE
 * conversation is never downgraded or sent in the clear (no MANAGED fallback); the UI is
 * told the conversation's real profile; a KeyPackage is added only after its device and
 * key were verified (and, with a pinned log key, found in the transparency log); message
 * text is only ever rendered as text.
 */
import { MessagingRealtimeTransport } from "@/services/messaging-realtime";
import { messagingTokenProvider } from "@/services/messaging-session";
import { cookieStore } from "@/store";

import { b64u, identityApi, MessagingApiError, relay, type Colleague, type MessagingIdentity } from "./api";
import { CryptoClient, CryptoWorkerError, type StoredRecord } from "./crypto-client";

export type SecurityProfile = "MANAGED" | "PRIVATE_E2EE";
export type Phase = "loading" | "needs-device" | "registering" | "ready" | "locked" | "revoked" | "unavailable" | "error";
export type Connection = "connected" | "connecting" | "disconnected" | "auth-required" | "offline";

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
};

export type MessageView = {
  key: string;
  sequence: number | null;
  senderPrincipalId: string | null;
  mine: boolean;
  createdAt: string | null;
  profile: SecurityProfile;
  state: "sending" | "sent" | "delivered" | "failed" | "unreadable" | "removed" | "before-join" | "system";
  text: string | null;
  file: { name: string; mediaType: string; size: number | null; attachmentId: string | null } | null;
  note: string | null;
  transferred: boolean;
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
};

type ServerConversation = {
  conversationId: string;
  contextClass: string;
  securityProfile: SecurityProfile;
  lifecycleState: string;
  createdAt: string;
  encryption: { mlsGroupId: string | null; serverCanReadContent: boolean };
  members?: { principalId: string }[];
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
const KEY_PACKAGE_STOCK = 10;
const POLL_MS = Math.max(1000, Number(process.env.NEXT_PUBLIC_MESSAGING_POLL_MS) || 15_000);
/** How often a member device checks an encrypted group for members' new devices. */
const DEVICE_CHECK_MS = Math.max(1000, Number(process.env.NEXT_PUBLIC_MESSAGING_DEVICE_CHECK_MS) || 30_000);
const MESSAGING_URL = process.env.NEXT_PUBLIC_MESSAGING_URL ?? "";
const API_V1_URL = process.env.NEXT_PUBLIC_API_URL ?? "";
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
  };
  private readonly listeners = new Set<() => void>();
  private crypto: CryptoClient | null = null;
  private transport: MessagingRealtimeTransport | null = null;
  private server = new Map<string, ServerConversation>();
  private managedCursor = new Map<string, number>();
  private syncing = new Map<string, Promise<void>>();
  private resync = new Set<string>();
  private timers: ReturnType<typeof setInterval>[] = [];
  private reconnectDelay = 1000;
  private stopped = false;
  private pendingSends = new Map<string, MessageView[]>();
  private deviceChecks = new Map<string, number>();
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
    if (this.stopped || this.snapshot.phase !== "ready") return;
    this.reconnectDelay = 1000;
    this.connect();
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
          return;
        }
        this.set({ phase: "ready", deviceId: opened.deviceId });
        await this.topUpKeyPackages();
        this.connect();
      } else {
        this.set({ phase: "needs-device" });
      }
      await this.refreshConversations();
      await this.checkWelcomes();
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

  private async onDeviceRevoked(detail = "This browser's messaging device was revoked. Its local keys and history were erased."): Promise<void> {
    this.detachTransport();
    await this.crypto?.call("wipe", { account: this.snapshot.identity!.principalId });
    await this.reopenCrypto();
    this.set({ phase: "revoked", detail, deviceId: null, messages: {}, connection: "disconnected" });
    await this.refreshConversations();
  }

  /** Logout: the local device and its history are erased and the device revoked. */
  async signOut(): Promise<void> {
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
    const deviceId = this.snapshot.deviceId;
    if (typeof navigator !== "undefined" && navigator.onLine === false) {
      this.set({ connection: "offline" });
      return;
    }
    if (!deviceId || !MESSAGING_URL || this.stopped) {
      if (!MESSAGING_URL) this.set({ connection: "offline" });
      return;
    }
    this.detachTransport();
    const transport = new MessagingRealtimeTransport({
      baseUrl: MESSAGING_URL,
      getAccessToken: messagingTokenProvider({ meetingApiV1Url: API_V1_URL, getLoginToken: () => cookieStore.getAccessToken() ?? null, deviceId }),
      onEvent: (event) => {
        this.scheduleSync(event.conversationId);
      },
      onSyncRequired: (conversationId) => {
        void this.syncAuthoritative(conversationId);
      },
      onConnectionState: (state) => {
        if (this.transport !== transport) return;
        this.set({ connection: state });
        if (state === "connected") {
          this.reconnectDelay = 1000;
          // Missed while offline: catch up every conversation from the Delivery Service.
          for (const id of this.server.keys()) this.scheduleSync(id);
        } else if (!this.stopped) {
          this.scheduleReconnect(state === "auth-required");
        }
      },
    });
    this.transport = transport;
    for (const id of this.server.keys()) transport.subscribe(id, 0);
    this.set({ connection: "connecting" });
    transport.connect().catch(() => {
      if (this.transport === transport && !this.stopped) this.scheduleReconnect(false);
    });
  }

  private scheduleReconnect(authRequired: boolean): void {
    const delay = this.reconnectDelay;
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30_000);
    setTimeout(async () => {
      if (this.stopped || this.snapshot.phase !== "ready") return;
      if (authRequired && this.snapshot.deviceId && !(await this.deviceIsActive(this.snapshot.deviceId).catch(() => true))) {
        await this.onDeviceRevoked();
        return;
      }
      this.connect();
    }, delay + Math.floor(Math.random() * 250));
  }

  private async syncAuthoritative(conversationId: string): Promise<void> {
    await this.sync(conversationId);
    // The event cursor the gateway resumes from: the head of the durable event log.
    let after = this.transport?.cursor(conversationId) ?? 0;
    for (;;) {
      const page = await relay<{ events: { eventSequence: number }[] }>(
        "GET",
        `v1/conversations/${conversationId}/events?afterEventSequence=${after}&limit=${PAGE}`,
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
        `v1/conversations?limit=${PAGE}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`,
      );
      out.push(...page.conversations);
      cursor = page.nextCursor;
      if (!cursor) break;
    }
    // The list carries at most two members; groups are read in full.
    for (const c of out) {
      if (c.contextClass !== "DIRECT") {
        const m = await relay<{ members: { principalId: string }[] }>("GET", `v1/conversations/${c.conversationId}/members?limit=${PAGE}`).catch(() => null);
        if (m) c.members = m.members.map((x) => ({ principalId: x.principalId }));
      }
    }
    const fresh = out.filter((c) => !this.server.has(c.conversationId));
    const gone = [...this.server.keys()].filter((id) => !out.some((c) => c.conversationId === id));
    for (const id of gone) {
      // No longer a member (removed / left): it disappears from this client.
      this.server.delete(id);
      this.transport?.unsubscribe(id);
    }
    for (const c of out) this.server.set(c.conversationId, c);
    for (const c of fresh) this.transport?.subscribe(c.conversationId, 0);
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
      v.title = others.length ? others.map((m) => this.snapshot.names[m] ?? "CF Meet user").join(", ") : v.contextClass === "GROUP" ? "Group conversation" : "Conversation";
    }
    views.sort((a, b) => b.createdAt.localeCompare(a.createdAt));
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
  async createConversation(userIds: string[], profile: SecurityProfile): Promise<string> {
    const people = (await identityApi.resolve(userIds)).people.map((p) => p.principalId);
    if (profile === "PRIVATE_E2EE" && this.snapshot.phase !== "ready") throw new MessagingApiError(0, "DEVICE_REQUIRED", null);
    const direct = people.length === 1;
    const body = direct
      ? { contextClass: "DIRECT", counterpartPrincipalId: people[0], securityProfile: profile, ...(profile === "MANAGED" ? { managedDisclosureAcknowledged: true } : {}) }
      : { contextClass: "GROUP", securityProfile: profile, ...(profile === "MANAGED" ? { managedDisclosureAcknowledged: true } : {}) };
    const conv = await relay<ServerConversation>("POST", "v1/conversations", body, `web-${uuid()}`);
    if (conv.securityProfile !== profile) {
      // Never silently accept another profile than the one the user chose.
      throw new MessagingApiError(0, "PROFILE_NOT_GRANTED", conv.securityProfile);
    }
    this.server.set(conv.conversationId, conv);
    if (!direct) {
      for (const p of people) await relay("POST", `v1/conversations/${conv.conversationId}/members`, { principalId: p });
    }
    if (profile === "PRIVATE_E2EE") await this.establishGroup(conv, people);
    this.transport?.subscribe(conv.conversationId, 0);
    await this.refreshConversations();
    return conv.conversationId;
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
    const activeInTree: string[] = [];
    for (const m of members.members ?? []) {
      if (m.state && m.state !== "ACTIVE") continue;
      const d = await relay<{ devices: ServerDevice[] }>("GET", `v1/messaging/principals/${m.principalId}/devices`).catch(() => ({ devices: [] as ServerDevice[] }));
      for (const x of d.devices) if (x.status === "ACTIVE" && inTree.has(x.deviceId)) activeInTree.push(x.deviceId);
      if (d.devices.some((x) => x.status === "ACTIVE" && !inTree.has(x.deviceId))) missing.push(m.principalId);
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

  private async syncManaged(conv: ServerConversation): Promise<void> {
    const me = this.snapshot.identity?.principalId ?? null;
    const existing = (this.snapshot.messages[conv.conversationId] ?? []).filter((m) => m.state !== "sending" && m.state !== "failed");
    let after = this.managedCursor.get(conv.conversationId) ?? 0;
    const views = [...existing];
    for (;;) {
      const page = await relay<{ messages: ServerMessage[] }>("GET", `v1/conversations/${conv.conversationId}/messages?afterSequence=${after}&limit=${PAGE}`);
      for (const m of page.messages) {
        after = Math.max(after, m.sequence);
        if (m.kind !== "APPLICATION") continue;
        views.push({
          key: `s${m.sequence}`,
          sequence: m.sequence,
          senderPrincipalId: m.senderPrincipalId,
          mine: m.senderPrincipalId === me,
          createdAt: m.createdAt,
          profile: "MANAGED",
          state: m.lifecycle === "ACTIVE" ? "delivered" : "removed",
          text: m.lifecycle === "ACTIVE" ? (m.managed?.text ?? null) : null,
          file: m.attachmentIds.length ? { name: "attachment", mediaType: "application/octet-stream", size: null, attachmentId: m.attachmentIds[0] } : null,
          note: m.lifecycle === "ACTIVE" ? null : "Message removed",
          transferred: false,
        });
      }
      if (page.messages.length < PAGE) break;
    }
    this.managedCursor.set(conv.conversationId, after);
    const pending = this.pendingSends.get(conv.conversationId) ?? [];
    this.set({ messages: { ...this.snapshot.messages, [conv.conversationId]: [...views, ...pending] } });
  }

  async markRead(conversationId: string): Promise<void> {
    const list = this.snapshot.messages[conversationId] ?? [];
    const last = [...list].reverse().find((m) => m.sequence !== null)?.sequence;
    if (last) await relay("PUT", `v1/conversations/${conversationId}/read-cursor`, { sequence: last }).catch(() => {});
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

  /** Sends text. PRIVATE_E2EE: encrypted on this device, or refused — never in the clear. */
  async send(conversationId: string, text: string): Promise<void> {
    const conv = this.server.get(conversationId);
    if (!conv || !text.trim()) return;
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
        await this.upload(intent.upload, data);
        const fin = await relay<{ state: string }>("POST", `v1/conversations/${conversationId}/attachments/${intent.attachmentId}/finalize`, {});
        if (fin.state !== "READY") throw new MessagingApiError(0, `ATTACHMENT_${fin.state}`, null);
        await relay("POST", `v1/conversations/${conversationId}/messages`, { clientMessageId: uuid(), text: file.name.slice(0, 255), attachmentIds: [intent.attachmentId] });
      }
      this.settlePending(conversationId, key, null);
    } catch (e) {
      this.settlePending(conversationId, key, `Not sent (${e instanceof Error ? e.message : "error"})`);
    }
    await this.sync(conversationId);
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

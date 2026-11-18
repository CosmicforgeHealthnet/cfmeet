// Partner chat on a device (ADR 0062; build contract §4, §5).
//
//   user token ──POST /v1/messaging/user-sessions──▶ partner session device + client token
//   client token ──POST /v1/realtime/tickets──▶ ticket ──WebSocket /v1/realtime──▶ events
//   client token ──REST (CF-Client-Token, CF-Acting-Principal: <principalId>)──▶ messages
//
// The session device and its client token are renewed through the same exchange (with
// `sessionDeviceId`) before they expire; the realtime channel reconnects with backoff and
// a fresh ticket each time. Partner devices are MANAGED-only (no E2EE / MLS here).

import type { CfMeetAuth } from './auth.js';
import { CfMeetClient, type ParamsOf, type ResponseOf } from './client.js';
import { Emitter, RenewingCredential, expiryMs, isAuthRefusal, type Expiring } from './device-shared.js';
import { CfMeetApiError, CfMeetConfigurationError, CfMeetError } from './errors.js';
import type { Messaging } from './generated/models.js';
import type { InstallationIdSource } from './installation.js';
import { isValidInstallationId } from './installation.js';
import { DEVICE_ROUTES } from './platform-routes.js';
import type { RealtimeCloseInfo, RealtimeConnection, RealtimeOptions } from './realtime.js';
import type { ApiResponse, RequestOptions, RequestParams } from './types.js';

/** `POST /v1/messaging/user-sessions` answer. */
export interface MessagingUserSession {
  readonly principalId: string;
  readonly sessionDeviceId: string;
  readonly sessionDeviceExpiresAt: string;
  readonly clientToken: string;
  readonly clientTokenExpiresAt: string;
}

interface ChatCredential extends Expiring {
  readonly session: MessagingUserSession;
}

export interface ChatEvents<M> {
  /** A realtime frame (Messaging realtime events, `type` e.g. `message.created`). */
  message: { readonly data: M; readonly raw: unknown };
  open: { readonly attempt: number; readonly protocol: string };
  close: { readonly code: number; readonly reason: string; readonly willReconnect: boolean };
  reconnecting: { readonly attempt: number; readonly delayMs: number };
  /** The session device / client token was (re)issued. */
  renewed: { readonly session: MessagingUserSession };
  error: { readonly error: unknown };
  /** The chat session ended for good (close(), or the user can no longer chat). */
  closed: RealtimeCloseInfo;
}

export interface ChatConnectOptions extends Omit<RealtimeOptions, 'signal'> {
  /** Defaults to the device client's persisted installation id. */
  readonly installationId?: string;
  /** Open the realtime channel (default true); false for REST-only use. */
  readonly realtime?: boolean;
  /** Ends the session when aborted. */
  readonly signal?: AbortSignal;
}

export interface ChatDefaults {
  readonly installationIds: InstallationIdSource;
  readonly realtime: Omit<RealtimeOptions, 'signal'>;
  readonly renewBeforeMs: number;
  readonly autoRenew: boolean;
  readonly now: () => number;
}

/** Text, or a full MANAGED send body. */
export type ChatMessageInput = string | Omit<Messaging.SendMessageRequest, 'ciphertext' | 'epoch' | 'deviceId'>;

function newUuid(): string {
  const c = (globalThis as { crypto?: Crypto }).crypto;
  if (c && typeof c.randomUUID === 'function') return c.randomUUID();
  throw new CfMeetConfigurationError('no crypto.randomUUID in this runtime: pass clientMessageId explicitly');
}

/** `device.chat`: starts chat sessions. */
export class ChatClient {
  readonly #client: CfMeetClient;
  readonly #defaults: ChatDefaults;

  constructor(client: CfMeetClient, defaults: ChatDefaults) {
    this.#client = client;
    this.#defaults = defaults;
  }

  /**
   * Exchanges the user token for a partner session device, then (unless
   * `realtime: false`) opens the realtime channel. Resolves once the exchange succeeded;
   * listen to `open` / `message` for the channel.
   */
  async connect<M = unknown>(options: ChatConnectOptions = {}): Promise<ChatSession<M>> {
    const installationId = options.installationId ?? (await this.#defaults.installationIds.get());
    if (!isValidInstallationId(installationId)) throw new CfMeetConfigurationError('installationId must be 8-128 characters of [A-Za-z0-9._-]');
    const session = new ChatSession<M>(this.#client, installationId, options, this.#defaults);
    await session.start();
    return session;
  }
}

/** One device's chat: REST with the client token, and the realtime event stream. */
export class ChatSession<M = unknown> {
  readonly installationId: string;
  readonly #client: CfMeetClient;
  readonly #options: ChatConnectOptions;
  readonly #defaults: ChatDefaults;
  readonly #events = new Emitter<ChatEvents<M>>();
  readonly #credential: RenewingCredential<ChatCredential>;
  /** Client-token auth for any CLIENT_TOKEN operation (renewed transparently). */
  readonly clientAuth: CfMeetAuth;
  #conn: RealtimeConnection<M> | null = null;
  #closed = false;
  #reopened = 0;
  #resolveClosed!: (info: RealtimeCloseInfo) => void;
  /** Resolves once the session ended for good. */
  readonly closed: Promise<RealtimeCloseInfo>;

  constructor(client: CfMeetClient, installationId: string, options: ChatConnectOptions, defaults: ChatDefaults) {
    this.#client = client;
    this.installationId = installationId;
    this.#options = options;
    this.#defaults = defaults;
    this.closed = new Promise((r) => (this.#resolveClosed = r));
    this.#credential = new RenewingCredential<ChatCredential>((prev) => this.#exchange(prev), {
      renewBeforeMs: defaults.renewBeforeMs,
      autoRenew: defaults.autoRenew,
      now: defaults.now,
      onRenewed: () => this.#events.emit('renewed', { session: this.#credential.current!.session }),
      onError: (error) => this.#events.emit('error', { error }),
    });
    this.clientAuth = { type: 'clientToken', token: () => this.clientToken() };
    options.signal?.addEventListener('abort', () => this.close(), { once: true });
  }

  /** @internal Called by `ChatClient.connect`. */
  async start(): Promise<void> {
    await this.#credential.get();
    if (this.#options.realtime !== false && !this.#closed) this.#openRealtime();
  }

  /** The user's principal id (the acting principal of every request). */
  get principalId(): string {
    const c = this.#credential.current;
    if (!c) throw new CfMeetError('the chat session is not connected');
    return c.session.principalId;
  }

  /** The current session device (ids and expiries; the client token is not repeated here). */
  get sessionDevice(): { readonly sessionDeviceId: string; readonly sessionDeviceExpiresAt: string; readonly clientTokenExpiresAt: string } | null {
    const s = this.#credential.current?.session;
    return s ? { sessionDeviceId: s.sessionDeviceId, sessionDeviceExpiresAt: s.sessionDeviceExpiresAt, clientTokenExpiresAt: s.clientTokenExpiresAt } : null;
  }

  /** The live realtime connection (null with `realtime: false` or after close). */
  get realtime(): RealtimeConnection<M> | null {
    return this.#conn;
  }

  on<K extends keyof ChatEvents<M>>(event: K, handler: (ev: ChatEvents<M>[K]) => void): () => void {
    return this.#events.on(event, handler);
  }

  /** A valid client token (renewing the session device when it is about to expire). */
  async clientToken(): Promise<string> {
    return (await this.#credential.get()).session.clientToken;
  }

  /** Sends a MANAGED message (a `clientMessageId` is generated when absent, so a resend is deduplicated). */
  sendMessage(conversationId: string, message: ChatMessageInput, options?: RequestOptions): Promise<ApiResponse<ResponseOf<'messaging', 'sendMessage'>>> {
    const body: Messaging.SendMessageRequest =
      typeof message === 'string' ? { text: message, clientMessageId: newUuid() } : { ...message, clientMessageId: message.clientMessageId ?? newUuid() };
    return this.request('sendMessage', { path: { conversationId }, body }, options);
  }

  /** One page of a conversation's messages. */
  listMessages(
    conversationId: string,
    query: NonNullable<ParamsOf<'messaging', 'listMessages'>['query']> = {},
    options?: RequestOptions,
  ): Promise<ApiResponse<ResponseOf<'messaging', 'listMessages'>>> {
    return this.request('listMessages', { path: { conversationId }, query }, options);
  }

  /** One page of the user's conversations. */
  listConversations(
    query: NonNullable<ParamsOf<'messaging', 'listConversations'>['query']> = {},
    options?: RequestOptions,
  ): Promise<ApiResponse<ResponseOf<'messaging', 'listConversations'>>> {
    return this.request('listConversations', { query }, options);
  }

  /** Moves the user's read cursor to `sequence` (monotonic). */
  markRead(conversationId: string, sequence: number, options?: RequestOptions): Promise<ApiResponse<ResponseOf<'messaging', 'setReadCursor'>>> {
    return this.request('setReadCursor', { path: { conversationId }, body: { sequence } }, options);
  }

  getUnreadCount(conversationId: string, options?: RequestOptions): Promise<ApiResponse<ResponseOf<'messaging', 'getUnreadCount'>>> {
    return this.request('getUnreadCount', { path: { conversationId } }, options);
  }

  /**
   * Any Messaging operation a partner session device may call (the partner allowlist:
   * conversations, members, messages, reactions, read cursor, unread, events,
   * attachments, search, report), with the client token and `CF-Acting-Principal` set.
   * A refused client token is renewed and the call retried once.
   */
  async request<T = unknown>(name: string, params: RequestParams = {}, options: RequestOptions = {}): Promise<ApiResponse<T>> {
    const spec = CfMeetClient.operation('messaging', name);
    if (!spec.auth.includes('CLIENT_TOKEN')) throw new CfMeetConfigurationError(`messaging.${name} does not accept a client token`);
    if (this.#closed) throw new CfMeetError('the chat session is closed');
    const call = async () => {
      const { session } = await this.#credential.get();
      const acting = spec.headers.some((h) => h.name === 'cf-acting-principal');
      const headers = acting ? { ...params.headers, 'cf-acting-principal': session.principalId } : params.headers;
      return this.#client.request<T>(spec, { ...params, headers }, { ...options, auth: { type: 'clientToken', token: session.clientToken } });
    };
    try {
      return await call();
    } catch (e) {
      if (!isAuthRefusal(e)) throw e;
      this.#credential.markStale();
      return call();
    }
  }

  /** Ends the session: closes the realtime channel and stops renewing. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#credential.close();
    const conn = this.#conn;
    this.#conn = null;
    conn?.close();
    this.#finish({ code: 1000, reason: 'client closed', cause: 'client' });
  }

  #finish(info: RealtimeCloseInfo): void {
    this.#closed = true;
    this.#credential.close();
    this.#events.emit('closed', info);
    this.#resolveClosed(info);
  }

  async #exchange(previous: ChatCredential | null): Promise<ChatCredential> {
    const send = (sessionDeviceId?: string) =>
      this.#client.request<MessagingUserSession>(DEVICE_ROUTES.messagingUserSession, {
        body: sessionDeviceId ? { installationId: this.installationId, sessionDeviceId } : { installationId: this.installationId },
      });
    let res: ApiResponse<MessagingUserSession>;
    try {
      res = await send(previous?.session.sessionDeviceId);
    } catch (e) {
      // The device ended (expired, revoked): start a new one for this installation.
      if (!(previous && e instanceof CfMeetApiError && e.status === 404)) throw e;
      res = await send();
    }
    const s = res.data;
    if (!s || typeof s.clientToken !== 'string' || typeof s.principalId !== 'string' || typeof s.sessionDeviceId !== 'string') {
      throw new CfMeetError('the messaging user session response is malformed');
    }
    const tokenExp = expiryMs(s.clientTokenExpiresAt, 'client token');
    const deviceExp = expiryMs(s.sessionDeviceExpiresAt, 'session device');
    return { session: s, expiresAtMs: Math.min(tokenExp, deviceExp) };
  }

  #openRealtime(): void {
    const { realtime } = this.#defaults;
    const conn = this.#client.realtime.messaging<M>({ ...realtime, ...this.#options, signal: undefined, auth: this.clientAuth });
    this.#conn = conn;
    conn.on('message', (e) => this.#events.emit('message', e));
    conn.on('open', (e) => {
      this.#reopened = 0;
      this.#events.emit('open', e);
    });
    conn.on('close', (e) => this.#events.emit('close', e));
    conn.on('reconnecting', (e) => this.#events.emit('reconnecting', e));
    conn.on('error', (e) => this.#events.emit('error', e));
    conn.on('closed', (info) => {
      if (this.#closed || this.#conn !== conn) return;
      this.#conn = null;
      // The ticket was refused with the client token (device ended, token revoked): one
      // fresh exchange with the user token, then a new channel. Twice in a row: give up.
      if (info.cause === 'refused' && isAuthRefusal(info.error) && this.#reopened < 1) {
        this.#reopened++;
        this.#credential.markStale();
        this.#credential.get().then(
          () => {
            if (!this.#closed) this.#openRealtime();
          },
          (error: unknown) => {
            this.#events.emit('error', { error });
            this.#finish({ ...info, error });
          },
        );
        return;
      }
      this.#finish(info);
    });
  }
}

// Realtime WebSocket channels, with automatic reconnect.
//
// | channel                   | ticket / credential                         | placement (per contract)                             |
// |---------------------------|---------------------------------------------|------------------------------------------------------|
// | messaging  /v1/realtime   | POST /v1/realtime/tickets (client token)    | Sec-WebSocket-Protocol: cfmeet.messaging.v1,         |
// |                           |                                             |   cfmeet.ticket.<ticket>                             |
// | meetings   /api/v2/       | POST /api/v2/conferences/{id}/realtime/     | query ?ticket=<ticket>&cursor=<last sequence>        |
// |   conferences/{id}/realtime | ticket (user session / participant token) |   (the only channel whose contract puts it in the URL) |
// | calling    /v1/calling/   | the endpoint's signaling credential         | Sec-WebSocket-Protocol: cfmeet.signaling.v1,         |
// |   signaling               |   (cfsig_..., provider recommended)         |   bearer.<credential>                                |
//
// A fresh ticket / credential is obtained for EVERY (re)connect. Reconnects back off
// exponentially with jitter; they stop on an explicit close(), on a terminal close code
// (auth / permission / policy failures) and when ticket issuance is refused (401/403/404).

import { resolveToken, type CfMeetAuth, type TokenSource } from './auth.js';
import type { CfMeetClient } from './client.js';
import { CfMeetApiError, CfMeetConfigurationError, CfMeetError } from './errors.js';
import type { Meetings, Messaging } from './generated/models.js';
import { OPERATIONS, type OperationSpec } from './generated/operations.js';
import { buildPath } from './path.js';

/** The subset of the WHATWG WebSocket the SDK uses (the global WebSocket of browsers and Node >= 22). */
export interface WebSocketLike {
  readonly readyState: number;
  readonly protocol: string;
  binaryType: string;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code: number; reason: string; wasClean?: boolean }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  send(data: string | ArrayBufferLike | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
}
export type WebSocketConstructorLike = new (url: string, protocols?: string | string[]) => WebSocketLike;

export interface ReconnectOptions {
  /** Consecutive failed attempts before giving up (default 10). */
  readonly maxAttempts?: number;
  /** First delay (default 500 ms), doubled per failure with jitter. */
  readonly initialDelayMs?: number;
  /** Delay cap (default 30 s). */
  readonly maxDelayMs?: number;
  /**
   * Waits before a reconnect (default: a real timer). Tests inject a fake clock
   * (`createFakeClock().sleep` from `@cfmeet/platform-sdk/testing`); close() still wins.
   */
  readonly sleep?: (ms: number) => Promise<void>;
}

export interface RealtimeOptions {
  /** WebSocket implementation (defaults to globalThis.WebSocket). */
  readonly WebSocket?: WebSocketConstructorLike;
  /** Reconnect tuning; false disables reconnecting. */
  readonly reconnect?: ReconnectOptions | false;
  /** Overrides which close codes end the connection for good. */
  readonly isTerminalClose?: (code: number, reason: string) => boolean;
  /** Closes the connection when aborted. */
  readonly signal?: AbortSignal;
  /** A socket not open within this time is abandoned and retried (default 15 s). */
  readonly connectTimeoutMs?: number;
}

export type RealtimeState = 'connecting' | 'open' | 'reconnecting' | 'closed';

export interface RealtimeCloseInfo {
  readonly code: number;
  readonly reason: string;
  /** Why the connection ended for good. */
  readonly cause: 'client' | 'terminal_close' | 'refused' | 'reconnect_exhausted' | 'reconnect_disabled';
  readonly error?: unknown;
}

export interface RealtimeEvents<M> {
  open: { readonly attempt: number; readonly protocol: string };
  message: { readonly data: M; readonly raw: unknown };
  close: { readonly code: number; readonly reason: string; readonly willReconnect: boolean };
  reconnecting: { readonly attempt: number; readonly delayMs: number };
  error: { readonly error: unknown };
  closed: RealtimeCloseInfo;
}

/** Where and how to connect (resolved afresh for every attempt). */
export interface RealtimeEndpoint {
  readonly url: string;
  readonly protocols: readonly string[];
}

interface ChannelHooks<M> {
  readonly resolve: () => Promise<RealtimeEndpoint>;
  readonly onFrame?: (data: M) => void;
  /** Close codes after which a reconnect with a fresh credential succeeds (e.g. calling 4001). */
  readonly refreshCodes?: readonly number[];
}

const WS_OPEN = 1;
const STABLE_MS = 10_000;

/** Default terminal close codes: normal closure by the server, protocol / policy errors, auth failures. */
export function defaultTerminalClose(code: number): boolean {
  if ([1000, 1002, 1003, 1007, 1008, 1009, 1010].includes(code)) return true;
  if (code >= 4000 && code <= 4099) return true;
  if (code >= 4400 && code <= 4499) return true;
  return false;
}

type Handler<T> = (ev: T) => void;

/** One logical realtime connection (survives reconnects). */
export class RealtimeConnection<M = unknown> {
  #state: RealtimeState = 'connecting';
  #ws: WebSocketLike | null = null;
  #attempt = 0;
  #failures = 0;
  #openedAt = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #connectTimer: ReturnType<typeof setTimeout> | undefined;
  #userClosed = false;
  readonly #handlers = new Map<keyof RealtimeEvents<M>, Set<Handler<never>>>();
  #resolveClosed!: (info: RealtimeCloseInfo) => void;
  readonly #hooks: ChannelHooks<M>;
  readonly #options: RealtimeOptions;
  readonly #WS: WebSocketConstructorLike;
  /** Resolves once the connection has ended for good. */
  readonly closed: Promise<RealtimeCloseInfo>;

  constructor(hooks: ChannelHooks<M>, options: RealtimeOptions = {}) {
    this.#hooks = hooks;
    this.#options = options;
    const WS = options.WebSocket ?? (globalThis as { WebSocket?: WebSocketConstructorLike }).WebSocket;
    if (!WS) throw new CfMeetConfigurationError('no WebSocket in this runtime: pass options.WebSocket');
    this.#WS = WS;
    this.closed = new Promise((r) => (this.#resolveClosed = r));
    options.signal?.addEventListener('abort', () => this.close(1000, 'aborted'), { once: true });
    // Start on the next tick so callers can attach handlers first.
    queueMicrotask(() => void this.#connect());
  }

  get state(): RealtimeState {
    return this.#state;
  }

  /** Subscribes to an event; returns an unsubscribe function. */
  on<K extends keyof RealtimeEvents<M>>(event: K, handler: Handler<RealtimeEvents<M>[K]>): () => void {
    let set = this.#handlers.get(event);
    if (!set) this.#handlers.set(event, (set = new Set()));
    const h = handler as Handler<never>;
    set.add(h);
    return () => void set.delete(h);
  }

  /** Sends a frame (objects are JSON-encoded). Throws unless the socket is open. */
  send(data: string | ArrayBufferLike | ArrayBufferView | object): void {
    const ws = this.#ws;
    if (!ws || this.#state !== 'open' || ws.readyState !== WS_OPEN) throw new CfMeetError('the realtime connection is not open');
    if (typeof data === 'string' || data instanceof ArrayBuffer || ArrayBuffer.isView(data)) ws.send(data as string);
    else ws.send(JSON.stringify(data));
  }

  /** Closes for good (no reconnect). */
  close(code = 1000, reason = 'client closed'): void {
    if (this.#userClosed || this.#state === 'closed') return;
    this.#userClosed = true;
    if (this.#timer) clearTimeout(this.#timer);
    clearTimeout(this.#connectTimer);
    const ws = this.#ws;
    if (ws && ws.readyState <= WS_OPEN) {
      try {
        ws.close(code, reason);
      } catch {
        /* already closing */
      }
    }
    this.#finish({ code, reason, cause: 'client' });
  }

  #emit<K extends keyof RealtimeEvents<M>>(event: K, payload: RealtimeEvents<M>[K]): void {
    for (const h of [...(this.#handlers.get(event) ?? [])] as Handler<RealtimeEvents<M>[K]>[]) {
      try {
        h(payload);
      } catch (e) {
        if (event !== 'error') this.#emit('error', { error: e });
      }
    }
  }

  #finish(info: RealtimeCloseInfo): void {
    if (this.#state === 'closed') return;
    this.#state = 'closed';
    this.#ws = null;
    this.#emit('closed', info);
    this.#resolveClosed(info);
  }

  async #connect(): Promise<void> {
    if (this.#userClosed) return;
    this.#attempt++;
    let endpoint: RealtimeEndpoint;
    try {
      endpoint = await this.#hooks.resolve();
    } catch (error) {
      if (this.#userClosed) return;
      this.#emit('error', { error });
      const refused =
        error instanceof CfMeetConfigurationError ||
        (error instanceof CfMeetApiError && [400, 401, 403, 404].includes(error.status));
      if (refused) return this.#finish({ code: 4000, reason: 'ticket or credential refused', cause: 'refused', error });
      return this.#retry();
    }
    if (this.#userClosed) return;
    let ws: WebSocketLike;
    try {
      ws = new this.#WS(endpoint.url, [...endpoint.protocols]);
    } catch (error) {
      this.#emit('error', { error });
      return this.#finish({ code: 4000, reason: 'invalid WebSocket endpoint', cause: 'refused', error });
    }
    this.#ws = ws;
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      clearTimeout(this.#connectTimer);
      if (this.#userClosed) return;
      this.#state = 'open';
      this.#openedAt = Date.now();
      this.#emit('open', { attempt: this.#attempt, protocol: ws.protocol });
    };
    ws.onmessage = (ev) => {
      let data: unknown = ev.data;
      if (typeof data === 'string') {
        try {
          data = JSON.parse(data);
        } catch {
          /* a non-JSON text frame is delivered as a string */
        }
      }
      try {
        this.#hooks.onFrame?.(data as M);
      } catch {
        /* bookkeeping must never break delivery */
      }
      this.#emit('message', { data: data as M, raw: ev.data });
    };
    ws.onerror = (error) => {
      this.#emit('error', { error });
      // Some runtimes report a failed handshake with an error but no close event:
      // give the close event a moment, then treat the socket as closed (1006).
      if (this.#openedAt === 0) setTimeout(() => this.#abandon(ws, 'connection failed'), 500);
    };
    ws.onclose = (ev) => this.#handleClose(ws, ev.code, ev.reason);
    const connectTimeout = this.#options.connectTimeoutMs ?? 15_000;
    this.#connectTimer = setTimeout(() => {
      if (this.#ws === ws && this.#openedAt === 0) this.#abandon(ws, 'connect timeout');
    }, connectTimeout);
  }

  /** Gives up on a socket that will not report its own close. */
  #abandon(ws: WebSocketLike, reason: string): void {
    if (this.#ws !== ws) return;
    ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null;
    try {
      ws.close();
    } catch {
      /* not open yet */
    }
    this.#handleClose(ws, 1006, reason);
  }

  #handleClose(ws: WebSocketLike, code: number, reason: string): void {
    if (this.#ws !== ws) return;
    clearTimeout(this.#connectTimer);
    this.#ws = null;
    const wasOpen = this.#openedAt > 0;
    const stable = wasOpen && Date.now() - this.#openedAt >= STABLE_MS;
    this.#openedAt = 0;
    if (this.#userClosed) return;
    const refresh = this.#hooks.refreshCodes?.includes(code) ?? false;
    const terminal = !refresh && (this.#options.isTerminalClose ?? defaultTerminalClose)(code, reason);
    const disabled = this.#options.reconnect === false;
    this.#emit('close', { code, reason, willReconnect: !terminal && !disabled });
    if (terminal) return this.#finish({ code, reason, cause: 'terminal_close' });
    if (disabled) return this.#finish({ code, reason, cause: 'reconnect_disabled' });
    if (stable || refresh) this.#failures = 0;
    this.#retry();
  }

  #retry(): void {
    if (this.#userClosed) return;
    const r = this.#options.reconnect;
    if (r === false) return this.#finish({ code: 1006, reason: 'connection failed', cause: 'reconnect_disabled' });
    const max = r?.maxAttempts ?? 10;
    if (this.#failures >= max) return this.#finish({ code: 1006, reason: 'reconnect attempts exhausted', cause: 'reconnect_exhausted' });
    const initial = r?.initialDelayMs ?? 500;
    const cap = r?.maxDelayMs ?? 30_000;
    const d = Math.min(cap, initial * 2 ** this.#failures);
    const delayMs = Math.round(d / 2 + Math.random() * (d / 2));
    this.#failures++;
    this.#state = 'reconnecting';
    this.#emit('reconnecting', { attempt: this.#attempt + 1, delayMs });
    if (r?.sleep) {
      // #connect() returns at once when close() was called during the wait.
      r.sleep(delayMs).then(
        () => void this.#connect(),
        (error: unknown) => this.#finish({ code: 1006, reason: 'reconnect wait failed', cause: 'reconnect_exhausted', error }),
      );
      return;
    }
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.#connect();
    }, delayMs);
  }
}

function spec(domain: string, name: string): OperationSpec {
  const s = OPERATIONS.find((o) => o.domain === domain && o.name === name);
  if (!s) throw new CfMeetConfigurationError(`operation ${domain}.${name} is not in the catalogue`);
  return s;
}

/** Subprotocol entries must be RFC 7230 tokens; tickets / credentials are checked before use. */
const SUBPROTOCOL_TOKEN_RE = /^[A-Za-z0-9!#$%&'*+.^_`|~-]{1,4096}$/;

export interface MessagingRealtimeOptions extends RealtimeOptions {
  /** Client-token credentials (defaults to the client's auth, which must be a clientToken). */
  readonly auth?: CfMeetAuth;
}
export interface MeetingsRealtimeOptions extends RealtimeOptions {
  /** User session or participant token (defaults to the client's auth). */
  readonly auth?: CfMeetAuth;
  /** Resume after this conference sequence on the first connect (later reconnects resume after the last event received). */
  readonly cursor?: number;
}
export interface CallingSignalingOptions extends RealtimeOptions {
  /**
   * The endpoint's signaling credential (cfsig_...). Prefer a provider function: the
   * server closes with 4001 when the credential expires, and a provider lets the SDK
   * reconnect with a fresh one. Defaults to the client's auth when it is a signalingCredential.
   */
  readonly credential?: TokenSource;
}

/** Realtime helpers bound to a client (`client.realtime`). */
export class RealtimeClient {
  readonly #client: CfMeetClient;
  readonly #wsBase: string;
  readonly #auth: CfMeetAuth | null;
  readonly #allowPlatformInBrowser: boolean;

  constructor(client: CfMeetClient, baseUrl: string, auth: CfMeetAuth | null, allowPlatformInBrowser: boolean) {
    this.#client = client;
    this.#wsBase = baseUrl.replace(/^http/, 'ws');
    this.#auth = auth;
    this.#allowPlatformInBrowser = allowPlatformInBrowser;
  }

  /** Messaging realtime (`GET /v1/realtime`): a fresh single-use ticket per connect, sent as a subprotocol. */
  messaging<M = unknown>(options: MessagingRealtimeOptions = {}): RealtimeConnection<M> {
    const auth = options.auth ?? this.#auth;
    if (auth?.type !== 'clientToken') {
      throw new CfMeetConfigurationError('messaging realtime requires a clientToken (POST /v1/realtime/tickets accepts CF-Client-Token only)');
    }
    const issue = spec('messaging', 'issueRealtimeTicket');
    const socket = spec('messaging', 'connectRealtime');
    return new RealtimeConnection<M>(
      {
        resolve: async () => {
          const res = await this.#client.request<Messaging.RealtimeTicket>(issue, {}, { auth, signal: options.signal });
          const ticket = res.data?.ticket;
          if (typeof ticket !== 'string' || !SUBPROTOCOL_TOKEN_RE.test(`cfmeet.ticket.${ticket}`)) {
            throw new CfMeetError('the realtime ticket response is malformed');
          }
          return { url: this.#wsBase + socket.path, protocols: ['cfmeet.messaging.v1', `cfmeet.ticket.${ticket}`] };
        },
      },
      options,
    );
  }

  /**
   * Meetings conference realtime (`GET /api/v2/conferences/{id}/realtime`): a fresh
   * one-shot ticket per connect, in the query as the contract requires; reconnects
   * resume after the last delivered event sequence.
   */
  meetings<M = unknown>(conferenceId: string, options: MeetingsRealtimeOptions = {}): RealtimeConnection<M> {
    const auth = options.auth ?? this.#auth;
    if (auth?.type !== 'userSession' && auth?.type !== 'participantToken') {
      throw new CfMeetConfigurationError('meetings realtime requires a userSession or participantToken');
    }
    const issue = spec('meetings', 'v2RealtimeTicket');
    const socket = spec('meetings', 'v2RealtimeSubscribe');
    const path = buildPath(socket, { id: conferenceId });
    let cursor: number | null = options.cursor ?? null;
    return new RealtimeConnection<M>(
      {
        resolve: async () => {
          const res = await this.#client.request<Meetings.RealtimeTicketResponse>(
            issue,
            { path: { id: conferenceId } },
            { auth, signal: options.signal },
          );
          const ticket = res.data?.ticket;
          if (typeof ticket !== 'string' || ticket === '') throw new CfMeetError('the realtime ticket response is malformed');
          const q = new URLSearchParams({ ticket });
          if (cursor !== null) q.set('cursor', String(cursor));
          return { url: `${this.#wsBase}${path}?${q}`, protocols: [] };
        },
        onFrame: (frame) => {
          const f = frame as { type?: unknown; transient?: unknown; cursor?: unknown; event?: { sequence?: unknown } } | null;
          if (!f || typeof f !== 'object') return;
          if (f.type === 'event' && f.transient !== true && typeof f.event?.sequence === 'number') cursor = f.event.sequence;
          else if (f.type === 'resync_required' && typeof f.cursor === 'number') cursor = f.cursor;
        },
      },
      options,
    );
  }

  /** Calling signaling (`GET /v1/calling/signaling`): the credential is sent as the `bearer.` subprotocol entry. */
  callingSignaling<M = unknown>(options: CallingSignalingOptions = {}): RealtimeConnection<M> {
    const source: TokenSource | undefined =
      options.credential ?? (this.#auth?.type === 'signalingCredential' ? this.#auth.credential : undefined);
    if (source === undefined) throw new CfMeetConfigurationError('calling signaling requires a signaling credential (cfsig_...)');
    const auth: CfMeetAuth = { type: 'signalingCredential', credential: source };
    const socket = spec('calling', 'openSignalingChannel');
    return new RealtimeConnection<M>(
      {
        resolve: async () => {
          const credential = await resolveToken(auth, this.#allowPlatformInBrowser);
          if (!SUBPROTOCOL_TOKEN_RE.test(`bearer.${credential}`)) {
            throw new CfMeetConfigurationError('the signaling credential contains characters a WebSocket subprotocol cannot carry');
          }
          return { url: this.#wsBase + socket.path, protocols: ['cfmeet.signaling.v1', `bearer.${credential}`] };
        },
        // 4001: the credential expired; a provider can supply a fresh one.
        refreshCodes: typeof source === 'function' ? [4001] : [],
      },
      options,
    );
  }
}

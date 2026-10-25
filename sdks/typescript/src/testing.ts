// Deterministic test fakes for applications built on the CF Meet platform SDK
// (Part VI §52.3) — `@cfmeet/platform-sdk/testing`.
//
// Unit and integration tests of YOUR code run against the real CfMeetClient and realtime
// helpers without a network, live CF Meet services or telecom providers, and without
// real time passing:
//
//   const fake = createFakeFetch();
//   const clock = createFakeClock();
//   const sockets = createFakeWebSocket();
//   const client = new CfMeetClient({ baseUrl: 'https://api.cfmeet.test', auth, fetch: fake.fetch, retry: { sleep: clock.sleep } });
//   fake.on('sms', 'getMessage', FakeResponse.json({ id: 'm1' }));
//   client.realtime.messaging({ WebSocket: sockets.WebSocket, reconnect: { sleep: clock.sleep } });
//
// Not imported by the main entry (and side-effect free): it never ships in an
// application bundle that does not import it. Uses only web-standard globals
// (Response, Headers, URL, DOMException).

import type { QuotaInfo } from './errors.js';
import { CfMeetConfigurationError } from './errors.js';
import type { DomainOperations } from './generated/models.js';
import { OPERATIONS, type OperationSpec, type PaginationSpec } from './generated/operations.js';
import type { WebSocketLike } from './realtime.js';

// ---------------------------------------------------------------------------------------
// Replies

/** Headers and metadata of a fake HTTP answer. */
export interface FakeResponseInit {
  readonly status?: number;
  /** Extra / overriding headers (any case). */
  readonly headers?: Readonly<Record<string, string>>;
  /** `x-request-id` (default: the fake's deterministic id of the request). */
  readonly requestId?: string;
  readonly correlationId?: string;
  /** Sends `idempotent-replayed: true`. */
  readonly idempotentReplayed?: boolean;
  /** Sent as the `x-cf-quota-*` headers. */
  readonly quota?: Partial<QuotaInfo>;
  /** `Retry-After`: seconds (number) or an HTTP date (string). */
  readonly retryAfter?: number | string;
}

/** What the fake does with one request. Build it with `FakeResponse`. */
export type FakeReply =
  | {
      readonly type: 'response';
      readonly status: number;
      readonly json?: unknown;
      readonly bytes?: Uint8Array;
      readonly contentType?: string;
      readonly envelope?: { readonly code: string; readonly message: string };
      readonly init: FakeResponseInit;
    }
  | { readonly type: 'networkError'; readonly requestSent: boolean }
  | { readonly type: 'timeout' }
  | { readonly type: 'hang' };

type Domain = keyof DomainOperations;
type OperationName<D extends Domain> = Extract<keyof DomainOperations[D], string>;

/** The public API Gateway error code for a status (contract `cfmeet.api-gateway.common` ErrorCode). */
export function fakeErrorCodeFor(status: number): string {
  const codes: Record<number, string> = {
    400: 'VALIDATION_ERROR',
    401: 'AUTHENTICATION_FAILED',
    403: 'INSUFFICIENT_SCOPE',
    404: 'NOT_FOUND',
    405: 'METHOD_NOT_ALLOWED',
    409: 'CONFLICT',
    412: 'PRECONDITION_FAILED',
    413: 'PAYLOAD_TOO_LARGE',
    414: 'URI_TOO_LONG',
    429: 'RATE_LIMITED',
    431: 'HEADERS_TOO_LARGE',
    500: 'INTERNAL_ERROR',
    502: 'UPSTREAM_UNAVAILABLE',
    503: 'SERVICE_UNAVAILABLE',
    504: 'UPSTREAM_TIMEOUT',
  };
  return codes[status] ?? `HTTP_${status}`;
}

/** The contract error envelope `{"error": {"code", "message", "requestId"}}`. */
export function fakeErrorEnvelope(
  code: string,
  options: { readonly message?: string; readonly requestId?: string | null } = {},
): { readonly error: { readonly code: string; readonly message: string; readonly requestId: string | null } } {
  return { error: { code, message: options.message ?? `fake ${code}`, requestId: options.requestId ?? null } };
}

/** One page of a cursor-paginated operation, shaped by its pagination spec. */
export function fakePageBody<D extends Domain>(
  domain: D,
  name: OperationName<D>,
  items: readonly unknown[],
  options: { readonly nextCursor?: string | null; readonly extra?: Readonly<Record<string, unknown>> } = {},
): Record<string, unknown> {
  const p = paginationOf(domain, name);
  return { ...options.extra, [p.itemsField]: items, [p.nextField]: options.nextCursor ?? null };
}

/** The `x-cf-quota-*` headers carrying a quota state. */
export function fakeQuotaHeaders(quota: Partial<QuotaInfo>): Record<string, string> {
  const out: Record<string, string> = {};
  const set = (name: string, v: string | number | null | undefined) => {
    if (v !== null && v !== undefined) out[name] = String(v);
  };
  set('x-cf-quota-limit-minute', quota.limitPerMinute);
  set('x-cf-quota-remaining-minute', quota.remainingThisMinute);
  set('x-cf-quota-reset', quota.resetSeconds);
  set('x-cf-quota-limit-day', quota.limitPerDay);
  set('x-cf-quota-window', quota.window);
  set('x-cf-quota-level', quota.level);
  return out;
}

/** Builders of fake replies. */
export const FakeResponse = {
  /** A JSON answer (status 200 unless given). The type parameter lets a test check the body's shape. */
  json<T = unknown>(body: T, init: FakeResponseInit = {}): FakeReply {
    return { type: 'response', status: init.status ?? 200, json: body, init };
  },
  /** An answer without a body (204 unless given). */
  empty(init: FakeResponseInit = {}): FakeReply {
    return { type: 'response', status: init.status ?? 204, init };
  },
  /** Raw bytes (e.g. a media download). */
  bytes(bytes: Uint8Array, contentType = 'application/octet-stream', init: FakeResponseInit = {}): FakeReply {
    return { type: 'response', status: init.status ?? 200, bytes, contentType, init };
  },
  /** A contract error envelope; `code` defaults to the gateway code of the status, `requestId` to the request's id. */
  error(status: number, options: FakeResponseInit & { readonly code?: string; readonly message?: string } = {}): FakeReply {
    const code = options.code ?? fakeErrorCodeFor(status);
    return { type: 'response', status, envelope: { code, message: options.message ?? `fake ${code}` }, init: options };
  },
  /** One page of the cursor-paginated operation `domain.name` (throws for an operation without pagination). */
  page<D extends Domain>(
    domain: D,
    name: OperationName<D>,
    items: readonly unknown[],
    options: FakeResponseInit & { readonly nextCursor?: string | null; readonly extra?: Readonly<Record<string, unknown>> } = {},
  ): FakeReply {
    return FakeResponse.json(fakePageBody(domain, name, items, options), options);
  },
  /**
   * The connection failed. `requestSent: false` — before any byte reached the server
   * (connection refused); true (default) — after (reset): a NEVER operation's outcome is unknown.
   */
  networkError(options: { readonly requestSent?: boolean } = {}): FakeReply {
    return { type: 'networkError', requestSent: options.requestSent ?? true };
  },
  /** The attempt timed out (reported at once: no time passes). */
  timeout(): FakeReply {
    return { type: 'timeout' };
  },
  /** Never answers; settles only when the request is aborted (caller signal or the client's real timeout). */
  hang(): FakeReply {
    return { type: 'hang' };
  },
} as const;

// ---------------------------------------------------------------------------------------
// Fake fetch

/** One request the fake received. */
export interface RecordedRequest {
  /** 1-based position among every request the fake received. */
  readonly index: number;
  /** The id the fake answered with (`x-request-id`, the envelope's `requestId`). */
  readonly requestId: string;
  readonly method: string;
  readonly url: string;
  /** The URL path without the fake's pathPrefix. */
  readonly path: string;
  /** Query parameters (first value of each); all values in `searchParams`. */
  readonly query: Readonly<Record<string, string>>;
  readonly searchParams: URLSearchParams;
  /** Request headers, names lower-case (including authorization, idempotency-key). */
  readonly headers: Readonly<Record<string, string>>;
  readonly idempotencyKey: string | null;
  /** The body decoded: JSON when it parses, otherwise text; undefined when there is none. */
  readonly body: unknown;
  readonly rawBody: string | null;
  /** The catalogue operation the method and path belong to, if any. */
  readonly operation: OperationSpec | null;
  /** `domain.name` of `operation`. */
  readonly operationName: string | null;
  /** Decoded path parameters of `operation`. */
  readonly pathParams: Readonly<Record<string, string>>;
}

export type FakeResponder = (request: RecordedRequest) => FakeReply | Promise<FakeReply>;

/** The script of one route: replies used in order, the last one repeating. */
export interface FakeRoute {
  /** `sms.getMessage` or `GET /v1/...`. */
  readonly description: string;
  /** The requests this route answered. */
  readonly requests: readonly RecordedRequest[];
  readonly callCount: number;
  /** Whether every scripted reply has been used at least once. */
  readonly exhausted: boolean;
  thenReply(...replies: FakeReply[]): FakeRoute;
  thenReplyWith(responder: FakeResponder): FakeRoute;
}

export interface FakeFetchOptions {
  /** A base-URL path prefix stripped before matching (`/gw` for `https://host/gw`). */
  readonly pathPrefix?: string;
  /** The request id of the n-th request (default `req_fake_000001`, ...). */
  readonly requestId?: (n: number) => string;
}

export interface FakeFetch {
  /** Pass as `new CfMeetClient({ fetch: fake.fetch })`. */
  readonly fetch: (input: string | URL | { readonly url: string }, init?: RequestInit) => Promise<Response>;
  /** Every request received, in order. */
  readonly requests: readonly RecordedRequest[];
  /** Requests no route answered (they got the default 404 NOT_FOUND envelope). */
  readonly unmatched: readonly RecordedRequest[];
  /**
   * Scripts the catalogue operation `domain.name` (checked at compile time and at run
   * time: an unknown name throws CfMeetConfigurationError). Registering it again
   * replaces the script.
   */
  on<D extends Domain>(domain: D, name: OperationName<D>, ...replies: FakeReply[]): FakeRoute;
  /** Scripts a method and path (literal, or a template with `{param}`); precedes `on`, latest wins. */
  onRequest(method: string, path: string, ...replies: FakeReply[]): FakeRoute;
  /**
   * Serves `pages` of the cursor-paginated operation in order: no cursor → page 1 with
   * nextCursor `fake_cursor_1`, that cursor → page 2, ...; the last page has none; an
   * unknown cursor answers 400 VALIDATION_ERROR.
   */
  onPages<D extends Domain>(domain: D, name: OperationName<D>, pages: readonly (readonly unknown[])[]): FakeRoute;
  /** The requests of the catalogue operation `domain.name`. */
  requestsFor<D extends Domain>(domain: D, name: OperationName<D>): RecordedRequest[];
  /** Forgets routes and requests; request ids restart at 1. */
  reset(): void;
}

/** `req_fake_000001`, `req_fake_000002`, ... */
export function defaultFakeRequestId(n: number): string {
  return `req_fake_${String(n).padStart(6, '0')}`;
}

function restOperation(domain: string, name: string): OperationSpec {
  const spec = OPERATIONS.find((o) => o.domain === domain && o.name === name);
  if (!spec) {
    const known = OPERATIONS.filter((o) => o.domain === domain);
    if (known.length === 0) {
      const domains = [...new Set(OPERATIONS.map((o) => o.domain))].sort();
      throw new CfMeetConfigurationError(`unknown API domain ${domain} (known: ${domains.join(', ')})`);
    }
    const close = known.map((o) => o.name).filter((n) => distance(n.toLowerCase(), name.toLowerCase()) <= 3);
    throw new CfMeetConfigurationError(
      `unknown operation ${domain}.${name}${close.length ? ` (did you mean ${close.slice(0, 3).join(', ')}?)` : ''}`,
    );
  }
  if (spec.protocol !== 'REST') {
    throw new CfMeetConfigurationError(`${domain}.${name} is a WebSocket channel: script it with createFakeWebSocket()`);
  }
  return spec;
}

function paginationOf(domain: string, name: string): PaginationSpec {
  const p = restOperation(domain, name).pagination;
  if (!p) throw new CfMeetConfigurationError(`${domain}.${name} is not cursor-paginated`);
  return p;
}

function distance(a: string, b: string): number {
  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const cur = [i];
    for (let j = 1; j <= b.length; j++) {
      cur[j] = Math.min(prev[j]! + 1, cur[j - 1]! + 1, prev[j - 1]! + (a[i - 1] === b[j - 1] ? 0 : 1));
    }
    prev = cur;
  }
  return prev[b.length]!;
}

interface Template {
  readonly names: readonly string[];
  readonly literals: number;
  readonly regex: RegExp;
}

function template(path: string): Template {
  const names = [...path.matchAll(/\{([^}/]+)\}/g)].map((m) => m[1]!);
  const escaped = path.split(/\{[^}/]+\}/).map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  return {
    names,
    literals: path.split('/').filter((s) => s !== '' && !s.includes('{')).length,
    regex: new RegExp(`^${escaped.join('([^/]+)')}$`),
  };
}

function matchTemplate(t: Template, path: string): Record<string, string> | null {
  const m = t.regex.exec(path);
  if (!m) return null;
  const out: Record<string, string> = {};
  t.names.forEach((n, i) => (out[n] = decodeURIComponent(m[i + 1]!)));
  return out;
}

let catalogue: { spec: OperationSpec; t: Template }[] | null = null;
function restCatalogue(): { spec: OperationSpec; t: Template }[] {
  return (catalogue ??= OPERATIONS.filter((o) => o.protocol === 'REST').map((spec) => ({ spec, t: template(spec.path) })));
}

class Route implements FakeRoute {
  readonly requests: RecordedRequest[] = [];
  readonly #script: FakeResponder[] = [];
  #used = 0;
  constructor(readonly description: string) {}
  get callCount(): number {
    return this.requests.length;
  }
  get exhausted(): boolean {
    return this.#used >= this.#script.length;
  }
  thenReply(...replies: FakeReply[]): FakeRoute {
    for (const r of replies) this.#script.push(() => r);
    return this;
  }
  thenReplyWith(responder: FakeResponder): FakeRoute {
    this.#script.push(responder);
    return this;
  }
  next(): FakeResponder | null {
    if (this.#script.length === 0) return null;
    const r = this.#script[Math.min(this.#used, this.#script.length - 1)]!;
    this.#used++;
    return r;
  }
}

function bodyText(body: unknown): string | null {
  if (body === undefined || body === null) return null;
  if (typeof body === 'string') return body;
  if (body instanceof ArrayBuffer) return new TextDecoder().decode(body);
  if (ArrayBuffer.isView(body)) return new TextDecoder().decode(body);
  if (body instanceof URLSearchParams) return body.toString();
  return String(body);
}

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException('The operation was aborted.', 'AbortError');
}

/** A deterministic in-memory `fetch` for `CfMeetClient`. No network, no timers. */
export function createFakeFetch(options: FakeFetchOptions = {}): FakeFetch {
  const prefix = options.pathPrefix ?? '';
  const requestId = options.requestId ?? defaultFakeRequestId;
  const operationRoutes = new Map<string, Route>();
  const requestRoutes: { method: string; t: Template; route: Route }[] = [];
  const requests: RecordedRequest[] = [];
  const unmatched: RecordedRequest[] = [];

  function respond(reply: Extract<FakeReply, { type: 'response' }>, id: string): Response {
    const init = reply.init;
    const reqId = init.requestId ?? id;
    const headers: Record<string, string> = { 'x-request-id': reqId };
    let body: BodyInit | null = null;
    if (reply.envelope) {
      body = JSON.stringify(fakeErrorEnvelope(reply.envelope.code, { message: reply.envelope.message, requestId: reqId }));
      headers['content-type'] = 'application/json';
      headers['cache-control'] = 'no-store';
    } else if (reply.bytes) {
      body = reply.bytes;
      headers['content-type'] = reply.contentType ?? 'application/octet-stream';
    } else if (reply.json !== undefined) {
      body = JSON.stringify(reply.json);
      headers['content-type'] = 'application/json';
    }
    if (init.correlationId) headers['x-correlation-id'] = init.correlationId;
    if (init.idempotentReplayed) headers['idempotent-replayed'] = 'true';
    if (init.quota) Object.assign(headers, fakeQuotaHeaders(init.quota));
    if (init.retryAfter !== undefined) headers['retry-after'] = String(init.retryAfter);
    for (const [k, v] of Object.entries(init.headers ?? {})) headers[k.toLowerCase()] = v;
    const nullBody = reply.status === 204 || reply.status === 205 || reply.status === 304;
    return new Response(nullBody ? null : body, { status: reply.status, headers });
  }

  async function fakeFetch(input: string | URL | { readonly url: string }, init: RequestInit = {}): Promise<Response> {
    const signal = init.signal ?? undefined;
    if (signal?.aborted) throw abortReason(signal);
    const url = new URL(typeof input === 'string' ? input : input instanceof URL ? input.href : input.url);
    const method = (init.method ?? 'GET').toUpperCase();
    let path = url.pathname;
    if (prefix && path.startsWith(prefix)) path = path.slice(prefix.length) || '/';

    let operation: OperationSpec | null = null;
    let pathParams: Record<string, string> = {};
    let best = -1;
    for (const { spec, t } of restCatalogue()) {
      if (spec.method !== method || t.literals <= best) continue;
      const m = matchTemplate(t, path);
      if (m) {
        operation = spec;
        pathParams = m;
        best = t.literals;
      }
    }
    let route: Route | undefined;
    for (let i = requestRoutes.length - 1; i >= 0; i--) {
      const r = requestRoutes[i]!;
      if (r.method === method && r.t.regex.test(path)) {
        route = r.route;
        break;
      }
    }
    if (!route && operation) route = operationRoutes.get(`${operation.domain}.${operation.name}`);

    const headers: Record<string, string> = {};
    new Headers(init.headers).forEach((v, k) => (headers[k.toLowerCase()] = v));
    const rawBody = bodyText(init.body);
    let body: unknown = undefined;
    if (rawBody !== null) {
      try {
        body = JSON.parse(rawBody);
      } catch {
        body = rawBody;
      }
    }
    const query: Record<string, string> = {};
    url.searchParams.forEach((v, k) => {
      if (!(k in query)) query[k] = v;
    });
    const n = requests.length + 1;
    const rec: RecordedRequest = Object.freeze({
      index: n,
      requestId: requestId(n),
      method,
      url: url.href,
      path,
      query: Object.freeze(query),
      searchParams: url.searchParams,
      headers: Object.freeze(headers),
      idempotencyKey: headers['idempotency-key'] ?? null,
      body,
      rawBody,
      operation,
      operationName: operation ? `${operation.domain}.${operation.name}` : null,
      pathParams: Object.freeze(pathParams),
    });
    requests.push(rec);
    route?.requests.push(rec);

    const responder = route?.next() ?? null;
    let reply: FakeReply;
    if (!responder) {
      unmatched.push(rec);
      reply = FakeResponse.error(404, {
        code: 'NOT_FOUND',
        message: route ? `no reply scripted for ${route.description}` : `no fake response registered for ${method} ${path}`,
      });
    } else {
      reply = await responder(rec);
    }
    if (signal?.aborted) throw abortReason(signal);
    switch (reply.type) {
      case 'networkError':
        throw new TypeError('fetch failed (fake network error)', {
          cause: { code: reply.requestSent ? 'ECONNRESET' : 'ECONNREFUSED' },
        });
      case 'timeout':
        throw new DOMException('fake timeout', 'TimeoutError');
      case 'hang':
        return new Promise<Response>((_, reject) => {
          signal?.addEventListener('abort', () => reject(abortReason(signal)), { once: true });
        });
      case 'response':
        return respond(reply, rec.requestId);
    }
  }

  const fake: FakeFetch = {
    fetch: fakeFetch,
    requests,
    unmatched,
    on(domain, name, ...replies) {
      const spec = restOperation(domain, name);
      const route = new Route(`${spec.domain}.${spec.name}`);
      route.thenReply(...replies);
      operationRoutes.set(route.description, route);
      return route;
    },
    onRequest(method, path, ...replies) {
      const m = method.toUpperCase();
      if (!['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].includes(m)) throw new CfMeetConfigurationError(`not an HTTP method: ${method}`);
      if (!path.startsWith('/') || path.includes('?')) throw new CfMeetConfigurationError('path must start with / and carry no query');
      const route = new Route(`${m} ${path}`);
      route.thenReply(...replies);
      requestRoutes.push({ method: m, t: template(path), route });
      return route;
    },
    onPages(domain, name, pages) {
      const p = paginationOf(domain, name);
      const cursorOf = (i: number) => `fake_cursor_${i}`;
      return fake.on(domain, name).thenReplyWith((req) => {
        const cursor = req.query[p.cursorParam];
        let i = 0;
        if (cursor !== undefined) {
          i = Number(cursor.replace('fake_cursor_', ''));
          if (cursor !== cursorOf(i) || !(i >= 1 && i < pages.length)) {
            return FakeResponse.error(400, { code: 'VALIDATION_ERROR', message: 'unknown cursor' });
          }
        }
        return FakeResponse.page(domain, name, pages[i] ?? [], { nextCursor: i + 1 < pages.length ? cursorOf(i + 1) : null });
      });
    },
    requestsFor(domain, name) {
      const spec = restOperation(domain, name);
      return requests.filter((r) => r.operation === spec);
    },
    reset() {
      operationRoutes.clear();
      requestRoutes.length = 0;
      requests.length = 0;
      unmatched.length = 0;
    },
  };
  return fake;
}

// ---------------------------------------------------------------------------------------
// Fake clock

/** Records the SDK's waits (retry backoff, Retry-After, reconnect backoff) instead of sleeping. */
export interface FakeClock {
  /** Every delay requested (ms), in order. */
  readonly sleeps: readonly number[];
  /** Total virtual time slept (ms). */
  readonly elapsedMs: number;
  /** Virtual now (ms since the epoch): the start plus elapsedMs. */
  now(): number;
  /**
   * Pass as `retry: { sleep }` and `reconnect: { sleep }`: records the delay, advances
   * virtual time and resolves on the next macrotask (no real wait); rejects with the
   * signal's reason when aborted.
   */
  readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
}

export function createFakeClock(options: { readonly start?: number } = {}): FakeClock {
  const start = options.start ?? Date.UTC(2026, 0, 1);
  const sleeps: number[] = [];
  let elapsed = 0;
  return {
    sleeps,
    get elapsedMs() {
      return elapsed;
    },
    now: () => start + elapsed,
    sleep: (ms, signal) => {
      if (signal?.aborted) return Promise.reject(abortReason(signal));
      sleeps.push(ms);
      elapsed += ms;
      return new Promise((resolve, reject) =>
        setTimeout(() => (signal?.aborted ? reject(abortReason(signal)) : resolve()), 0),
      );
    },
  };
}

// ---------------------------------------------------------------------------------------
// Fake WebSocket

interface SocketHub {
  readonly selectProtocol: (protocols: readonly string[]) => string;
  readonly autoOpen: boolean;
  takeRefusal(): number | null;
  added(ws: FakeWebSocket): void;
  opened(ws: FakeWebSocket): void;
}

type Frame = string | ArrayBufferLike | ArrayBufferView;

/** One fake WebSocket the SDK opened (see `createFakeWebSocket`). */
export class FakeWebSocket implements WebSocketLike {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readonly url: string;
  /** Offered subprotocols, in order. */
  readonly protocols: readonly string[];
  /** 1-based connection attempt number. */
  readonly index: number;
  readyState = 0;
  protocol = '';
  binaryType = 'blob';
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: { code: number; reason: string; wasClean?: boolean }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  /** Frames the client sent. */
  readonly sent: Frame[] = [];
  /** Set when the client closed the socket. */
  closedByClient: { readonly code: number | undefined; readonly reason: string | undefined } | null = null;
  readonly #hub: SocketHub;

  constructor(url: string, protocols: string | string[] | undefined, hub: SocketHub, index: number) {
    this.url = url;
    this.protocols = protocols === undefined ? [] : typeof protocols === 'string' ? [protocols] : [...protocols];
    this.#hub = hub;
    this.index = index;
    hub.added(this);
    const refusal = hub.takeRefusal();
    queueMicrotask(() => {
      if (this.readyState !== 0) return;
      if (refusal !== null) return this.#closed(refusal, 'refused by the fake server', false);
      if (hub.autoOpen) this.open();
    });
  }

  /** The ticket of a `cfmeet.ticket.<ticket>` subprotocol (Messaging). */
  get ticketSubprotocol(): string | null {
    const p = this.protocols.find((x) => x.startsWith('cfmeet.ticket.'));
    return p ? p.slice('cfmeet.ticket.'.length) : null;
  }
  /** The `ticket` query parameter (Meetings). */
  get queryTicket(): string | null {
    return new URL(this.url).searchParams.get('ticket');
  }
  /** The ticket wherever the channel carries it. */
  get ticket(): string | null {
    return this.ticketSubprotocol ?? this.queryTicket;
  }
  /** The credential of a `bearer.<credential>` subprotocol (Calling signaling). */
  get bearer(): string | null {
    const p = this.protocols.find((x) => x.startsWith('bearer.'));
    return p ? p.slice('bearer.'.length) : null;
  }
  /** `sent` with JSON text frames decoded. */
  sentJson(): unknown[] {
    return this.sent.map((f) => {
      if (typeof f !== 'string') return f;
      try {
        return JSON.parse(f);
      } catch {
        return f;
      }
    });
  }

  /** Completes the handshake (automatic unless `autoOpen: false`). */
  open(protocol?: string): void {
    if (this.readyState !== 0) throw new Error('the fake socket is not connecting');
    this.readyState = 1;
    this.protocol = protocol ?? this.#hub.selectProtocol(this.protocols);
    this.onopen?.({});
    this.#hub.opened(this);
  }

  /** Delivers a frame: objects as JSON text, strings as text, binary as is. */
  message(data: unknown): void {
    if (this.readyState !== 1) throw new Error('the fake socket is not open');
    const frame =
      typeof data === 'string' || data instanceof ArrayBuffer || ArrayBuffer.isView(data) ? data : JSON.stringify(data);
    this.onmessage?.({ data: frame });
  }

  /** The server closes the connection with `code` (e.g. 1011 → reconnect, 4401 → stop). */
  serverClose(code: number, reason = ''): void {
    if (this.readyState === 3) throw new Error('the fake socket is closed');
    this.#closed(code, reason, true);
  }

  /** The connection drops without a close frame (an error, then close 1006). */
  drop(): void {
    if (this.readyState === 3) throw new Error('the fake socket is closed');
    this.onerror?.({ type: 'error' });
    this.#closed(1006, '', false);
  }

  send(data: Frame): void {
    if (this.readyState !== 1) throw new Error('the fake socket is not open');
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    if (this.readyState >= 2) return;
    this.closedByClient = { code, reason };
    this.readyState = 2;
    queueMicrotask(() => this.#closed(code ?? 1005, reason ?? '', true));
  }

  #closed(code: number, reason: string, wasClean: boolean): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.({ code, reason, wasClean });
  }
}

export interface FakeWebSocketOptions {
  /** Open each socket on the next microtask (default true); otherwise call `socket.open()`. */
  readonly autoOpen?: boolean;
  /** The subprotocol the fake server selects (default: the first offered that is neither a ticket nor a credential). */
  readonly selectProtocol?: (protocols: readonly string[]) => string;
}

export interface FakeWebSocketServer {
  /** Pass as the realtime helpers' `WebSocket` option. */
  readonly WebSocket: new (url: string, protocols?: string | string[]) => FakeWebSocket;
  /** Every socket constructed (refused ones included), in order. */
  readonly sockets: readonly FakeWebSocket[];
  /** The latest socket (throws when none). */
  readonly last: FakeWebSocket;
  /** Resolves with the n-th (1-based) socket that OPENED, once the client saw it open. */
  connection(n: number): Promise<FakeWebSocket>;
  /** The next socket closes with `code` (default 1006, retried; 1008 / 4xxx are terminal) before opening. */
  refuseNext(code?: number): void;
}

/** A fake WebSocket constructor for `client.realtime.*({ WebSocket })`. */
export function createFakeWebSocket(options: FakeWebSocketOptions = {}): FakeWebSocketServer {
  const sockets: FakeWebSocket[] = [];
  const open: FakeWebSocket[] = [];
  const waiters = new Map<number, (ws: FakeWebSocket) => void>();
  const refusals: number[] = [];
  const hub: SocketHub = {
    autoOpen: options.autoOpen ?? true,
    selectProtocol:
      options.selectProtocol ?? ((ps) => ps.find((p) => !p.startsWith('cfmeet.ticket.') && !p.startsWith('bearer.')) ?? ''),
    takeRefusal: () => refusals.shift() ?? null,
    added: (ws) => void sockets.push(ws),
    opened: (ws) => {
      open.push(ws);
      waiters.get(open.length)?.(ws);
      waiters.delete(open.length);
    },
  };
  class BoundFakeWebSocket extends FakeWebSocket {
    constructor(url: string, protocols?: string | string[]) {
      super(url, protocols, hub, sockets.length + 1);
    }
  }
  return {
    WebSocket: BoundFakeWebSocket,
    sockets,
    get last() {
      const s = sockets[sockets.length - 1];
      if (!s) throw new Error('no fake WebSocket was opened');
      return s;
    },
    connection(n) {
      if (!(n >= 1)) return Promise.reject(new RangeError('connections are numbered from 1'));
      const ws = open[n - 1];
      if (ws) return Promise.resolve(ws);
      return new Promise((resolve) => waiters.set(n, resolve));
    },
    refuseNext(code = 1006) {
      refusals.push(code);
    },
  };
}

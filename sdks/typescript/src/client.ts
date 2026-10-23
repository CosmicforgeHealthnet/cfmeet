// The CF Meet platform API client.

import {
  AUTH_MODES,
  BROWSER_ESCAPE_HATCH,
  authHeaders,
  isBrowserLike,
  validateAuth,
  type CfMeetAuth,
} from './auth.js';
import {
  CfMeetAbortError,
  CfMeetConfigurationError,
  CfMeetError,
  CfMeetNetworkError,
  CfMeetTimeoutError,
  UnknownOutcomeError,
  apiErrorFor,
  operationRef,
  type CfMeetApiError,
  type OperationRef,
  type QuotaInfo,
  type UnknownOutcomeReason,
} from './errors.js';
import type { DomainOperations } from './generated/models.js';
import { OPERATIONS, type OperationSpec } from './generated/operations.js';
import { buildPath } from './path.js';
import { RealtimeClient } from './realtime.js';
import {
  DEFAULT_RETRY_POLICY,
  backoffDelay,
  parseRetryAfter,
  retryableStatus,
  retryableTransport,
  sleep,
  type RetryPolicy,
} from './retry.js';
import type { ApiResponse, QueryValue, RequestOptions, RequestParams } from './types.js';
import { SDK_VERSION } from './version.js';

type FetchLike = (input: string, init: RequestInit) => Promise<Response>;

export interface CfMeetClientConfig {
  /** Gateway origin, e.g. `https://api.cfmeet.com` (http only for localhost unless allowInsecureHttp). */
  readonly baseUrl: string;
  /** Default credentials. Omit for anonymous calls only (discovery, invitation resolution). */
  readonly auth?: CfMeetAuth;
  /** Per-attempt timeout in ms (default 30 000). */
  readonly timeoutMs?: number;
  /** Automatic retries after the first attempt (default 2). NEVER-class operations are never retried. */
  readonly maxRetries?: number;
  /** Backoff tuning. */
  readonly retry?: Partial<Omit<RetryPolicy, 'maxRetries'>>;
  /** fetch implementation (defaults to the global fetch). */
  readonly fetch?: FetchLike;
  /** Appended to the SDK's User-Agent (server-side runtimes only; browsers own User-Agent). */
  readonly userAgent?: string;
  /** Allows a non-localhost http:// baseUrl (credentials then travel in clear text). */
  readonly allowInsecureHttp?: boolean;
  /**
   * DANGER. Platform credentials (cfmk_...) are machine credentials and are refused when
   * the runtime looks like a browser (window and document exist). Set this ONLY in a
   * trusted server-side runtime that defines those globals; never in code that ships to
   * users' browsers, where the credential would be exposed to anyone.
   */
  readonly dangerouslyAllowPlatformCredentialInBrowserLikeRuntime?: boolean;
}

// ---------------------------------------------------------------------------------------
// Typed accessors (client.calling.createCall(...)): built at runtime from OPERATIONS and
// typed by the generated DomainOperations.

interface OperationTypes {
  readonly params: object;
  readonly response: unknown;
}

type ArgsOf<P> = {} extends P ? [params?: P, options?: RequestOptions] : [params: P, options?: RequestOptions];

/** One operation: callable, plus `.all` / `.pages` for cursor-paginated lists. */
export type OperationMethod<E extends OperationTypes> = ((...args: ArgsOf<E['params']>) => Promise<ApiResponse<E['response']>>) & {
  readonly spec: OperationSpec;
} & (E extends { readonly item: infer I }
    ? {
        /** Every item of every page (follows nextCursor). */
        all(...args: ArgsOf<E['params']>): AsyncGenerator<I, void, undefined>;
        /** Every page (follows nextCursor). */
        pages(...args: ArgsOf<E['params']>): AsyncGenerator<ApiResponse<E['response']>, void, undefined>;
      }
    : {});

export type DomainClient<Ops> = {
  readonly [K in keyof Ops]: Ops[K] extends OperationTypes ? OperationMethod<Ops[K]> : never;
};

/** Accessor property name of each domain. */
export const DOMAIN_ACCESSORS = {
  calling: 'calling',
  'developer-platform': 'developerPlatform',
  gateway: 'gateway',
  meetings: 'meetings',
  messaging: 'messaging',
  numbering: 'numbering',
  'privacy-relay': 'privacyRelay',
  sms: 'sms',
} as const satisfies Record<keyof DomainOperations, string>;

/** Operation types of one domain operation, e.g. `OperationTypesOf<'sms', 'sendMessage'>`. */
export type OperationTypesOf<D extends keyof DomainOperations, N extends keyof DomainOperations[D]> = DomainOperations[D][N];
/** Typed params of one operation. */
export type ParamsOf<D extends keyof DomainOperations, N extends keyof DomainOperations[D]> = DomainOperations[D][N] extends {
  params: infer P;
}
  ? P
  : never;
/** Typed response body of one operation. */
export type ResponseOf<D extends keyof DomainOperations, N extends keyof DomainOperations[D]> = DomainOperations[D][N] extends {
  response: infer R;
}
  ? R
  : never;

const DISCOVERY: OperationSpec = {
  domain: 'gateway',
  name: 'discovery',
  operationId: 'discovery',
  method: 'GET',
  path: '/discovery',
  protocol: 'REST',
  auth: ['ANONYMOUS'],
  scope: null,
  pathParams: [],
  query: [],
  headers: [],
  body: 'NONE',
  idempotency: 'NONE',
  retry: 'SAFE',
  paidTelecom: false,
  pagination: null,
  success: [200],
};

const DEFINITELY_NOT_SENT = new Set(['ECONNREFUSED', 'ENOTFOUND', 'EAI_AGAIN', 'ERR_INVALID_URL', 'ERR_TLS_CERT_ALTNAME_INVALID']);
const HEADER_VALUE_RE = /^[\x20-\x7e]*$/;
const IDEMPOTENCY_KEY_RE = /^[\x21-\x7e]{1,255}$/;

function newIdempotencyKey(): string {
  const c = (globalThis as { crypto?: Crypto }).crypto;
  if (c && typeof c.randomUUID === 'function') return c.randomUUID();
  if (!c || typeof c.getRandomValues !== 'function') {
    throw new CfMeetConfigurationError('no Web Crypto in this runtime: pass an idempotencyKey explicitly');
  }
  const b = c.getRandomValues(new Uint8Array(16));
  b[6] = (b[6]! & 0x0f) | 0x40;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

function intHeader(h: Headers, name: string): number | null {
  const v = h.get(name);
  if (v == null || !/^\d+$/.test(v.trim())) return null;
  return Number(v.trim());
}

/** Quota state from the response headers (null when the gateway sent none). */
export function readQuota(h: Headers): QuotaInfo | null {
  const names = [
    'x-cf-quota-limit-minute',
    'x-cf-quota-remaining-minute',
    'x-cf-quota-reset',
    'x-cf-quota-limit-day',
    'x-cf-quota-window',
    'x-cf-quota-level',
  ];
  if (!names.some((n) => h.has(n))) return null;
  return {
    limitPerMinute: intHeader(h, 'x-cf-quota-limit-minute'),
    remainingThisMinute: intHeader(h, 'x-cf-quota-remaining-minute'),
    resetSeconds: intHeader(h, 'x-cf-quota-reset'),
    limitPerDay: intHeader(h, 'x-cf-quota-limit-day'),
    window: h.get('x-cf-quota-window'),
    level: h.get('x-cf-quota-level'),
  };
}

function normalizeBaseUrl(raw: string, allowInsecureHttp: boolean): string {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    throw new CfMeetConfigurationError('baseUrl must be an absolute http(s) URL');
  }
  if (u.protocol !== 'https:' && u.protocol !== 'http:') throw new CfMeetConfigurationError('baseUrl must be http(s)');
  if (u.username || u.password) throw new CfMeetConfigurationError('baseUrl must not contain credentials');
  if (u.search || u.hash) throw new CfMeetConfigurationError('baseUrl must not contain a query or fragment');
  const local = ['localhost', '127.0.0.1', '[::1]'].includes(u.hostname) || u.hostname.endsWith('.localhost');
  if (u.protocol === 'http:' && !local && !allowInsecureHttp) {
    throw new CfMeetConfigurationError('refusing a non-localhost http:// baseUrl (credentials would travel in clear text); use https');
  }
  return u.toString().replace(/\/+$/, '');
}

interface SendResult {
  readonly response: Response;
  readonly body: ArrayBuffer;
}

export class CfMeetClient {
  readonly baseUrl: string;
  readonly realtime: RealtimeClient;

  declare readonly calling: DomainClient<DomainOperations['calling']>;
  declare readonly developerPlatform: DomainClient<DomainOperations['developer-platform']>;
  declare readonly gateway: DomainClient<DomainOperations['gateway']>;
  declare readonly meetings: DomainClient<DomainOperations['meetings']>;
  declare readonly messaging: DomainClient<DomainOperations['messaging']>;
  declare readonly numbering: DomainClient<DomainOperations['numbering']>;
  declare readonly privacyRelay: DomainClient<DomainOperations['privacy-relay']>;
  declare readonly sms: DomainClient<DomainOperations['sms']>;

  readonly #auth: CfMeetAuth | null;
  readonly #timeoutMs: number;
  readonly #policy: RetryPolicy;
  readonly #fetch: FetchLike;
  readonly #userAgent: string | null;
  readonly #allowPlatformInBrowser: boolean;

  constructor(config: CfMeetClientConfig) {
    if (!config || typeof config !== 'object') throw new CfMeetConfigurationError('config is required');
    this.#allowPlatformInBrowser = config.dangerouslyAllowPlatformCredentialInBrowserLikeRuntime === true;
    this.baseUrl = normalizeBaseUrl(config.baseUrl, config.allowInsecureHttp === true);
    this.#auth = config.auth ?? null;
    if (this.#auth) validateAuth(this.#auth, this.#allowPlatformInBrowser);
    const timeoutMs = config.timeoutMs ?? 30_000;
    if (!(timeoutMs > 0) || !Number.isFinite(timeoutMs)) throw new CfMeetConfigurationError('timeoutMs must be a positive number');
    this.#timeoutMs = timeoutMs;
    const maxRetries = config.maxRetries ?? DEFAULT_RETRY_POLICY.maxRetries;
    if (!Number.isInteger(maxRetries) || maxRetries < 0 || maxRetries > 10) {
      throw new CfMeetConfigurationError('maxRetries must be an integer in 0..10');
    }
    this.#policy = { ...DEFAULT_RETRY_POLICY, ...config.retry, maxRetries };
    const f = config.fetch ?? (globalThis as { fetch?: FetchLike }).fetch;
    if (typeof f !== 'function') throw new CfMeetConfigurationError('no fetch in this runtime: pass config.fetch');
    this.#fetch = config.fetch ? f : (input, init) => f(input, init);
    this.#userAgent = isBrowserLike()
      ? null
      : `cfmeet-platform-sdk-typescript/${SDK_VERSION}${config.userAgent ? ` ${config.userAgent.replace(/[^\x20-\x7e]/g, '')}` : ''}`;
    this.realtime = new RealtimeClient(this, this.baseUrl, this.#auth, this.#allowPlatformInBrowser);

    const domains: Record<string, Record<string, unknown>> = {};
    for (const spec of OPERATIONS) {
      if (spec.protocol !== 'REST') continue;
      const accessor = (DOMAIN_ACCESSORS as Record<string, string>)[spec.domain];
      if (!accessor) continue;
      const fn = (params?: RequestParams, options?: RequestOptions) => this.request(spec, params, options);
      const method = Object.assign(fn, { spec }) as typeof fn & { spec: OperationSpec; [k: string]: unknown };
      if (spec.pagination) {
        method.all = (params?: RequestParams, options?: RequestOptions) => this.paginate(spec, params, options);
        method.pages = (params?: RequestParams, options?: RequestOptions) => this.pages(spec, params, options);
      }
      (domains[accessor] ??= {})[spec.name] = Object.freeze(method);
    }
    for (const accessor of Object.values(DOMAIN_ACCESSORS)) {
      Object.defineProperty(this, accessor, { value: Object.freeze(domains[accessor] ?? {}), enumerable: true });
    }
  }

  /** The operation spec of `domain.name` (throws if unknown). */
  static operation(domain: string, name: string): OperationSpec {
    const spec = OPERATIONS.find((o) => o.domain === domain && o.name === name);
    if (!spec) throw new CfMeetConfigurationError(`unknown operation ${domain}.${name}`);
    return spec;
  }

  /** `GET /discovery`: the gateway's public service discovery document (anonymous). */
  discovery(options?: RequestOptions): Promise<ApiResponse<unknown>> {
    return this.request(DISCOVERY, {}, { ...options, auth: null });
  }

  /** Performs one operation (with the operation's retry, idempotency and auth rules). */
  async request<T = unknown>(spec: OperationSpec, params: RequestParams = {}, options: RequestOptions = {}): Promise<ApiResponse<T>> {
    if (spec.protocol !== 'REST') {
      throw new CfMeetConfigurationError(`${spec.domain}.${spec.name} is a WebSocket operation: use client.realtime`);
    }
    const op = operationRef(spec);
    const url = this.baseUrl + buildPath(spec, params.path) + this.#query(spec, params.query);
    const headers = { ...this.#headers(spec, params.headers), ...forwardedHeaders(options.headers) };
    const idempotencyKey = this.#idempotencyKey(spec, params.idempotencyKey);
    if (idempotencyKey) headers['idempotency-key'] = idempotencyKey;
    const body = this.#body(spec, params.body);
    if (body !== undefined) headers['content-type'] = 'application/json';
    const auth = this.#authFor(spec, options);
    const timeoutMs = options.timeoutMs ?? this.#timeoutMs;
    const maxRetries = spec.retry === 'NEVER' ? 0 : (options.maxRetries ?? this.#policy.maxRetries);
    const signal = options.signal;
    if (signal?.aborted) throw new CfMeetAbortError(op, signal.reason);

    for (let attempt = 1; ; attempt++) {
      const requestHeaders = { ...headers, ...(auth ? await authHeaders(auth, this.#allowPlatformInBrowser) : {}) };
      let sent: SendResult;
      try {
        sent = await this.#send(url, { method: spec.method, headers: requestHeaders, body }, timeoutMs, signal, op);
      } catch (e) {
        if (spec.retry === 'NEVER') {
          if (e instanceof CfMeetNetworkError && DEFINITELY_NOT_SENT.has(errorCode(e.cause))) throw e;
          const reason: UnknownOutcomeReason =
            e instanceof CfMeetTimeoutError ? 'timeout' : e instanceof CfMeetAbortError ? 'aborted' : 'network';
          throw new UnknownOutcomeError({ reason, idempotencyKey, operation: op, cause: e });
        }
        if (e instanceof CfMeetAbortError || !retryableTransport(spec.retry) || attempt > maxRetries) throw e;
        await this.#pause(backoffDelay(this.#policy, attempt - 1), signal, op);
        continue;
      }
      const { response, body: raw } = sent;
      const requestId = response.headers.get('x-request-id');
      if (response.ok) {
        return {
          data: decode(response, raw) as T,
          status: response.status,
          headers: response.headers,
          requestId,
          correlationId: response.headers.get('x-correlation-id'),
          idempotencyKey,
          idempotentReplayed: (response.headers.get('idempotent-replayed') ?? '').trim().toLowerCase() === 'true',
          quota: readQuota(response.headers),
          attempts: attempt,
        };
      }
      const error = this.#apiError(response, raw, op);
      if (spec.retry === 'NEVER' && (response.status === 500 || response.status === 502 || response.status === 504)) {
        throw new UnknownOutcomeError({
          reason: response.status === 500 ? 'server_error' : response.status === 502 ? 'bad_gateway' : 'gateway_timeout',
          idempotencyKey,
          operation: op,
          requestId: error.requestId,
          status: response.status,
          cause: error,
        });
      }
      if (!retryableStatus(spec.retry, response.status) || attempt > maxRetries) throw error;
      if (error.retryAfterMs != null && error.retryAfterMs > this.#policy.maxRetryAfterMs) throw error;
      await this.#pause(error.retryAfterMs ?? backoffDelay(this.#policy, attempt - 1), signal, op);
    }
  }

  /** Every page of a cursor-paginated operation. */
  async *pages<T = unknown>(spec: OperationSpec, params: RequestParams = {}, options: RequestOptions = {}): AsyncGenerator<ApiResponse<T>, void, undefined> {
    const p = spec.pagination;
    if (!p) throw new CfMeetConfigurationError(`${spec.domain}.${spec.name} is not cursor-paginated`);
    let cursor = params.query?.[p.cursorParam];
    const seen = new Set<unknown>();
    for (;;) {
      const page = await this.request<T>(spec, { ...params, query: { ...params.query, [p.cursorParam]: cursor } }, options);
      yield page;
      const next = (page.data as Record<string, unknown> | null | undefined)?.[p.nextField];
      if (next === null || next === undefined || next === '') return;
      if (typeof next !== 'string' && typeof next !== 'number') {
        throw new CfMeetError(`${spec.domain}.${spec.name}: ${p.nextField} is not a cursor`);
      }
      if (seen.has(next)) throw new CfMeetError(`${spec.domain}.${spec.name}: the server repeated a cursor (pagination loop)`);
      seen.add(next);
      cursor = next;
    }
  }

  /** Every item of every page of a cursor-paginated operation. */
  async *paginate<I = unknown>(spec: OperationSpec, params: RequestParams = {}, options: RequestOptions = {}): AsyncGenerator<I, void, undefined> {
    const field = spec.pagination?.itemsField;
    for await (const page of this.pages(spec, params, options)) {
      const items = (page.data as Record<string, unknown> | null | undefined)?.[field ?? ''];
      if (!Array.isArray(items)) throw new CfMeetError(`${spec.domain}.${spec.name}: ${field} is not an array`);
      yield* items as I[];
    }
  }

  // -------------------------------------------------------------------------------------

  #authFor(spec: OperationSpec, options: RequestOptions): CfMeetAuth | null {
    if (options.auth !== undefined) {
      if (options.auth) validateAuth(options.auth, this.#allowPlatformInBrowser);
      return options.auth;
    }
    if (spec.auth.includes('ANONYMOUS')) return null; // never send credentials where none are expected
    const auth = this.#auth;
    if (!auth) {
      throw new CfMeetConfigurationError(`${spec.domain}.${spec.name} requires authentication (${spec.auth.join(' | ')}): configure auth`);
    }
    if (!AUTH_MODES[auth.type].some((m) => spec.auth.includes(m))) {
      throw new CfMeetConfigurationError(
        `${spec.domain}.${spec.name} accepts ${spec.auth.join(' | ')}; this client is configured with ${auth.type} (pass options.auth to override)`,
      );
    }
    return auth;
  }

  #query(spec: OperationSpec, query: RequestParams['query'] = {}): string {
    const out = new URLSearchParams();
    const allowed = new Map(spec.query.map((q) => [q.name, q.required]));
    for (const k of Object.keys(query)) {
      if (!allowed.has(k)) throw new CfMeetConfigurationError(`unknown query parameter ${k} for ${spec.domain}.${spec.name}`);
    }
    for (const [name, required] of allowed) {
      const v: QueryValue = query[name];
      if (v === undefined || v === null) {
        if (required) throw new CfMeetConfigurationError(`missing required query parameter ${name} for ${spec.domain}.${spec.name}`);
        continue;
      }
      for (const item of Array.isArray(v) ? v : [v]) {
        if (!['string', 'number', 'boolean'].includes(typeof item)) {
          throw new CfMeetConfigurationError(`query parameter ${name} must be a string, number or boolean`);
        }
        out.append(name, String(item));
      }
    }
    const s = out.toString();
    return s ? `?${s}` : '';
  }

  #headers(spec: OperationSpec, given: RequestParams['headers'] = {}): Record<string, string> {
    const headers: Record<string, string> = { accept: 'application/json' };
    if (this.#userAgent) headers['user-agent'] = this.#userAgent;
    const allowed = new Map(spec.headers.map((h) => [h.name.toLowerCase(), h.required]));
    const lower: Record<string, string | undefined> = {};
    for (const [k, v] of Object.entries(given)) lower[k.toLowerCase()] = v;
    for (const k of Object.keys(lower)) {
      if (!allowed.has(k)) {
        throw new CfMeetConfigurationError(`header ${k} is not a parameter of ${spec.domain}.${spec.name} (credentials are set through auth)`);
      }
    }
    for (const [name, required] of allowed) {
      const v = lower[name];
      if (v === undefined) {
        if (required) throw new CfMeetConfigurationError(`missing required header ${name} for ${spec.domain}.${spec.name}`);
        continue;
      }
      if (typeof v !== 'string' || !HEADER_VALUE_RE.test(v) || v.length > 4096) {
        throw new CfMeetConfigurationError(`header ${name} must be visible ASCII (at most 4096 characters)`);
      }
      headers[name] = v;
    }
    return headers;
  }

  #idempotencyKey(spec: OperationSpec, given: string | undefined): string | null {
    if (spec.idempotency === 'NONE') {
      if (given !== undefined) {
        throw new CfMeetConfigurationError(`${spec.domain}.${spec.name} does not take an Idempotency-Key`);
      }
      return null;
    }
    if (given === undefined) return newIdempotencyKey();
    if (typeof given !== 'string' || !IDEMPOTENCY_KEY_RE.test(given)) {
      throw new CfMeetConfigurationError('idempotencyKey must be 1-255 visible ASCII characters');
    }
    return given;
  }

  #body(spec: OperationSpec, body: unknown): string | undefined {
    if (spec.body === 'NONE') {
      if (body !== undefined) throw new CfMeetConfigurationError(`${spec.domain}.${spec.name} takes no request body`);
      return undefined;
    }
    if (body === undefined) {
      if (spec.body === 'REQUIRED') throw new CfMeetConfigurationError(`${spec.domain}.${spec.name} requires a request body`);
      return undefined;
    }
    return JSON.stringify(body);
  }

  async #send(url: string, init: RequestInit, timeoutMs: number, signal: AbortSignal | undefined, op: OperationRef): Promise<SendResult> {
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);
    const onAbort = () => controller.abort();
    signal?.addEventListener('abort', onAbort, { once: true });
    try {
      const response = await this.#fetch(url, { ...init, signal: controller.signal, redirect: 'error', credentials: 'omit' });
      const body = await response.arrayBuffer();
      return { response, body };
    } catch (e) {
      if (timedOut) throw new CfMeetTimeoutError(op, timeoutMs);
      if (signal?.aborted) throw new CfMeetAbortError(op, signal.reason);
      throw new CfMeetNetworkError(`network error calling ${op.method} ${op.path}`, op, e);
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
    }
  }

  async #pause(ms: number, signal: AbortSignal | undefined, op: OperationRef): Promise<void> {
    try {
      await sleep(ms, signal);
    } catch (e) {
      throw new CfMeetAbortError(op, e);
    }
  }

  #apiError(response: Response, raw: ArrayBuffer, op: OperationRef): CfMeetApiError {
    let parsed: unknown = null;
    try {
      const text = new TextDecoder().decode(raw);
      parsed = text ? JSON.parse(text) : null;
    } catch {
      parsed = null;
    }
    const env = (parsed as { error?: { code?: unknown; message?: unknown; requestId?: unknown } } | null)?.error;
    const code = typeof env?.code === 'string' ? env.code : `HTTP_${response.status}`;
    const message = typeof env?.message === 'string' ? env.message : response.statusText || 'request failed';
    const requestId = response.headers.get('x-request-id') ?? (typeof env?.requestId === 'string' ? env.requestId : null);
    return apiErrorFor({
      status: response.status,
      code,
      message,
      requestId,
      correlationId: response.headers.get('x-correlation-id'),
      retryAfterMs: parseRetryAfter(response.headers.get('retry-after')),
      quota: readQuota(response.headers),
      operation: op,
      body: parsed,
      headers: response.headers,
    });
  }
}

const GATEWAY_FORWARDED = new Set(['if-match', 'if-none-match', 'accept-language', 'range']);

function forwardedHeaders(given: RequestOptions['headers']): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(given ?? {})) {
    const name = k.toLowerCase();
    if (!GATEWAY_FORWARDED.has(name)) throw new CfMeetConfigurationError(`options.headers: ${k} is not forwarded by the gateway`);
    if (v === undefined) continue;
    if (typeof v !== 'string' || !HEADER_VALUE_RE.test(v) || v.length > 4096) {
      throw new CfMeetConfigurationError(`options.headers: ${k} must be visible ASCII (at most 4096 characters)`);
    }
    out[name] = v;
  }
  return out;
}

function errorCode(e: unknown): string {
  let cur: unknown = e;
  for (let i = 0; i < 4 && cur && typeof cur === 'object'; i++) {
    const code = (cur as { code?: unknown }).code;
    if (typeof code === 'string') return code;
    cur = (cur as { cause?: unknown }).cause;
  }
  return '';
}

function decode(response: Response, raw: ArrayBuffer): unknown {
  if (response.status === 204 || raw.byteLength === 0) return undefined;
  const type = (response.headers.get('content-type') ?? '').toLowerCase();
  if (type.includes('application/json') || /\+json\b/.test(type)) {
    const text = new TextDecoder().decode(raw);
    try {
      return JSON.parse(text);
    } catch (e) {
      throw new CfMeetError(`invalid JSON in a ${response.status} response (request ${response.headers.get('x-request-id') ?? 'unknown'})`, {
        cause: e,
      });
    }
  }
  return raw;
}

export { BROWSER_ESCAPE_HATCH };

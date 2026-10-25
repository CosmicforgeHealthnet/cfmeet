import type { CfMeetAuth } from './auth.js';
import type { QuotaInfo } from './errors.js';

/** A query parameter value; arrays repeat the parameter, null / undefined omit it. */
export type QueryValue = string | number | boolean | null | undefined | readonly (string | number | boolean)[];

/** Untyped parameters of `client.request` (the generated accessors type them per operation). */
export interface RequestParams {
  /** Path template parameters, e.g. `{ call_id: '…' }` for `/v1/calls/{call_id}`. */
  readonly path?: { readonly [name: string]: string | number };
  readonly query?: { readonly [name: string]: QueryValue };
  /** Only the headers the operation documents (e.g. `cf-acting-principal`). */
  readonly headers?: { readonly [name: string]: string | undefined };
  /** JSON request body. */
  readonly body?: unknown;
  /**
   * Idempotency-Key for operations that take one. When omitted the SDK generates one
   * (crypto.randomUUID) and reuses it on every automatic retry of this call.
   */
  readonly idempotencyKey?: string;
}

/** Per-call options. */
export interface RequestOptions {
  /** Cancels the call (and any pending retry). */
  readonly signal?: AbortSignal;
  /** Per-attempt timeout (defaults to the client's timeoutMs). */
  readonly timeoutMs?: number;
  /** Overrides the client's maxRetries for this call (never applies to NEVER operations). */
  readonly maxRetries?: number;
  /** Uses other credentials for this call; null sends none. */
  readonly auth?: CfMeetAuth | null;
  /** Conditional / negotiation headers the gateway forwards on every route. */
  readonly headers?: GatewayForwardedHeaders;
}

/** Request headers the public gateway forwards on every route (besides documented parameters). */
export interface GatewayForwardedHeaders {
  readonly 'if-match'?: string;
  readonly 'if-none-match'?: string;
  readonly 'accept-language'?: string;
  readonly range?: string;
}

/** A successful response. */
export interface ApiResponse<T> {
  /** Parsed JSON body (an ArrayBuffer for binary responses, undefined when empty). */
  readonly data: T;
  readonly status: number;
  readonly headers: Headers;
  /** The gateway's `x-request-id` (server-generated; quote it to support). */
  readonly requestId: string | null;
  /** The gateway's `x-correlation-id`. */
  readonly correlationId: string | null;
  /** The Idempotency-Key sent (generated or yours), null if the operation takes none. */
  readonly idempotencyKey: string | null;
  /** `Idempotent-Replayed: true`: the server returned the stored result of an earlier attempt. */
  readonly idempotentReplayed: boolean;
  /** Platform request quota headers (credential-authenticated calls). */
  readonly quota: QuotaInfo | null;
  /** Attempts made (1 + automatic retries). */
  readonly attempts: number;
}

// Typed errors of the CF Meet platform SDK.
//
// Every error the SDK throws is a `CfMeetError`. HTTP error responses become a
// `CfMeetApiError` subclass chosen by status and error code; transport failures are
// `CfMeetNetworkError` / `CfMeetTimeoutError` / `CfMeetAbortError`; and a paid telecom
// (retry class NEVER) operation whose outcome cannot be known is an `UnknownOutcomeError`.

import type { OperationSpec } from './generated/operations.js';

/** Platform request quota state, from the gateway's `x-cf-quota-*` headers. */
export interface QuotaInfo {
  /** `x-cf-quota-limit-minute`. */
  readonly limitPerMinute: number | null;
  /** `x-cf-quota-remaining-minute`. */
  readonly remainingThisMinute: number | null;
  /** `x-cf-quota-reset`: seconds until the minute window resets. */
  readonly resetSeconds: number | null;
  /** `x-cf-quota-limit-day`. */
  readonly limitPerDay: number | null;
  /** `x-cf-quota-window` (on QUOTA_EXCEEDED): `requests-per-minute` or `requests-per-day`. */
  readonly window: string | null;
  /** `x-cf-quota-level` (on QUOTA_EXCEEDED): which level of the quota hierarchy was exhausted. */
  readonly level: string | null;
}

/** A short, safe identification of the operation an error belongs to (never parameters). */
export interface OperationRef {
  readonly domain: string;
  readonly name: string;
  readonly method: string;
  readonly path: string;
}

export function operationRef(spec: OperationSpec): OperationRef {
  return { domain: spec.domain, name: spec.name, method: spec.method, path: spec.path };
}

/** Base class of every error thrown by the SDK. */
export class CfMeetError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = new.target.name;
  }
}

/** The SDK was used in a way it refuses (client-side, nothing was sent). */
export class CfMeetConfigurationError extends CfMeetError {}

/** Discriminant of `CfMeetApiError`. */
export type ApiErrorKind =
  | 'authentication'
  | 'permission'
  | 'not_found'
  | 'validation'
  | 'conflict'
  | 'rate_limited'
  | 'quota_exceeded'
  | 'upstream_unavailable'
  | 'server'
  | 'client';

export interface ApiErrorInit {
  readonly status: number;
  readonly code: string;
  readonly message: string;
  readonly requestId: string | null;
  readonly correlationId: string | null;
  readonly retryAfterMs: number | null;
  readonly quota: QuotaInfo | null;
  readonly operation: OperationRef | null;
  readonly body: unknown;
  readonly headers: Headers | null;
}

/** An error response of the API (the gateway envelope `{"error":{code,message,requestId}}`). */
export class CfMeetApiError extends CfMeetError {
  readonly kind: ApiErrorKind = 'client';
  /** HTTP status. */
  readonly status: number;
  /** Stable error code (gateway ErrorCode or the domain's code, e.g. `STALE_CALL_VERSION`). */
  readonly code: string;
  /** The server's `x-request-id` (or the envelope's requestId): quote it to support. */
  readonly requestId: string | null;
  readonly correlationId: string | null;
  /** `Retry-After`, in milliseconds, when the server sent one. */
  readonly retryAfterMs: number | null;
  readonly quota: QuotaInfo | null;
  readonly operation: OperationRef | null;
  /** The parsed response body (the error envelope, or null). */
  readonly body: unknown;
  readonly headers: Headers | null;

  constructor(init: ApiErrorInit) {
    super(`${init.code}: ${init.message}${init.requestId ? ` (request ${init.requestId})` : ''}`);
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId;
    this.correlationId = init.correlationId;
    this.retryAfterMs = init.retryAfterMs;
    this.quota = init.quota;
    this.operation = init.operation;
    this.body = init.body;
    this.headers = init.headers;
  }
}

/** 401: missing, invalid, expired or revoked credential. */
export class CfMeetAuthenticationError extends CfMeetApiError {
  override readonly kind = 'authentication' as const;
}
/** 403: authenticated but not allowed (scope, policy, origin). */
export class CfMeetPermissionError extends CfMeetApiError {
  override readonly kind = 'permission' as const;
}
/** 404: unknown resource, or a route the gateway does not expose (default deny). */
export class CfMeetNotFoundError extends CfMeetApiError {
  override readonly kind = 'not_found' as const;
}
/** 400 / 413 / 414 / 422 / 431: the request was refused as invalid. */
export class CfMeetValidationError extends CfMeetApiError {
  override readonly kind = 'validation' as const;
}
/** 409 / 412: version, state or idempotency conflict. */
export class CfMeetConflictError extends CfMeetApiError {
  override readonly kind = 'conflict' as const;
}
/** 429 RATE_LIMITED (or any 429 that is not QUOTA_EXCEEDED). */
export class CfMeetRateLimitedError extends CfMeetApiError {
  override readonly kind = 'rate_limited' as const;
}
/** 429 QUOTA_EXCEEDED: the environment's platform request quota is exhausted. */
export class CfMeetQuotaExceededError extends CfMeetApiError {
  override readonly kind = 'quota_exceeded' as const;
}
/** 502 / 503 / 504: the gateway or the service behind it is unavailable. */
export class CfMeetUpstreamUnavailableError extends CfMeetApiError {
  override readonly kind = 'upstream_unavailable' as const;
}
/** Other 5xx. */
export class CfMeetServerError extends CfMeetApiError {
  override readonly kind = 'server' as const;
}

/** The API error class for a status / code. */
export function apiErrorFor(init: ApiErrorInit): CfMeetApiError {
  const { status, code } = init;
  if (status === 401) return new CfMeetAuthenticationError(init);
  if (status === 403) return new CfMeetPermissionError(init);
  if (status === 404) return new CfMeetNotFoundError(init);
  if (status === 409 || status === 412) return new CfMeetConflictError(init);
  if (status === 429) {
    return code === 'QUOTA_EXCEEDED' ? new CfMeetQuotaExceededError(init) : new CfMeetRateLimitedError(init);
  }
  if ([400, 413, 414, 422, 431].includes(status)) return new CfMeetValidationError(init);
  if (status === 502 || status === 503 || status === 504) return new CfMeetUpstreamUnavailableError(init);
  if (status >= 500) return new CfMeetServerError(init);
  return new CfMeetApiError(init);
}

/** The request could not be completed at the transport level (DNS, TLS, reset...). */
export class CfMeetNetworkError extends CfMeetError {
  readonly operation: OperationRef | null;
  constructor(message: string, operation: OperationRef | null, cause?: unknown) {
    super(message, { cause });
    this.operation = operation;
  }
}

/** No response within the configured `timeoutMs`. */
export class CfMeetTimeoutError extends CfMeetError {
  readonly operation: OperationRef | null;
  readonly timeoutMs: number;
  constructor(operation: OperationRef | null, timeoutMs: number) {
    super(`no response within ${timeoutMs} ms`);
    this.operation = operation;
    this.timeoutMs = timeoutMs;
  }
}

/** The caller's AbortSignal fired. */
export class CfMeetAbortError extends CfMeetError {
  readonly operation: OperationRef | null;
  constructor(operation: OperationRef | null, cause?: unknown) {
    super('the request was aborted by the caller', { cause });
    this.operation = operation;
  }
}

/** Why the outcome of a NEVER-class operation is unknown. */
export type UnknownOutcomeReason = 'timeout' | 'network' | 'aborted' | 'server_error' | 'bad_gateway' | 'gateway_timeout';

/**
 * A paid telecom operation (retry class NEVER: place a call, send an SMS, acquire a
 * number...) may or may not have been processed — and may already have incurred cost.
 * The SDK never retries it. Reconcile: look the resource up, or replay the request
 * deliberately with the SAME `idempotencyKey` (the server then answers with the original
 * result and `idempotentReplayed: true` instead of acting twice).
 */
export class UnknownOutcomeError extends CfMeetError {
  readonly reason: UnknownOutcomeReason;
  /** The Idempotency-Key that was sent (null if the operation takes none). */
  readonly idempotencyKey: string | null;
  readonly operation: OperationRef;
  /** The server's request id, when a response was received. */
  readonly requestId: string | null;
  /** HTTP status, when a response was received. */
  readonly status: number | null;

  constructor(init: {
    reason: UnknownOutcomeReason;
    idempotencyKey: string | null;
    operation: OperationRef;
    requestId?: string | null;
    status?: number | null;
    cause: unknown;
  }) {
    super(
      `the outcome of ${init.operation.method} ${init.operation.path} is unknown (${init.reason}); it was NOT retried` +
        (init.idempotencyKey ? `; reconcile, or replay deliberately with Idempotency-Key ${init.idempotencyKey}` : ''),
      { cause: init.cause },
    );
    this.reason = init.reason;
    this.idempotencyKey = init.idempotencyKey;
    this.operation = init.operation;
    this.requestId = init.requestId ?? null;
    this.status = init.status ?? null;
  }
}

/** A webhook delivery failed verification (thrown by `constructEvent`). */
export class CfMeetWebhookSignatureError extends CfMeetError {
  readonly reason: 'MISMATCH' | 'STALE_TIMESTAMP' | 'MALFORMED';
  constructor(reason: 'MISMATCH' | 'STALE_TIMESTAMP' | 'MALFORMED', detail: string) {
    super(`webhook signature verification failed: ${reason} (${detail})`);
    this.reason = reason;
  }
}

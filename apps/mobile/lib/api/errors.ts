/**
 * API error decoding (FE-019).
 *
 * The meeting service answers every error, v1 and v2, with the envelope
 * `{ success: false, error: { code, message, requestId?, retryAfterSeconds? } }`
 * (`ErrorEnvelope` in packages/contracts/openapi/meeting-service.json). This module decodes
 * that envelope, keeps the stable `code`, and classifies the failure so screens can tell an
 * expired session from a forbidden action, a validation problem or wrong sign-in details.
 *
 * Pure and dependency-free (structural typing instead of importing axios) so it runs under
 * `node --test`.
 */

export type ApiErrorKind =
  | "invalid_credentials"
  | "auth_expired"
  | "auth_required"
  | "verification_required"
  | "forbidden"
  | "validation"
  | "not_found"
  | "conflict"
  | "rate_limited"
  | "unavailable"
  | "network"
  | "unknown";

export interface ApiError {
  message: string;
  /** HTTP status; 0 when no response arrived. */
  status: number;
  kind: ApiErrorKind;
  /** Stable semantic code from the envelope, e.g. `MEETING_PASSCODE_REQUIRED`. */
  code?: string;
  requestId?: string;
  retryAfterSeconds?: number;
  /** Optional structured details, when a service sends them. */
  details?: unknown;
  /** Legacy field-level validation errors, when a service sends them. */
  errors?: Record<string, string[]>;
}

/** An `Error` that keeps the decoded API failure (thrown by the data hooks). */
export class ApiRequestError extends Error implements ApiError {
  status: number;
  kind: ApiErrorKind;
  code?: string;
  requestId?: string;
  retryAfterSeconds?: number;
  details?: unknown;
  errors?: Record<string, string[]>;

  constructor(decoded: ApiError) {
    super(decoded.message);
    this.name = "ApiRequestError";
    this.status = decoded.status;
    this.kind = decoded.kind;
    this.code = decoded.code;
    this.requestId = decoded.requestId;
    this.retryAfterSeconds = decoded.retryAfterSeconds;
    this.details = decoded.details;
    this.errors = decoded.errors;
  }
}

interface HttpErrorLike {
  isAxiosError?: boolean;
  message?: string;
  config?: { url?: string; headers?: Record<string, unknown> };
  response?: { status?: number; data?: unknown };
}

/** Codes that mean "your credential is missing, invalid, expired or revoked". */
const AUTH_CODES = new Set(["AUTHENTICATION_REQUIRED", "UNAUTHORIZED"]);

/** Codes that mean "you are known, but not allowed". */
const FORBIDDEN_CODES = new Set([
  "FORBIDDEN",
  "PERMISSION_DENIED",
  "MEETING_ACCESS_DENIED",
  "PARTICIPANT_BANNED",
  "BACKSTAGE_ACCESS_DENIED",
  "ARTIFACT_ACCESS_DENIED",
  "OPERATIONS_ACCESS_DENIED",
  "MEMBERSHIP_REQUIRED",
  "INVITATION_REQUIRED",
  "PROFILE_POLICY_DENIED",
  "ORGANIZATION_POLICY_RESTRICTED",
]);

/** Codes that mean "the request itself is wrong; fix the input". */
const VALIDATION_CODES = new Set([
  "VALIDATION_ERROR",
  "BAD_REQUEST",
  "MEETING_PASSCODE_REQUIRED",
  "MEETING_PASSCODE_INVALID",
  "POLL_RESPONSE_INVALID",
  "INVITATION_INVALID",
  "ADMISSION_TOKEN_INVALID",
  "REQUEST_TOO_LARGE",
  "QUALITY_SAMPLE_INVALID",
]);

const SIGN_IN_PATHS = ["/auth/login", "/auth/oauth"];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function str(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function isSignInRequest(url: string | undefined): boolean {
  return !!url && SIGN_IN_PATHS.some((p) => url.includes(p));
}

export function classify(
  status: number,
  code: string | undefined,
  url: string | undefined,
  hadBearer = true
): ApiErrorKind {
  if (status === 0) return "network";
  if (code === "RATE_LIMITED" || code === "TOO_MANY_REQUESTS" || code === "QUALITY_RATE_LIMITED" || status === 429) {
    return "rate_limited";
  }
  if (code === "EMAIL_VERIFICATION_REQUIRED") return "verification_required";
  // A 401 that names a passcode problem is an input error, not an expired session.
  if (code && VALIDATION_CODES.has(code)) return "validation";
  if (status === 401 || (code && AUTH_CODES.has(code))) {
    if (isSignInRequest(url)) return "invalid_credentials";
    // Only a request that carried an account token can have an "expired session".
    return hadBearer ? "auth_expired" : "auth_required";
  }
  if (status === 403 || (code && FORBIDDEN_CODES.has(code))) return "forbidden";
  if (status === 400 || status === 422) return "validation";
  if (status === 404) return "not_found";
  if (status === 409) return "conflict";
  if (status >= 500) return "unavailable";
  return "unknown";
}

const KIND_MESSAGES: Record<ApiErrorKind, string> = {
  invalid_credentials: "Invalid email or password.",
  auth_expired: "Your session has expired. Please sign in again.",
  auth_required: "Sign in, or join the meeting again, to continue.",
  verification_required: "Verify your email address to continue.",
  forbidden: "You don't have permission to perform this action.",
  validation: "Invalid request. Please check your input.",
  not_found: "The requested resource was not found.",
  conflict: "This item was changed elsewhere. Refresh and try again.",
  rate_limited: "Too many requests. Please wait a moment and try again.",
  unavailable: "Service temporarily unavailable. Please try again later.",
  network: "Unable to reach the server. Check your connection and try again.",
  unknown: "An error occurred. Please try again.",
};

/** Decode the response body of a failed request (v2 envelope first, then legacy shapes). */
export function decodeErrorBody(data: unknown): {
  code?: string;
  message?: string;
  requestId?: string;
  retryAfterSeconds?: number;
  details?: unknown;
  errors?: Record<string, string[]>;
} {
  if (!isRecord(data)) return { message: str(data) };
  const envelope = data.error;
  if (isRecord(envelope)) {
    const retry = envelope.retryAfterSeconds;
    return {
      code: str(envelope.code),
      message: str(envelope.message),
      requestId: str(envelope.requestId),
      retryAfterSeconds: typeof retry === "number" && Number.isFinite(retry) ? retry : undefined,
      details: envelope.details,
      errors: isRecord(data.errors) ? (data.errors as Record<string, string[]>) : undefined,
    };
  }
  // Legacy shapes: `{ message }`, `{ error: "text" }`, `{ code, message }`.
  return {
    code: str(data.code),
    message: str(data.message) ?? str(data.error),
    errors: isRecord(data.errors) ? (data.errors as Record<string, string[]>) : undefined,
  };
}

/** Whether the failed request carried an account bearer token (never returns the token). */
function requestHadBearer(headers: Record<string, unknown> | undefined): boolean {
  if (!headers) return false;
  const getter = (headers as { get?: unknown }).get;
  const value =
    typeof getter === "function"
      ? (getter as (name: string) => unknown).call(headers, "Authorization")
      : headers.Authorization ?? headers.authorization;
  return typeof value === "string" && value.startsWith("Bearer ");
}

/** Decode any thrown value into an {@link ApiError}. */
export function decodeApiError(error: unknown): ApiError {
  if (error instanceof ApiRequestError) return error;
  const http = error as HttpErrorLike;
  if (isRecord(error) && (http.isAxiosError || "response" in error)) {
    const response = http.response;
    if (!response) {
      return { message: KIND_MESSAGES.network, status: 0, kind: "network" };
    }
    const status = typeof response.status === "number" ? response.status : 0;
    const body = decodeErrorBody(response.data);
    const kind = classify(status || 500, body.code, http.config?.url, requestHadBearer(http.config?.headers));
    // A stale/expired session always gets the friendly sign-in-again text; elsewhere the
    // server's message is safe to show (the contract forbids secrets, SQL or paths in it).
    const message =
      kind === "auth_expired" || kind === "auth_required"
        ? KIND_MESSAGES[kind]
        : body.message ?? KIND_MESSAGES[kind];
    return {
      message,
      status: status || 500,
      kind,
      code: body.code,
      requestId: body.requestId,
      retryAfterSeconds: body.retryAfterSeconds,
      details: body.details,
      errors: body.errors,
    };
  }
  if (error instanceof Error && error.message) {
    return { message: error.message, status: 0, kind: "unknown" };
  }
  if (typeof error === "string" && error) {
    return { message: error, status: 0, kind: "unknown" };
  }
  return { message: "An unexpected error occurred", status: 0, kind: "unknown" };
}

/** Decode and wrap as a throwable {@link ApiRequestError}. */
export function toApiRequestError(error: unknown): ApiRequestError {
  return error instanceof ApiRequestError ? error : new ApiRequestError(decodeApiError(error));
}

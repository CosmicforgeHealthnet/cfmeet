/**
 * Developer Portal presentation rules: error -> user message, retry policy, one-time
 * secret handling and credential status badges.
 *
 * Pure module (node tests load it with --experimental-strip-types).
 */
import type { Credential, ConsoleWebhookWithSecret, IssuedCredential, InvitationWithToken } from "./types";

type ErrorLike = {
  name?: string;
  status?: number;
  code?: string;
  message?: string;
  requestId?: string | null;
  retryAfterSeconds?: number | null;
  kind?: string;
};

export type ErrorView = {
  title: string;
  detail: string;
  /** Shown in the toast for support (X-Request-Id). */
  requestId: string | null;
  action: "signin" | "retry" | "reload" | "none";
};

const STATE_CODES: Record<string, string> = {
  ORGANIZATION_RESTRICTED: "This developer workspace is restricted: only membership, billing-reference, closure and access-withdrawal changes are possible.",
  ORGANIZATION_SUSPENDED: "This developer workspace is suspended: it stays readable, but nothing can be changed.",
  ORGANIZATION_CLOSED: "This developer workspace is closed: it stays readable, but nothing can be changed.",
  ORGANIZATION_NOT_ACTIVE: "This developer workspace is not active yet.",
};

const CONFLICT_CODES: Record<string, string> = {
  VERSION_CONFLICT: "Someone changed this in the meantime. Reload and try again.",
  INVALID_STATE: "This action is not possible in the resource's current state.",
  LAST_OWNER: "A workspace must keep one owner. Transfer ownership first.",
  ALREADY_MEMBER: "That person is already a member of this developer workspace.",
  INVITATION_PENDING: "An invitation for that email is already pending.",
  INVITATION_INVALID: "This invitation token is invalid, expired, revoked or already used.",
  SELF_APPROVAL_FORBIDDEN: "A reviewer may not decide a request of an organization they belong to.",
  INVITATION_EMAIL_MISMATCH: "This invitation was issued to a different email address than your CF Meet account.",
  IDEMPOTENCY_KEY_REUSED: "This request was already submitted with different details. Start again.",
  CONFLICTING_REPLAY: "This request was already submitted with different details. Start again.",
  POLICY_VIOLATION: "The request is not allowed by platform or workspace policy.",
  APPROVAL_EXPIRED: "The production approval has expired. Submit a new request.",
  PRODUCTION_ACCESS_REQUIRED: "This needs approved production access for the project.",
  CAPABILITY_UNAVAILABLE: "This capability is not available for the project.",
  OPERATOR_REQUIRED: "This change can only be made by CF Meet operators.",
};

export function describeError(error: unknown): ErrorView {
  const e = (error ?? {}) as ErrorLike;
  const requestId = e.requestId ?? null;
  if (e.kind === "unavailable") {
    return { title: "Developer Platform not available in this environment", detail: e.message ?? "", requestId: null, action: "none" };
  }
  if (e.kind === "network") {
    return {
      title: "Developer Platform unreachable",
      detail: "The API Gateway did not answer. It may not be deployed or reachable from this environment yet.",
      requestId: null,
      action: "retry",
    };
  }
  if (e.code === "SESSION_UNAVAILABLE") {
    return { title: "Sign-in service unavailable", detail: "Your CF Meet session could not be checked. Try again.", requestId: null, action: "retry" };
  }
  if (e.code === "MACHINE_CREDENTIAL_REFUSED") {
    return { title: "Machine credential refused", detail: e.message ?? "", requestId: null, action: "none" };
  }
  const code = e.code ?? "";
  // Specific contract codes first: several are not on the status their family suggests
  // (INVITATION_INVALID is 404, INVITATION_EMAIL_MISMATCH / PRODUCTION_ACCESS_REQUIRED are
  // 403, OPERATOR_REQUIRED / CONFLICTING_REPLAY are 400, CAPABILITY_UNAVAILABLE is 503).
  if (STATE_CODES[code]) return { title: "Workspace state", detail: STATE_CODES[code], requestId, action: "none" };
  if (CONFLICT_CODES[code]) {
    return {
      title: e.status === 409 ? "Conflict" : e.status === 422 ? "Not allowed by policy" : "Not possible",
      detail: CONFLICT_CODES[code],
      requestId,
      action: code === "VERSION_CONFLICT" ? "reload" : "none",
    };
  }
  switch (e.status) {
    case 401:
      return { title: "Sign in again", detail: "Your CF Meet session has expired or is not valid for the Developer Platform.", requestId, action: "signin" };
    case 403:
      return {
        title: "Not allowed",
        detail: "Your developer workspace role does not allow this action. Roles are checked on every request.",
        requestId,
        action: "none",
      };
    case 404:
      return { title: "Not found", detail: "It does not exist or is not part of this developer workspace.", requestId, action: "reload" };
    case 409:
    case 422:
      return {
        title: e.status === 409 ? "Conflict" : "Not allowed by policy",
        detail: e.message ?? "",
        requestId,
        action: "none",
      };
    case 429: {
      const wait = e.retryAfterSeconds;
      return {
        title: "Too many requests",
        detail: wait != null ? `Rate limited by the API Gateway. Try again in ${wait} s.` : "Rate limited by the API Gateway. Try again shortly.",
        requestId,
        action: "retry",
      };
    }
    case 400:
      return { title: "Invalid request", detail: e.message ?? "Check the values and try again.", requestId, action: "none" };
    case 502:
    case 503:
    case 504:
      return {
        title: "Temporarily unavailable",
        detail:
          code === "SERVICE_UNAVAILABLE"
            ? "A required service (for example the sign-in session bridge or Platform Access) is unavailable. Try again later."
            : e.message ?? "Try again later.",
        requestId,
        action: "retry",
      };
    default:
      return { title: "Request failed", detail: e.message ?? "Unexpected error.", requestId, action: "retry" };
  }
}

/** Reads only: retry network failures, 429 and 502/503/504 (at most `max` times). */
export function shouldRetryQuery(failureCount: number, error: unknown, max = 2): boolean {
  const e = (error ?? {}) as ErrorLike;
  if (failureCount >= max) return false;
  if (e.kind === "network") return true;
  return e.status === 429 || e.status === 502 || e.status === 503 || e.status === 504;
}

/** Delay before a query retry: Retry-After when the gateway gave one, else backoff. */
export function retryDelayMs(attempt: number, error: unknown): number {
  const e = (error ?? {}) as ErrorLike;
  if (typeof e.retryAfterSeconds === "number") return Math.min(e.retryAfterSeconds, 60) * 1000;
  return Math.min(1000 * 2 ** attempt, 15000);
}

export type OneTimeSecret =
  | { kind: "secret"; value: string }
  /** An idempotent replay: the secret is never returned twice. */
  | { kind: "redacted" };

/** The one-time secret of an issued / rotated credential (never persisted by the caller). */
export function credentialSecret(issued: IssuedCredential): OneTimeSecret {
  return typeof issued.secret === "string" && issued.secret.length > 0 ? { kind: "secret", value: issued.secret } : { kind: "redacted" };
}

/** The one-time signing secret of a created webhook / rotated webhook secret. */
export function webhookSecret(result: ConsoleWebhookWithSecret): OneTimeSecret {
  return typeof result.signingSecret === "string" && result.signingSecret.length > 0
    ? { kind: "secret", value: result.signingSecret }
    : { kind: "redacted" };
}

/** The one-time invitation acceptance token (to hand to the invitee out of band). */
export function invitationToken(result: InvitationWithToken): OneTimeSecret {
  return typeof result.token === "string" && result.token.length > 0 ? { kind: "secret", value: result.token } : { kind: "redacted" };
}

export type Badge = { label: string; tone: "ok" | "warn" | "bad" | "muted" };

/** Status badges of a credential row: effective state, expiry warning, last use. */
export function credentialBadges(credential: Credential, now: number = Date.now()): Badge[] {
  const badges: Badge[] = [];
  const state = credential.effectiveState ?? credential.status ?? "UNKNOWN";
  const tone: Badge["tone"] =
    state === "ACTIVE" ? "ok" : state === "ROTATION_OVERLAP" || state === "NOT_YET_VALID" ? "warn" : state === "UNKNOWN" ? "muted" : "bad";
  badges.push({ label: state.replace(/_/g, " "), tone });
  if (credential.expiringSoon) badges.push({ label: "Expiring soon", tone: "warn" });
  if (credential.expiresAt && Date.parse(credential.expiresAt) <= now && state !== "REVOKED") {
    badges.push({ label: "Expired", tone: "bad" });
  }
  badges.push(
    credential.lastAuthenticatedAt
      ? { label: `Last used ${new Date(credential.lastAuthenticatedAt).toISOString().slice(0, 16).replace("T", " ")} UTC`, tone: "muted" }
      : { label: "Never used", tone: "muted" },
  );
  return badges;
}

/** Credential actions offered for a row (role checks are applied separately). */
export function credentialActions(credential: Credential): { rotate: boolean; suspend: boolean; resume: boolean; revoke: boolean } {
  const state = credential.effectiveState;
  const revoked = credential.status === "REVOKED" || state === "REVOKED";
  if (revoked || state === "EXPIRED") return { rotate: false, suspend: false, resume: false, revoke: false };
  // A superseded credential may still authenticate until supersessionValidUntil: it can
  // be revoked early, but not rotated again.
  if (state === "SUPERSEDED") return { rotate: false, suspend: false, resume: false, revoke: true };
  if (state === "SUSPENDED") return { rotate: false, suspend: false, resume: true, revoke: true };
  return { rotate: true, suspend: true, resume: false, revoke: true };
}

/** `YYYY-MM-DD` (UTC) `days` before `now`, for usage ranges. */
export function isoDay(now: number, daysBack = 0): string {
  return new Date(now - daysBack * 86_400_000).toISOString().slice(0, 10);
}

/** Tone of a lifecycle / delivery status word, for badges. */
export function statusTone(status: string | null | undefined): Badge["tone"] {
  switch (status) {
    case "ACTIVE":
    case "SUCCEEDED":
    case "LIVE":
    case "APPROVED":
    case "ACCEPTED":
      return "ok";
    case "PENDING":
    case "DELIVERING":
    case "RETRY_SCHEDULED":
    case "INVITED":
    case "RESTRICTED":
    case "SUSPENDED":
    case "DISABLED":
    case "PRODUCTION_REQUESTED":
    case "UNDER_REVIEW":
    case "ROTATION_OVERLAP":
    case "NOT_YET_VALID":
    case "WARNING":
      return "warn";
    case "FAILED":
    case "REVOKED":
    case "CLOSED":
    case "DELETED":
    case "REJECTED":
    case "EXPIRED":
    case "EXHAUSTED":
    case "DENIED":
      return "bad";
    default:
      return "muted";
  }
}

/** What a delivery status means for the integrator (SHADOW: recorded, never sent). */
export const DELIVERY_STATUS_HELP: Record<string, string> = {
  PENDING: "Queued for delivery.",
  DELIVERING: "Being delivered now.",
  RETRY_SCHEDULED: "The endpoint failed; another attempt is scheduled.",
  SUCCEEDED: "The endpoint answered 2xx.",
  FAILED: "All attempts failed (dead-lettered). It can be replayed.",
  CANCELLED: "Cancelled (the subscription was disabled or deleted).",
  SHADOW: "Recorded in shadow mode: this deployment does not send webhooks yet, so nothing was sent.",
};

/** A replay creates a new delivery of the same event to the same subscription, which must be ACTIVE. */
export function canReplayDelivery(webhookStatus: string | null | undefined, delivery: { status: string }): boolean {
  return webhookStatus === "ACTIVE" && delivery.status !== "PENDING" && delivery.status !== "DELIVERING";
}

/** Truthful commercial wording: plans are entitlement records, no billing engine exists. */
export const BILLING_NOT_AVAILABLE =
  "Billing and payments are not available yet: CF Meet does not issue invoices, take payment methods or charge for usage from this console. " +
  "Plans below are entitlement records assigned by CF Meet; usage figures are metering for limits and reporting only.";

/** Formats a quantity with its usage unit (no currency: there is no rating or pricing). */
export function formatQuantity(quantity: number, unit: string): string {
  const n = Number.isFinite(quantity) ? quantity : 0;
  const text = Math.abs(n) >= 1000 ? n.toLocaleString("en-US", { maximumFractionDigits: 2 }) : String(Math.round(n * 100) / 100);
  return `${text} ${unit.toLowerCase().replace(/_/g, " ")}`;
}

/** A limit's fill ratio clamped to 0..1 (null when the ceiling or current value is unknown). */
export function limitRatio(current: number | null | undefined, ceiling: number | null | undefined): number | null {
  if (current == null || ceiling == null || !Number.isFinite(current) || !Number.isFinite(ceiling) || ceiling <= 0) return null;
  return Math.min(1, Math.max(0, current / ceiling));
}

/** `YYYY-MM-DD HH:MM UTC`, or an em dash. */
export function formatTimestamp(value: string | null | undefined): string {
  if (!value) return "—";
  const at = Date.parse(value);
  if (Number.isNaN(at)) return "—";
  return `${new Date(at).toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

/** Validates an RFC 3339 expiry chosen in a `datetime-local` input; returns ISO or null. */
export function expiryFromLocalInput(value: string, now: number = Date.now()): { ok: true; iso: string | null } | { ok: false; reason: string } {
  if (!value) return { ok: true, iso: null };
  const at = Date.parse(value);
  if (Number.isNaN(at)) return { ok: false, reason: "Enter a valid date and time." };
  if (at <= now) return { ok: false, reason: "The expiry must be in the future." };
  return { ok: true, iso: new Date(at).toISOString() };
}

/**
 * A webhook endpoint the console accepts before asking the server, which applies the
 * authoritative SSRF policy (packages/webhooks/src/endpoint.rs: https only unless the
 * deployment allows http for local development, no private networks). Plain http is let
 * through only for a loopback host so a local stack can still be exercised.
 */
export function validateWebhookUrl(value: string): string | null {
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    return "Enter a full https:// URL.";
  }
  const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
  if (url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) return "Webhook endpoints must use HTTPS.";
  if (url.username || url.password) return "Do not put credentials in the endpoint URL.";
  return null;
}

/**
 * A documentation link from the public discovery document, resolved against the gateway
 * origin. Only http(s) results are returned (no `javascript:` / `data:` links); plain http
 * only for a loopback gateway.
 */
export function safeDocumentUrl(value: string | null | undefined, baseUrl: string): string | null {
  if (!value) return null;
  let url: URL;
  try {
    url = new URL(value, `${baseUrl.replace(/\/+$/, "")}/`);
  } catch {
    return null;
  }
  const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
  if (url.protocol === "https:" || (url.protocol === "http:" && loopback)) return url.toString();
  return null;
}

/**
 * Account rules the web client applies before and after calling the account authority
 * (Blueprint Section 17: ACC-022, ACC-026, ACC-025 password bounds, ACC-004/005 Google
 * exchange, Section 9 profile).
 *
 * Pure: type-only imports, so node tests run it directly (tests/account-workspace.test.mjs).
 */
import type { AccountProfile, UpdateAccountProfileRequest } from "../../types/generated/meeting-service";

export type AccountStatus = "pending_verification" | "active" | "inactive" | "suspended" | "unknown";

/**
 * ACC-022: v1 renders `pendingverification` until `ACCOUNT_STATUS_FORMAT=canonical`, then
 * `pending_verification` (v2 is canonical already). Accept both, and any casing.
 */
export function normalizeAccountStatus(value: unknown): AccountStatus {
  if (typeof value !== "string") return "unknown";
  const compact = value.trim().toLowerCase().replace(/[\s_-]/g, "");
  switch (compact) {
    case "pendingverification":
      return "pending_verification";
    case "active":
    case "inactive":
    case "suspended":
      return compact;
    default:
      return "unknown";
  }
}

export function accountStatusLabel(status: AccountStatus): string {
  switch (status) {
    case "pending_verification":
      return "Pending verification";
    case "active":
      return "Active";
    case "inactive":
      return "Deactivated";
    case "suspended":
      return "Suspended";
    default:
      return "Unknown";
  }
}

/** Where a fresh sign-in goes, by account status. */
export function postSignInRoute(status: unknown): "/dashboard" | "/verify" | "/login" {
  switch (normalizeAccountStatus(status)) {
    case "active":
      return "/dashboard";
    case "pending_verification":
      return "/verify";
    default:
      return "/login";
  }
}

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

export const PASSWORD_SPECIALS = "!@#$%^&*(),.?:{}|<>";

/**
 * The account authority's password rule (registration, reset, change): 8–72 characters,
 * at most 72 bytes (bcrypt reads only 72), upper, lower, digit and one of PASSWORD_SPECIALS.
 * Returns the first problem, or null.
 */
export function passwordProblem(password: string): string | null {
  if ([...password].length < 8) return "Password must be at least 8 characters.";
  if ([...password].length > 72 || utf8ByteLength(password) > 72) {
    return "Password must be at most 72 bytes (fewer characters if you use accented letters or emoji).";
  }
  if (!/[A-Z]/.test(password)) return "Password must contain at least one uppercase letter.";
  if (!/[a-z]/.test(password)) return "Password must contain at least one lowercase letter.";
  if (!/[0-9]/.test(password)) return "Password must contain at least one number.";
  if (![...password].some((c) => PASSWORD_SPECIALS.includes(c))) {
    return `Password must contain at least one special character (${PASSWORD_SPECIALS}).`;
  }
  return null;
}

export interface ApiErrorInfo {
  status: number;
  code: string;
  message: string | null;
  retryAfterSeconds: number | null;
  requestId: string | null;
}

/** The public error envelope `{ success: false, error: { code, message, requestId } }` of any thrown value. */
export function apiErrorOf(error: unknown): ApiErrorInfo {
  const e = error as {
    response?: { status?: number; data?: { error?: { code?: unknown; message?: unknown; retryAfterSeconds?: unknown; requestId?: unknown }; message?: unknown } };
    code?: unknown;
  } | null;
  const status = typeof e?.response?.status === "number" ? e.response.status : 0;
  const body = e?.response?.data?.error;
  const code = typeof body?.code === "string" ? body.code : status ? `HTTP_${status}` : typeof e?.code === "string" && e.code.startsWith("SESSION_") ? e.code : "NETWORK_UNAVAILABLE";
  const message = typeof body?.message === "string" ? body.message : typeof e?.response?.data?.message === "string" ? (e.response.data.message as string) : null;
  return {
    status,
    code,
    message,
    retryAfterSeconds: typeof body?.retryAfterSeconds === "number" ? body.retryAfterSeconds : null,
    requestId: typeof body?.requestId === "string" ? body.requestId : null,
  };
}

/**
 * ACC-026: what Settings → Danger Zone does with a deactivation refusal. The API answers
 * `403 REAUTHENTICATION_REQUIRED` both for a wrong password and for a missing password
 * outside the recent-sign-in window (a wrong password never falls back to that window).
 */
export function deactivationOutcome(error: unknown, sentPassword: boolean): "wrong_password" | "reauthenticate" | "failed" {
  const { code } = apiErrorOf(error);
  if (code !== "REAUTHENTICATION_REQUIRED") return "failed";
  return sentPassword ? "wrong_password" : "reauthenticate";
}

export type GoogleCallback =
  | { kind: "error"; reason: string }
  | { kind: "exchange"; exchangeCode: string }
  | {
      kind: "legacy";
      accessToken: string;
      refreshToken: string;
      user: { id: string; username: string; role: string; status: string };
    }
  | { kind: "invalid" };

const GOOGLE_ERRORS: Record<string, string> = {
  oauth_cancelled: "Google sign-in was cancelled.",
  oauth_invalid_request: "The Google sign-in request was incomplete. Try again.",
  oauth_invalid_state: "The Google sign-in expired or was started in another browser. Try again.",
  account_disabled: "This account is disabled.",
  account_conflict: "This Google account cannot be linked to the existing CF Meet account.",
  oauth_failed: "Google sign-in failed. Try again.",
};

export function googleErrorMessage(reason: string): string {
  return GOOGLE_ERRORS[reason] ?? "Google sign-in failed. Try again.";
}

/**
 * The Google callback query (ACC-004/005): `exchange_code` (GOOGLE_OAUTH_TOKEN_DELIVERY=
 * exchange_code — preferred, tokens never in the URL) or the legacy token query that the
 * API sends until that flag is switched.
 */
export function parseGoogleCallback(params: { get(name: string): string | null }): GoogleCallback {
  const error = params.get("error");
  if (error) return { kind: "error", reason: error };
  const exchangeCode = params.get("exchange_code");
  if (exchangeCode) {
    return exchangeCode.length <= 512 && /^[A-Za-z0-9_-]+$/.test(exchangeCode)
      ? { kind: "exchange", exchangeCode }
      : { kind: "invalid" };
  }
  const accessToken = params.get("access_token");
  const refreshToken = params.get("refresh_token");
  const id = params.get("user_id");
  const username = params.get("username");
  const role = params.get("role");
  const status = params.get("status");
  if (accessToken && refreshToken && id && username && role && status) {
    return { kind: "legacy", accessToken, refreshToken, user: { id, username, role, status } };
  }
  return { kind: "invalid" };
}

export interface ProfileForm {
  username: string;
  displayName: string;
  avatarUrl: string;
  timezone: string;
  locale: string;
}

export function profileForm(profile: AccountProfile): ProfileForm {
  return {
    username: profile.username,
    displayName: profile.displayName ?? "",
    avatarUrl: profile.avatarUrl ?? "",
    timezone: profile.timezone ?? "",
    locale: profile.locale ?? "",
  };
}

/**
 * `PATCH /api/v2/me/profile` body: only changed fields; an emptied optional field is sent
 * as `null` (clears it); username is never cleared.
 */
export function profilePatch(profile: AccountProfile, form: ProfileForm): UpdateAccountProfileRequest {
  const patch: UpdateAccountProfileRequest = {};
  const username = form.username.trim();
  if (username && username !== profile.username) patch.username = username;
  const optional = (value: string, current: string | null | undefined) => {
    const next = value.trim();
    if (next === (current ?? "")) return undefined;
    return next === "" ? null : next;
  };
  const displayName = optional(form.displayName, profile.displayName);
  if (displayName !== undefined) patch.displayName = displayName;
  const avatarUrl = optional(form.avatarUrl, profile.avatarUrl);
  if (avatarUrl !== undefined) patch.avatarUrl = avatarUrl;
  const timezone = optional(form.timezone, profile.timezone);
  if (timezone !== undefined) patch.timezone = timezone;
  const locale = optional(form.locale, profile.locale);
  if (locale !== undefined) patch.locale = locale;
  return patch;
}

/** Client-side checks mirroring the documented bounds; the server stays authoritative. */
export function profileProblems(form: ProfileForm): Partial<Record<keyof ProfileForm, string>> {
  const problems: Partial<Record<keyof ProfileForm, string>> = {};
  if (!/^[A-Za-z0-9_]{3,50}$/.test(form.username.trim())) problems.username = "3–50 letters, digits or underscores.";
  const name = form.displayName.trim();
  if (name && [...name].length > 64) problems.displayName = "At most 64 characters.";
  const avatar = form.avatarUrl.trim();
  if (avatar && (!/^https:\/\/[^\s]+$/i.test(avatar) || avatar.length > 512)) problems.avatarUrl = "An https:// address of at most 512 characters.";
  const locale = form.locale.trim();
  if (locale && !/^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(locale)) problems.locale = "A language tag such as en or en-GB.";
  const zone = form.timezone.trim();
  if (zone && !/^[A-Za-z_]+(\/[A-Za-z0-9_+-]+)*$/.test(zone)) problems.timezone = "An IANA time zone such as Europe/London.";
  return problems;
}

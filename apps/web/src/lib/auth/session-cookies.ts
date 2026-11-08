/**
 * Server half of the web session model (ACC-019): the cookies the app's own route handlers
 * (`src/app/api/session/*`) set, and the request checks they apply.
 *
 * - `cfm_rt`: the refresh token. HttpOnly (script never reads it), SameSite=Strict, Path
 *   `/api/session` (sent to the session routes only), Secure on HTTPS.
 * - `cfm_session`: a non-secret hint (`persistent` / `session`) script reads to know a
 *   session exists, and the route reads to keep "remember me" across rotations.
 *
 * Pure: no imports, so node tests run it directly (tests/account-session.test.mjs).
 */

export const REFRESH_COOKIE = "cfm_rt";
export const HINT_COOKIE = "cfm_session";
export const REFRESH_COOKIE_PATH = "/api/session";
/** Account sessions end 30 days after sign-in (ADR 0057); no cookie outlives that. */
export const MAX_SESSION_SECONDS = 30 * 24 * 60 * 60;
/** Header every session request must carry (forces a CORS preflight cross-site). */
export const SESSION_REQUEST_HEADER = "x-cf-session";

const JWT = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/;

export function looksLikeJwt(value: unknown): value is string {
  return typeof value === "string" && value.length <= 4096 && JWT.test(value);
}

/** `exp` (seconds) of a JWT without verifying it — only used to bound the cookie lifetime. */
export function jwtExpiry(token: string): number | null {
  if (!looksLikeJwt(token)) return null;
  try {
    const segment = token.split(".")[1];
    const padded = segment.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((segment.length + 3) % 4);
    const claims = JSON.parse(atob(padded)) as { exp?: unknown };
    return typeof claims.exp === "number" && Number.isFinite(claims.exp) ? claims.exp : null;
  } catch {
    return null;
  }
}

/** Seconds the refresh cookie may live: until the token's expiry, at most 30 days. */
export function refreshCookieMaxAge(refreshToken: string, nowMs: number): number {
  const exp = jwtExpiry(refreshToken);
  const remaining = exp === null ? MAX_SESSION_SECONDS : Math.floor(exp - nowMs / 1000);
  return Math.max(0, Math.min(MAX_SESSION_SECONDS, remaining));
}

export interface CookieOptions {
  path: string;
  httpOnly: boolean;
  secure: boolean;
  /** null: a browser-session cookie. */
  maxAge: number | null;
}

export function serializeCookie(name: string, value: string, o: CookieOptions): string {
  const parts = [`${name}=${encodeURIComponent(value)}`, `Path=${o.path}`, "SameSite=Strict"];
  if (o.maxAge !== null) parts.push(`Max-Age=${Math.max(0, Math.floor(o.maxAge))}`);
  if (o.httpOnly) parts.push("HttpOnly");
  if (o.secure) parts.push("Secure");
  return parts.join("; ");
}

/** Set-Cookie values that store a (rotated) refresh token. */
export function sessionCookies(input: { refreshToken: string; persistent: boolean; secure: boolean; nowMs: number }): string[] {
  const lifetime = refreshCookieMaxAge(input.refreshToken, input.nowMs);
  const maxAge = input.persistent ? lifetime : null;
  return [
    serializeCookie(REFRESH_COOKIE, input.refreshToken, { path: REFRESH_COOKIE_PATH, httpOnly: true, secure: input.secure, maxAge }),
    serializeCookie(HINT_COOKIE, input.persistent ? "persistent" : "session", { path: "/", httpOnly: false, secure: input.secure, maxAge }),
  ];
}

/** Set-Cookie values that remove both cookies. */
export function clearedSessionCookies(secure: boolean): string[] {
  return [
    serializeCookie(REFRESH_COOKIE, "", { path: REFRESH_COOKIE_PATH, httpOnly: true, secure, maxAge: 0 }),
    serializeCookie(HINT_COOKIE, "", { path: "/", httpOnly: false, secure, maxAge: 0 }),
  ];
}

export function readCookie(header: string | null | undefined, name: string): string | null {
  if (!header) return null;
  for (const part of header.split(";")) {
    const index = part.indexOf("=");
    if (index < 0) continue;
    if (part.slice(0, index).trim() === name) {
      try {
        return decodeURIComponent(part.slice(index + 1).trim());
      } catch {
        return null;
      }
    }
  }
  return null;
}

type HeaderReader = { get(name: string): string | null };

/** Whether the request reached us over HTTPS (directly or through the TLS-terminating proxy). */
export function isSecureRequest(headers: HeaderReader, requestUrl: string): boolean {
  if (headers.get("x-forwarded-proto")?.split(",")[0].trim() === "https") return true;
  try {
    return new URL(requestUrl).protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * CSRF / login-CSRF guard: a session request must come from this origin's own script —
 * the custom header (no simple cross-site request can carry it), `Sec-Fetch-Site:
 * same-origin` when the browser sends it, and an `Origin` equal to this host.
 */
export function isSameOriginSessionRequest(headers: HeaderReader, requestUrl: string): boolean {
  if (headers.get(SESSION_REQUEST_HEADER) !== "1") return false;
  const site = headers.get("sec-fetch-site");
  if (site !== null && site !== "same-origin") return false;
  const origin = headers.get("origin");
  if (origin === null) return site === "same-origin";
  const allowed = new Set<string>();
  try {
    allowed.add(new URL(requestUrl).origin);
  } catch {
    /* ignore */
  }
  const host = headers.get("x-forwarded-host")?.split(",")[0].trim() || headers.get("host");
  if (host) {
    const proto = headers.get("x-forwarded-proto")?.split(",")[0].trim() || (requestUrl.startsWith("https:") ? "https" : "http");
    allowed.add(`${proto}://${host}`);
  }
  return allowed.has(origin);
}

/** What the session route does with the account authority's refresh answer. */
export function refreshOutcome(status: number): "rotated" | "ended" | "rate_limited" | "unavailable" {
  if (status >= 200 && status < 300) return "rotated";
  // 400 (malformed / rejected token) and 401 (expired, reused, revoked): sign in again.
  if (status === 400 || status === 401) return "ended";
  if (status === 429) return "rate_limited";
  return "unavailable";
}

/** `{base}/auth/refresh` from the configured v1 API URL. */
export function accountRefreshUrl(apiUrl: string | undefined): string {
  const base = (apiUrl || "http://localhost:3001/api/v1").replace(/\/+$/, "");
  return `${base}/auth/refresh`;
}

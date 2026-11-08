/**
 * Account session model of the web app (Blueprint v1.0 Section 17, ACC-019).
 *
 * - The access token lives in memory only (this module's closure). It is never written to
 *   a cookie, localStorage or sessionStorage.
 * - The refresh token lives only in an httpOnly, SameSite=Strict cookie scoped to
 *   `/api/session`, set and rotated by this app's own route handlers
 *   (`src/app/api/session/*`). Script never sees it after sign-in.
 * - A non-secret hint cookie (`cfm_session=persistent|session`) tells script that a
 *   session cookie exists, so a reload can mint a new access token with
 *   `POST /api/session/refresh` without a network round trip for signed-out visitors.
 * - Refresh is single-flight per tab and serialized across tabs with the Web Locks API
 *   (refresh tokens are single-use: two parallel refreshes would end the session, ADR 0057).
 *
 * Pure: no imports, so node tests run it directly (tests/account-session.test.mjs).
 */

export type SessionFetch = (
  url: string,
  init: {
    method: string;
    headers: Record<string, string>;
    body?: string;
    credentials: "same-origin";
    cache: "no-store";
    redirect: "error";
  },
) => Promise<{ ok: boolean; status: number; json(): Promise<unknown> }>;

export interface SessionLocks {
  request<T>(name: string, callback: () => Promise<T>): Promise<T>;
}

export interface LegacyTokenCookies {
  /** The pre-Section 17 JS-readable cookies, if any. */
  read(): { accessToken?: string | null; refreshToken?: string | null; remember: boolean } | null;
  clear(): void;
}

export interface SessionManagerOptions {
  fetchImpl: SessionFetch;
  /** Value of the non-secret hint cookie (`persistent` / `session`), or null. */
  readHint: () => string | null;
  /** navigator.locks when available. */
  locks?: () => SessionLocks | null;
  legacy?: LegacyTokenCookies | null;
  now?: () => number;
  basePath?: string;
}

/** The session cannot be continued: the user must sign in again. */
export class SessionEndedError extends Error {
  readonly code = "SESSION_ENDED";
  constructor(message = "Your session has ended. Sign in again.") {
    super(message);
    this.name = "SessionEndedError";
  }
}

/** The session service could not be reached; the session may still be valid (retry). */
export class SessionUnavailableError extends Error {
  readonly code = "SESSION_UNAVAILABLE";
  readonly status: number;
  constructor(status = 0) {
    super("The sign-in service is temporarily unavailable. Try again.");
    this.status = status;
    this.name = "SessionUnavailableError";
  }
}

export const REFRESH_LOCK_NAME = "cfmeet-account-session-refresh";
/** An access token closer than this to its expiry is refreshed before use. */
export const ACCESS_TOKEN_SKEW_MS = 30_000;

const JWT = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/;

/** Whether a value has the compact JWS shape (three base64url segments, bounded length). */
export function looksLikeJwt(value: unknown): value is string {
  return typeof value === "string" && value.length <= 4096 && JWT.test(value);
}

function base64UrlDecode(segment: string): string {
  const padded = segment.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((segment.length + 3) % 4);
  if (typeof atob === "function") return atob(padded);
  // Node without atob is not supported by the app; tests run on Node 22+.
  throw new Error("base64 decoding unavailable");
}

/** The `exp` claim (seconds since the epoch) of a JWT, without verifying it. */
export function jwtExpiry(token: string): number | null {
  if (!looksLikeJwt(token)) return null;
  try {
    const claims = JSON.parse(base64UrlDecode(token.split(".")[1])) as { exp?: unknown };
    return typeof claims.exp === "number" && Number.isFinite(claims.exp) ? claims.exp : null;
  } catch {
    return null;
  }
}

/**
 * Why the signed-in state changed: `signed_in` (establish), `signed_out` (this tab's
 * clear()), `ended` (the refresh cookie was rejected: expired, revoked, password changed),
 * `signed_out_elsewhere` (another tab signed out).
 */
export type SessionChange = "signed_in" | "signed_out" | "ended" | "signed_out_elsewhere";
export type SessionListener = (signedIn: boolean, change: SessionChange) => void;

export function createSessionManager(options: SessionManagerOptions) {
  const base = options.basePath ?? "/api/session";
  const now = options.now ?? (() => Date.now());
  let accessToken: string | null = null;
  let inflight: Promise<string> | null = null;
  let legacyChecked = !options.legacy;
  const listeners = new Set<SessionListener>();

  const request = (method: string, body?: unknown) => ({
    method,
    headers: { "Content-Type": "application/json", "X-CF-Session": "1" },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin" as const,
    cache: "no-store" as const,
    redirect: "error" as const,
  });

  const notify = (signedIn: boolean, change: SessionChange) => {
    for (const listener of listeners) {
      try {
        listener(signedIn, change);
      } catch {
        /* a listener never breaks the session */
      }
    }
  };

  const usable = (token: string | null): token is string => {
    if (!token) return false;
    const exp = jwtExpiry(token);
    return exp === null || exp * 1000 - now() > ACCESS_TOKEN_SKEW_MS;
  };

  async function establish(access: string | null, refreshToken: string, remember: boolean): Promise<void> {
    if (!looksLikeJwt(refreshToken)) throw new SessionUnavailableError(0);
    let response;
    try {
      response = await options.fetchImpl(base, request("POST", { refresh_token: refreshToken, remember }));
    } catch {
      throw new SessionUnavailableError(0);
    }
    if (!response.ok) throw new SessionUnavailableError(response.status);
    accessToken = looksLikeJwt(access) ? access : null;
    notify(true, "signed_in");
  }

  /** Moves pre-Section 17 JS-readable cookies into the httpOnly model once. */
  async function migrateLegacy(): Promise<void> {
    if (legacyChecked || !options.legacy) return;
    legacyChecked = true;
    const old = options.legacy.read();
    if (!old) return;
    if (!looksLikeJwt(old.refreshToken)) {
      options.legacy.clear();
      return;
    }
    try {
      await establish(old.accessToken ?? null, old.refreshToken, old.remember);
      options.legacy.clear();
    } catch {
      legacyChecked = false; // retry on the next request; the old cookies stay until then
    }
  }

  function refreshAccessToken(): Promise<string> {
    if (inflight) return inflight;
    const run = async (): Promise<string> => {
      let response;
      try {
        response = await options.fetchImpl(`${base}/refresh`, request("POST", {}));
      } catch {
        throw new SessionUnavailableError(0);
      }
      if (response.status === 401) {
        accessToken = null;
        notify(false, "ended");
        throw new SessionEndedError();
      }
      if (!response.ok) throw new SessionUnavailableError(response.status);
      const body = (await response.json().catch(() => null)) as { access_token?: unknown } | null;
      if (!body || !looksLikeJwt(body.access_token)) throw new SessionUnavailableError(response.status);
      accessToken = body.access_token;
      return accessToken;
    };
    const locks = options.locks?.() ?? null;
    inflight = (locks ? locks.request(REFRESH_LOCK_NAME, run) : run()).finally(() => {
      inflight = null;
    });
    return inflight;
  }

  return {
    /** Synchronous: is there (probably) a signed-in account session in this browser? */
    hasSession(): boolean {
      if (accessToken) return true;
      if (options.readHint()) return true;
      if (!legacyChecked && options.legacy) return looksLikeJwt(options.legacy.read()?.refreshToken);
      return false;
    },
    /** The in-memory access token when it is still usable, without any network call. */
    peekAccessToken(): string | null {
      return usable(accessToken) ? accessToken : null;
    },
    /** A usable access token, refreshing through the session cookie when needed; null when signed out. */
    async getAccessToken(): Promise<string | null> {
      await migrateLegacy();
      if (usable(accessToken)) return accessToken;
      if (!options.readHint()) {
        accessToken = null;
        return null;
      }
      try {
        return await refreshAccessToken();
      } catch (error) {
        if (error instanceof SessionEndedError) return null;
        throw error;
      }
    },
    refreshAccessToken,
    /** After sign-in (password, Google exchange): store the refresh token httpOnly, keep the access token in memory. */
    establish,
    /** Ends the browser side of the session (the API session is ended separately by /auth/logout). */
    async clear(): Promise<void> {
      accessToken = null;
      try {
        await options.fetchImpl(base, request("DELETE"));
      } catch {
        /* the cookies expire anyway; the API session was ended by the caller */
      }
      options.legacy?.clear();
      notify(false, "signed_out");
    },
    /** Drops the in-memory token without a network call (another tab signed out). */
    forget(): void {
      accessToken = null;
      notify(false, "signed_out_elsewhere");
    },
    subscribe(listener: SessionListener): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

export type SessionManager = ReturnType<typeof createSessionManager>;

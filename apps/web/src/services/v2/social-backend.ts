/**
 * TRANSITIONAL (one release): which backend serves Status / Channels.
 * TODO(#193 / ADR 0069 "Rollout"): remove this module, social.legacy.ts and
 * status-publisher/legacy-transport.ts after #193 is deployed to the backend.
 *
 * The web app deploys minutes after a merge; the backend about an hour later. In between the
 * new client calls `/api/v2/messaging/relay/v1/messaging/status|channels/...`, which the old
 * Meeting relay refuses as a route it does not allow (`404 ENDPOINT_NOT_FOUND`), while the old
 * `/api/v2/social/*` endpoints still work. After the backend deploys it is the other way round:
 * `/api/v2/social/*` is gone (the Meeting router answers `404 ENDPOINT_NOT_FOUND`).
 *
 * Only that exact answer switches backends. A Messaging `404 NOT_FOUND` (a status you may not
 * see, a deleted channel), a `403` privacy denial or any other error is the real answer and is
 * never retried elsewhere. The choice is remembered for the session.
 *
 * Dependency-free on purpose (unit-tested under node --experimental-strip-types).
 */

export type SocialBackend = "messaging" | "legacy";

/** True only for the Meeting Service's "this route is not served here" answer. */
export function isRouteNotAllowed(e: unknown): boolean {
  if (!e || typeof e !== "object") return false;
  const err = e as { status?: unknown; code?: unknown; response?: { status?: unknown; data?: { error?: { code?: unknown } } } };
  const status = typeof err.status === "number" ? err.status : err.response?.status;
  const code = typeof err.code === "string" && err.code === "ENDPOINT_NOT_FOUND" ? err.code : err.response?.data?.error?.code;
  return status === 404 && code === "ENDPOINT_NOT_FOUND";
}

interface SessionStore { getItem(key: string): string | null; setItem(key: string, value: string): void }

const KEY = "cfmeet.social.backend";

function defaultStore(): SessionStore | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

export interface BackendSwitch {
  current(): SocialBackend;
  set(backend: SocialBackend): void;
  /**
   * Run an operation on the current backend. `legacy` is the same operation on
   * `/api/v2/social/*` (or a safe empty answer for a Messaging-only read); omitted when there
   * is no equivalent, in which case the Messaging call's own error is returned.
   */
  run<T>(messaging: () => Promise<T>, legacy?: () => Promise<T>): Promise<T>;
}

export function createBackendSwitch(store: SessionStore | null = defaultStore()): BackendSwitch {
  let mode: SocialBackend = store?.getItem(KEY) === "legacy" ? "legacy" : "messaging";
  const set = (backend: SocialBackend) => {
    mode = backend;
    try { store?.setItem(KEY, backend); } catch { /* private mode: memory only */ }
  };
  return {
    current: () => mode,
    set,
    async run<T>(messaging: () => Promise<T>, legacy?: () => Promise<T>): Promise<T> {
      if (mode === "legacy" && legacy) {
        try {
          return await legacy();
        } catch (e) {
          if (!isRouteNotAllowed(e)) throw e;
          set("messaging"); // the backend deployed mid-session
          return messaging();
        }
      }
      try {
        const out = await messaging();
        if (mode === "legacy") set("messaging");
        return out;
      } catch (e) {
        if (!legacy || !isRouteNotAllowed(e)) throw e;
        set("legacy"); // the backend has not deployed yet
        return legacy();
      }
    },
  };
}

/** The app's switch (one per page / session). */
export const socialBackend = createBackendSwitch();

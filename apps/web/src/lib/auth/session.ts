/**
 * The browser's account session (ACC-019): one instance per tab built on session-core.
 * See src/lib/auth/session-core.ts for the model and docs/frontend/section-17/
 * web-account-organizations.md for the handoff description.
 */
import Cookies from "js-cookie";
import { createSessionManager, type SessionLocks } from "./session-core";
import { HINT_COOKIE, readCookie } from "./session-cookies";

/** Pre-Section 17 cookie names (JS-readable); read once to migrate, then removed. */
const LEGACY_ACCESS = "access_token";
const LEGACY_REFRESH = "refresh_token";
const LEGACY_REMEMBER = "remember_me";

const browser = () => typeof window !== "undefined" && typeof document !== "undefined";

export const accountSession = createSessionManager({
  fetchImpl: (url, init) => fetch(url, init),
  readHint: () => (browser() ? readCookie(document.cookie, HINT_COOKIE) : null),
  locks: () => {
    if (!browser() || !("locks" in navigator) || !navigator.locks) return null;
    const locks = navigator.locks;
    return { request: (name, callback) => locks.request(name, callback) } as SessionLocks;
  },
  legacy: {
    read() {
      if (!browser()) return null;
      const accessToken = Cookies.get(LEGACY_ACCESS) ?? null;
      const refreshToken = Cookies.get(LEGACY_REFRESH) ?? null;
      if (!accessToken && !refreshToken) return null;
      let remember = false;
      try {
        remember = window.localStorage.getItem(LEGACY_REMEMBER) === "true";
      } catch {
        /* storage disabled */
      }
      return { accessToken, refreshToken, remember };
    },
    clear() {
      if (!browser()) return;
      Cookies.remove(LEGACY_ACCESS);
      Cookies.remove(LEGACY_REFRESH);
      Cookies.remove(LEGACY_REMEMBER);
      try {
        window.localStorage.removeItem(LEGACY_REMEMBER);
      } catch {
        /* storage disabled */
      }
    },
  },
});

// Sign-out in one tab ends the in-memory token of the others (the cookies are shared).
const CHANNEL = "cfmeet-account-session";
let channel: BroadcastChannel | null = null;
if (browser() && typeof BroadcastChannel !== "undefined") {
  channel = new BroadcastChannel(CHANNEL);
  channel.onmessage = (event: MessageEvent) => {
    if (event.data === "signed-out") accountSession.forget();
  };
}

/** Ends this browser's session state everywhere in this browser (all tabs). */
export async function clearBrowserSession(): Promise<void> {
  await accountSession.clear();
  channel?.postMessage("signed-out");
}

export { SessionEndedError, SessionUnavailableError } from "./session-core";

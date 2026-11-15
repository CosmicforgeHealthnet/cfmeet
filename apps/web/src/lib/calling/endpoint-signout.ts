/**
 * This browser's Calling endpoint at logout (S-10, Notification Service roadmap N0-11).
 *
 * - The id of the endpoint this browser registered is remembered per principal
 *   (`cfmeet.calling.endpoint-id.<principalId>`), next to the stable endpointKey.
 * - Logout revokes every remembered endpoint (`DELETE /v1/calling/endpoints/{id}` through
 *   the caller's revoke function) and forgets the ids, while the session still exists.
 *   Best effort and bounded: logout never fails or waits longer than `timeoutMs` because
 *   of it.
 * - A short-lived "signing out" marker stops this browser (any tab) from registering the
 *   endpoint again while logout runs (a revoked endpoint's socket closes with 4401, which
 *   otherwise re-registers); the next sign-in clears it.
 *
 * Pure: no imports, so node tests run it directly (tests/calling-signout.test.mjs).
 */

export interface KeyValueStorage {
  readonly length: number;
  key(index: number): string | null;
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export const ENDPOINT_ID_PREFIX = "cfmeet.calling.endpoint-id.";
export const SIGNING_OUT_KEY = "cfmeet.calling.signing-out";
/** A marker older than this no longer blocks registration (a logout that never finished). */
export const SIGNING_OUT_MAX_AGE_MS = 60_000;
export const ENDPOINT_SIGNOUT_TIMEOUT_MS = 3_000;

export function rememberEndpointId(storage: KeyValueStorage | null, principalId: string, endpointId: string): void {
  try { storage?.setItem(ENDPOINT_ID_PREFIX + principalId, endpointId); } catch { /* storage disabled */ }
}

export function forgetEndpointId(storage: KeyValueStorage | null, principalId: string): void {
  try { storage?.removeItem(ENDPOINT_ID_PREFIX + principalId); } catch { /* storage disabled */ }
}

/** Whether a logout is in progress in this browser (registration must wait for the next sign-in). */
export function isSigningOut(storage: KeyValueStorage | null, now: number = Date.now()): boolean {
  try {
    const at = Number(storage?.getItem(SIGNING_OUT_KEY));
    return Number.isFinite(at) && at > 0 && now - at >= 0 && now - at < SIGNING_OUT_MAX_AGE_MS;
  } catch {
    return false;
  }
}

/** A fresh sign-in: this browser may register its endpoint again. */
export function clearSigningOut(storage: KeyValueStorage | null): void {
  try { storage?.removeItem(SIGNING_OUT_KEY); } catch { /* storage disabled */ }
}

/** Takes (reads and removes) every remembered endpoint id. */
function takeEndpointIds(storage: KeyValueStorage): string[] {
  const keys: string[] = [];
  for (let i = 0; i < storage.length; i++) {
    const key = storage.key(i);
    if (key?.startsWith(ENDPOINT_ID_PREFIX)) keys.push(key);
  }
  const ids = new Set<string>();
  for (const key of keys) {
    const id = storage.getItem(key);
    if (id) ids.add(id);
    storage.removeItem(key);
  }
  return [...ids];
}

export interface EndpointSignOutOptions {
  storage: KeyValueStorage | null;
  /** Revokes one endpoint with the signed-in session (`DELETE /v1/calling/endpoints/{id}`). */
  revoke: (endpointId: string) => Promise<unknown>;
  timeoutMs?: number;
  now?: () => number;
}

/**
 * Revokes this browser's remembered Calling endpoint(s) and forgets their ids. Never throws
 * and resolves within `timeoutMs`; returns the ids it tried to revoke.
 */
export async function signOutCallingEndpoint(options: EndpointSignOutOptions): Promise<string[]> {
  const { storage, revoke } = options;
  if (!storage) return [];
  let ids: string[] = [];
  try {
    storage.setItem(SIGNING_OUT_KEY, String((options.now ?? Date.now)()));
  } catch { /* storage disabled: nothing to read either */ }
  try {
    ids = takeEndpointIds(storage);
  } catch {
    return [];
  }
  if (ids.length === 0) return ids;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const revoked = Promise.all(ids.map((id) => Promise.resolve().then(() => revoke(id)).catch(() => undefined)));
  const timeout = new Promise<void>((resolve) => { timer = setTimeout(resolve, options.timeoutMs ?? ENDPOINT_SIGNOUT_TIMEOUT_MS); });
  try {
    await Promise.race([revoked, timeout]);
  } finally {
    clearTimeout(timer);
  }
  return ids;
}

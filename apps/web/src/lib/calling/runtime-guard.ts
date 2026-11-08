/**
 * Calling adapter runtime guard (CAL-008, CAL-020, FE-010).
 *
 * - `CALLING_DEV_MODE=true` (fixed identity, mock reads) is honoured ONLY in a non-production
 *   build that has NO real platform credential. A production build, or any build holding
 *   `CALLING_PLATFORM_CREDENTIAL`, with the flag set fails closed: every calling request is
 *   refused with 503 CALLING_NOT_CONFIGURED, never silently authenticated.
 * - The Calling base URL (platform adapter AND signaling transport) must be an explicit
 *   https:// origin in production; a missing / plain-http value is refused. Development may
 *   fall back to the local Calling service.
 *
 * Dependency-free on purpose (node tests import it with --experimental-strip-types).
 */

export type CallingEnv = Record<string, string | undefined>;

export class CallingConfigurationError extends Error {
  readonly code = "CALLING_NOT_CONFIGURED";
  constructor(message: string) {
    super(message);
  }
}

export type CallingRuntimeMode = "live" | "dev";

const DEV_FALLBACK = "http://localhost:8090";

function isProductionBuild(env: CallingEnv): boolean {
  return env.NODE_ENV === "production";
}

/**
 * `dev` only for a development build without a real credential; `live` otherwise. Throws
 * when dev mode is requested where it must never apply.
 */
export function callingRuntimeMode(env: CallingEnv): CallingRuntimeMode {
  if (env.CALLING_DEV_MODE !== "true") return "live";
  if (isProductionBuild(env)) {
    throw new CallingConfigurationError("Calling development mode is not permitted in a production build.");
  }
  if (env.CALLING_PLATFORM_CREDENTIAL) {
    throw new CallingConfigurationError("Calling development mode cannot be combined with a platform credential.");
  }
  return "dev";
}

/** The Calling origin every server-side request (platform or signaling) is sent to. */
export function callingBaseUrl(env: CallingEnv): string {
  const configured = env.CALLING_API_URL || env.NEXT_PUBLIC_CALLING_API_URL || "";
  if (isProductionBuild(env)) {
    let url: URL;
    try {
      url = new URL(configured);
    } catch {
      throw new CallingConfigurationError("Calling API is not securely configured.");
    }
    if (url.protocol !== "https:" || url.username || url.password) {
      throw new CallingConfigurationError("Calling API is not securely configured.");
    }
    return configured.replace(/\/$/, "");
  }
  return (configured || DEV_FALLBACK).replace(/\/$/, "");
}

/**
 * Lifetime check of a short-lived Calling credential (signaling: ≤ 3600 s by the backend's
 * `CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS` cap; media likewise short). Returns the delay
 * in ms after which the client renews it (`marginMs` before expiry, at least `minMs`), or
 * null when the credential is malformed, already expired, or claims a longer life than
 * `maxLifetimeMs` (never trusted: renew immediately instead).
 */
export function renewalDelay(
  expiresAt: string,
  now: number,
  { marginMs = 15_000, minMs = 1_000, maxLifetimeMs = 3_600_000 + 60_000 } = {},
): number | null {
  const expiry = Date.parse(expiresAt);
  if (!Number.isFinite(expiry) || expiry <= now || expiry - now > maxLifetimeMs) return null;
  return Math.max(minMs, expiry - now - marginMs);
}

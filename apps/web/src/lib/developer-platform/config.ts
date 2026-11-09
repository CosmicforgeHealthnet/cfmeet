/**
 * Developer Portal configuration.
 *
 * NEXT_PUBLIC_DEVELOPER_API_BASE_URL is the public API Gateway origin (for example
 * https://api.cfmeet.com once the gateway is activated: PLT-005, decision D1 of
 * docs/operations/developer-platform-gateway-activation-checklist.md). Unset or invalid
 * means "Developer Platform not available in this environment": the portal then shows
 * that state and calls nothing. It never falls back to NEXT_PUBLIC_API_URL (the legacy
 * meeting-service API with its `/api/v1/api-keys`).
 *
 * NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED is the explicit switch: the portal calls the
 * gateway only when it is exactly "true" AND the base URL is valid. Default off, because
 * the gateway and the Developer Platform (with its Meeting Service session bridge) are
 * not deployed yet (PLT-005); an operator turns the portal on per environment.
 *
 * Pure module (node tests load it with --experimental-strip-types).
 */

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

export type UnavailableReason = "DISABLED" | "UNSET" | "INVALID" | "INSECURE" | "LEGACY_API";

export type DeveloperApiConfig = { available: true; baseUrl: string } | { available: false; reason: UnavailableReason };

/**
 * Validates a configured gateway base URL: HTTPS (plain HTTP only on a loopback host).
 * `enabled` is NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED; omitted (undefined) only checks the URL.
 */
export function resolveDeveloperApiConfig(raw: string | undefined | null, enabled?: string | null): DeveloperApiConfig {
  if (enabled !== undefined && (enabled ?? "").trim().toLowerCase() !== "true") {
    return { available: false, reason: "DISABLED" };
  }
  const value = (raw ?? "").trim();
  if (!value) return { available: false, reason: "UNSET" };
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return { available: false, reason: "INVALID" };
  }
  if (url.username || url.password || url.search || url.hash) return { available: false, reason: "INVALID" };
  const loopback = LOOPBACK.has(url.hostname);
  if (url.protocol !== "https:" && !(loopback && url.protocol === "http:")) {
    return { available: false, reason: "INSECURE" };
  }
  const path = url.pathname.replace(/\/+$/, "");
  // The legacy meeting-service API (`…/api/v1`, `…/api/v2`) is never a Developer
  // Platform origin: the console must not reach `/api/v1/api-keys` by misconfiguration.
  if (/^\/api(\/|$)/.test(path)) return { available: false, reason: "LEGACY_API" };
  return { available: true, baseUrl: `${url.origin}${path}` };
}

export const DEVELOPER_API_CONFIG_REASONS: Record<UnavailableReason, string> = {
  DISABLED: "The Developer Platform is switched off for this deployment (NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED is not \"true\").",
  UNSET: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL is not set for this deployment.",
  INVALID: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL is not a valid origin URL.",
  INSECURE: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL must use HTTPS.",
  LEGACY_API: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL points at the legacy meeting-service API, which is not the API Gateway.",
};

/**
 * The deployment's portal configuration. NEXT_PUBLIC_* values are inlined at build time,
 * so they must be read with literal property names (not process.env[name]).
 */
export function developerApiConfig(): DeveloperApiConfig {
  return resolveDeveloperApiConfig(
    process.env.NEXT_PUBLIC_DEVELOPER_API_BASE_URL,
    process.env.NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED ?? null,
  );
}

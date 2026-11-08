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
 * Pure module (node tests load it with --experimental-strip-types).
 */

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

export type DeveloperApiConfig =
  | { available: true; baseUrl: string }
  | { available: false; reason: "UNSET" | "INVALID" | "INSECURE" | "LEGACY_API" };

/** Validates a configured gateway base URL: HTTPS (plain HTTP only on a loopback host). */
export function resolveDeveloperApiConfig(raw: string | undefined | null): DeveloperApiConfig {
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

export const DEVELOPER_API_CONFIG_REASONS: Record<"UNSET" | "INVALID" | "INSECURE" | "LEGACY_API", string> = {
  UNSET: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL is not set for this deployment.",
  INVALID: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL is not a valid origin URL.",
  INSECURE: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL must use HTTPS.",
  LEGACY_API: "NEXT_PUBLIC_DEVELOPER_API_BASE_URL points at the legacy meeting-service API, which is not the API Gateway.",
};

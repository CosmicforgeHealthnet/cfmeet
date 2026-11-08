/**
 * API base URL resolution (FE-006).
 *
 * Pure and dependency-free so it can be unit-tested with `node --test` and reused by the
 * Expo config (`app.config.js`) logic it mirrors.
 *
 * Rules:
 * - The build environment wins: `EXPO_PUBLIC_API_URL` (inlined by Expo at bundle time),
 *   then `expo.extra.apiUrl` (which `app.config.js` itself derives from the environment),
 *   then the HTTPS production default. Nothing in source control hard-codes a LAN host.
 * - A release build (`!__DEV__`) never talks cleartext HTTP to a non-loopback host: such a
 *   URL is refused and the HTTPS production default is used instead.
 */

export const DEFAULT_API_URL = "https://api.cfmeet.com/api/v1";

export interface ApiUrlInputs {
  /** `process.env.EXPO_PUBLIC_API_URL` */
  envUrl?: string | null;
  /** `Constants.expoConfig.extra.apiUrl` */
  extraUrl?: string | null;
  /** `__DEV__` */
  isDev: boolean;
}

export interface ResolvedApiUrl {
  /** Base of the v1 API, e.g. `https://api.cfmeet.com/api/v1`. */
  apiUrl: string;
  /** Base of the v2 API, e.g. `https://api.cfmeet.com/api/v2`. */
  apiV2Url: string;
  /** Where `apiUrl` came from. */
  source: "env" | "extra" | "default";
  /** Set when a configured URL was refused (never contains credentials). */
  rejected?: { url: string; reason: string };
}

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "[::1]", "10.0.2.2"]);

interface ParsedUrl {
  protocol: string;
  username: string;
  password: string;
  hostname: string;
  host: string;
  pathname: string;
}

/**
 * Minimal absolute-URL parser. React Native's built-in `URL` does not implement the component
 * getters (`protocol`, `hostname`, …) on every engine version, so a security decision must not
 * depend on it.
 */
export function parseAbsoluteUrl(url: string): ParsedUrl | null {
  const match =
    /^([a-z][a-z0-9+.-]*:)\/\/(?:([^:@/?#]*)(?::([^@/?#]*))?@)?(\[[^\]]+\]|[^:/?#@]+)(?::(\d+))?([^?#]*)(?:[?#].*)?$/i.exec(
      url.trim()
    );
  if (!match) return null;
  const [, protocol, username = "", password = "", rawHost, port, pathname] = match;
  if (port !== undefined && (Number(port) < 1 || Number(port) > 65535)) return null;
  const hostname = rawHost.toLowerCase();
  return {
    protocol: protocol.toLowerCase(),
    username,
    password,
    hostname,
    host: port ? `${hostname}:${port}` : hostname,
    pathname: pathname || "/",
  };
}

const parse = parseAbsoluteUrl;

/** True for loopback hosts (including the Android emulator's host alias 10.0.2.2). */
export function isLoopbackHost(hostname: string): boolean {
  return LOOPBACK_HOSTS.has(hostname.toLowerCase());
}

/**
 * Why `url` may not be used by a build, or `null` when it is acceptable.
 * Release builds accept only `https:` (or `http:` to a loopback host, for local tooling).
 */
export function insecureApiUrlReason(url: string, isDev: boolean): string | null {
  const parsed = parse(url);
  if (!parsed) return "not a valid absolute URL";
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
    return `unsupported protocol ${parsed.protocol}`;
  }
  if (parsed.username || parsed.password) return "URL must not embed credentials";
  if (parsed.protocol === "http:" && !isDev && !isLoopbackHost(parsed.hostname)) {
    return "cleartext HTTP to a non-loopback host is refused in release builds";
  }
  return null;
}

function redact(url: string): string {
  const parsed = parse(url);
  if (!parsed) return "<invalid>";
  return `${parsed.protocol}//${parsed.host}${parsed.pathname}`;
}

function trimTrailingSlash(url: string): string {
  return url.replace(/\/+$/, "");
}

/** The v2 base that pairs with a v1 base (`…/api/v1` → `…/api/v2`). */
export function toV2BaseUrl(apiUrl: string): string {
  const base = trimTrailingSlash(apiUrl);
  if (/\/api\/v1$/.test(base)) return base.replace(/\/api\/v1$/, "/api/v2");
  if (/\/api$/.test(base)) return `${base}/v2`;
  return `${base}/api/v2`;
}

export function resolveApiUrl({ envUrl, extraUrl, isDev }: ApiUrlInputs): ResolvedApiUrl {
  const candidates: { url: string; source: "env" | "extra" }[] = [];
  if (envUrl && envUrl.trim()) candidates.push({ url: envUrl.trim(), source: "env" });
  if (extraUrl && extraUrl.trim()) candidates.push({ url: extraUrl.trim(), source: "extra" });

  let rejected: ResolvedApiUrl["rejected"];
  for (const candidate of candidates) {
    const reason = insecureApiUrlReason(candidate.url, isDev);
    if (reason) {
      rejected ??= { url: redact(candidate.url), reason };
      continue;
    }
    const apiUrl = trimTrailingSlash(candidate.url);
    return { apiUrl, apiV2Url: toV2BaseUrl(apiUrl), source: candidate.source, rejected };
  }
  return { apiUrl: DEFAULT_API_URL, apiV2Url: toV2BaseUrl(DEFAULT_API_URL), source: "default", rejected };
}

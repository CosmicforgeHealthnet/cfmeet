/** Keep login on v1 and the Unified Messaging bridge on v2 of the same API. */
export function messagingApiUrls(configured = "http://localhost:8080/api/v1") {
  const url = new URL(configured);
  const local = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if ((url.protocol !== "https:" && !(local && url.protocol === "http:")) ||
    url.username || url.password || url.search || url.hash || !/^\/api\/v[12]\/?$/.test(url.pathname)) {
    throw new Error("Expected a secure Meeting Service /api/v1 or /api/v2 URL.");
  }
  return { v1: `${url.origin}/api/v1`, v2: `${url.origin}/api/v2` };
}

/**
 * Browser-reachable Messaging realtime origin: NEXT_PUBLIC_MESSAGING_URL when set, else the
 * origin of the Meeting API URL (production serves /v1/realtime on the API origin). Local
 * development without an explicit URL keeps live updates off (the services run on
 * separate ports), as before. Only https origins (or explicit local http) are used.
 */
export function messagingRealtimeOrigin(configured?: string | null, apiUrl?: string | null): string {
  const explicit = (configured ?? "").trim();
  if (explicit) return explicit.replace(/\/+$/, "");
  try {
    const url = new URL((apiUrl ?? "").trim());
    if (url.protocol !== "https:" || url.username || url.password) return "";
    return url.origin;
  } catch {
    return "";
  }
}

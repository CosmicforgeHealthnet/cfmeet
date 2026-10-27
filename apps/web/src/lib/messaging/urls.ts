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

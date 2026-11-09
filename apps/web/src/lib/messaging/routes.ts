export type MessagingMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
const id = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
/** One reaction path segment (an emoji sequence or a :short_code:, URL-encoded). */
const reaction = "[^/?#\\s.]{1,192}";
const page = ["limit", "cursor"];
const conversations = ["lifecycleState", "contextClass", "limit", "cursor", "orderBy"];
const messages = ["afterSequence", "beforeSequence", "limit", "threadRootId"];
const c = `conversations/${id}`;
const routes: [MessagingMethod, string, readonly string[]][] = [
  ["GET", "conversations", conversations], ["POST", "conversations", []],
  ["GET", c, []], ["PATCH", c, []],
  ["GET", `${c}/members`, page], ["POST", `${c}/members`, []],
  ["DELETE", `${c}/members/${id}`, ["reason"]], ["PATCH", `${c}/members/${id}`, []],
  ["GET", `${c}/messages`, messages], ["POST", `${c}/messages`, []],
  ["GET", `${c}/messages/${id}`, []], ["PATCH", `${c}/messages/${id}`, []],
  ["DELETE", `${c}/messages/${id}`, ["scope"]],
  ["GET", `${c}/messages/${id}/revisions`, []], ["GET", `${c}/messages/${id}/thread`, []],
  ["GET", `${c}/messages/${id}/reactions`, []],
  ["PUT", `${c}/messages/${id}/reactions/${reaction}`, []], ["DELETE", `${c}/messages/${id}/reactions/${reaction}`, []],
  ["GET", `${c}/pins`, []], ["PUT", `${c}/pins/${id}`, []], ["DELETE", `${c}/pins/${id}`, []],
  ["PUT", `${c}/read-cursor`, []], ["GET", `${c}/unread`, []],
  // Messaging's canonical replay cursor is `after` (the relay still rewrites the old
  // `afterEventSequence` spelling, MSG-001).
  ["GET", `${c}/events`, ["after", "afterEventSequence", "limit"]],
  ["GET", `${c}/search`, ["q", "limit", "beforeSequence"]], ["GET", "messaging/search", ["q", "limit"]],
  ["POST", `${c}/reports`, []],
  ["GET", `${c}/bans`, []], ["POST", `${c}/bans`, []], ["DELETE", `${c}/bans/${id}`, []],
  ["POST", `${c}/invitation/accept`, []], ["POST", `${c}/invitation/decline`, []],
  ["GET", "messaging/invitations", []],
  ["POST", `${c}/mls/commits`, []], ["GET", `${c}/mls/state`, []],
  ["POST", `${c}/mls/key-package-claims`, []],
  ["GET", `messaging/devices/${id}/welcomes`, []], ["POST", `messaging/devices/${id}/welcomes/${id}/consume`, []],
  ["GET", `messaging/principals/${id}/devices`, []],
  ["GET", "messaging/key-transparency/proofs", ["deviceId", "fromTreeSize"]],
  ["POST", `${c}/attachments`, []], ["GET", `${c}/attachments/${id}`, []],
  ["POST", `${c}/attachments/${id}/finalize`, []],
  ["POST", `${c}/attachments/${id}/download-grants`, []],
  ["POST", `${c}/attachments/${id}/thumbnail-grants`, []],
  ["POST", "messaging/history-transfers", []], ["GET", "messaging/history-transfers", ["deviceId"]],
  ["GET", `messaging/history-transfers/${id}`, []],
  ["PUT", `messaging/history-transfers/${id}/sealed-history`, []],
  ["GET", `messaging/history-transfers/${id}/sealed-history`, []],
  ["POST", `messaging/history-transfers/${id}/complete`, []], ["POST", `messaging/history-transfers/${id}/cancel`, []],
  ["GET", "messaging/blocks", []], ["POST", "messaging/blocks", []], ["DELETE", `messaging/blocks/${id}`, []],
  ["POST", `messaging/client-sessions/${id}/revoke`, []],
  ["GET", "messaging/presence/settings", []], ["PUT", "messaging/presence/settings", []],
  ["PUT", `messaging/devices/${id}/push-registration`, []], ["DELETE", `messaging/devices/${id}/push-registration`, []],
  ["POST", "messaging/backups", []], ["GET", "messaging/backups/latest", []], ["DELETE", `messaging/backups/${id}`, []],
  ["POST", `messaging/backups/${id}/finalize`, []], ["POST", `messaging/backups/${id}/download-grants`, []],
];

/** Mirror Meeting's end-user allowlist (`relay.rs` ROUTES); the backend remains the authority. */
export function isMessagingRelayRoute(method: MessagingMethod, path: string): boolean {
  if (path.includes("#") || path.split("?").length > 2) return false;
  const [pathname, query] = path.split("?");
  if (pathname.includes("..") || /%2e/i.test(pathname)) return false;
  const route = routes.find(([verb, pattern]) => verb === method && new RegExp(`^v1/${pattern}$`).test(pathname));
  if (!route) return false;
  const params = [...new URLSearchParams(query)];
  return params.length <= 8 && params.every(([key, value]) => route[2].includes(key) && key.length <= 32 && value.length <= 256);
}

/** The path segment of a reaction (encoded; the relay refuses delimiters and dots). */
export function reactionSegment(value: string): string {
  return encodeURIComponent(value);
}

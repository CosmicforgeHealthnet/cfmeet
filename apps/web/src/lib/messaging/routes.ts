export type MessagingMethod = "GET" | "POST" | "PUT" | "DELETE";
const id = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const page = ["limit", "cursor"];
const routes: [MessagingMethod, string, readonly string[]][] = [
  ["GET", "conversations", page], ["POST", "conversations", []],
  ["GET", `conversations/${id}`, []],
  ["GET", `conversations/${id}/members`, page], ["POST", `conversations/${id}/members`, []],
  ["DELETE", `conversations/${id}/members/${id}`, []],
  ["GET", `conversations/${id}/messages`, ["afterSequence", "beforeSequence", "limit"]],
  ["POST", `conversations/${id}/messages`, []], ["GET", `conversations/${id}/messages/${id}`, []],
  ["DELETE", `conversations/${id}/messages/${id}`, []],
  ["PUT", `conversations/${id}/read-cursor`, []], ["GET", `conversations/${id}/unread`, []],
  ["GET", `conversations/${id}/events`, ["afterEventSequence", "limit"]],
  ["POST", `conversations/${id}/mls/commits`, []], ["GET", `conversations/${id}/mls/state`, []],
  ["POST", `conversations/${id}/mls/key-package-claims`, []],
  ["GET", `messaging/devices/${id}/welcomes`, []], ["POST", `messaging/devices/${id}/welcomes/${id}/consume`, []],
  ["GET", `messaging/principals/${id}/devices`, []],
  ["GET", "messaging/key-transparency/proofs", ["deviceId", "fromTreeSize"]],
  ["POST", `conversations/${id}/attachments`, []], ["GET", `conversations/${id}/attachments/${id}`, []],
  ["POST", `conversations/${id}/attachments/${id}/finalize`, []],
  ["POST", `conversations/${id}/attachments/${id}/download-grants`, []],
  ["POST", `conversations/${id}/attachments/${id}/thumbnail-grants`, []],
  ["POST", "messaging/history-transfers", []], ["GET", "messaging/history-transfers", ["deviceId"]],
  ["GET", `messaging/history-transfers/${id}`, []],
  ["PUT", `messaging/history-transfers/${id}/sealed-history`, []],
  ["GET", `messaging/history-transfers/${id}/sealed-history`, []],
  ["POST", `messaging/history-transfers/${id}/complete`, []], ["POST", `messaging/history-transfers/${id}/cancel`, []],
];

/** Mirror Meeting's end-user allowlist; the backend remains the authority. */
export function isMessagingRelayRoute(method: MessagingMethod, path: string): boolean {
  if (path.includes("#") || path.split("?").length > 2) return false;
  const [pathname, query] = path.split("?");
  const route = routes.find(([verb, pattern]) => verb === method && new RegExp(`^v1/${pattern}$`).test(pathname));
  if (!route) return false;
  const params = [...new URLSearchParams(query)];
  return params.length <= 8 && params.every(([key, value]) => route[2].includes(key) && key.length <= 32 && value.length <= 128);
}

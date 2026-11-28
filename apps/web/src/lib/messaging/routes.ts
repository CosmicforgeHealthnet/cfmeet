export type MessagingMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
const id = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
/** One reaction path segment (an emoji sequence or a :short_code:, URL-encoded). */
const reaction = "[^/?#\\s.]{1,192}";
const page = ["limit", "cursor"];
const conversations = ["lifecycleState", "contextClass", "limit", "cursor", "orderBy"];
const messages = ["afterSequence", "beforeSequence", "limit", "threadRootId"];
const c = `conversations/${id}`;
const st = "messaging/status";
const ch = "messaging/channels";
/** A media part number (0-63, canonical decimal). */
const part = "(?:[0-9]|[1-5][0-9]|6[0-3])";
type Route = [MessagingMethod, string, readonly string[]];
const routes: Route[] = [
  ["GET", "conversations", conversations], ["POST", "conversations", []],
  ["GET", c, []], ["PATCH", c, []],
  ["POST", `${c}/messages/${id}/forward`, []],
  ["GET", `${c}/invite-links`, []], ["POST", `${c}/invite-links`, []], ["POST", `${c}/invite-links/${id}/revoke`, []],
  ["POST", "messaging/invite-links/join", []],
  ["GET", `${c}/members`, page], ["POST", `${c}/members`, []],
  ["DELETE", `${c}/members/${id}`, ["reason"]], ["PATCH", `${c}/members/${id}`, []],
  ["GET", `${c}/messages`, messages], ["POST", `${c}/messages`, []],
  ["GET", `${c}/messages/${id}`, []], ["PATCH", `${c}/messages/${id}`, []],
  ["DELETE", `${c}/messages/${id}`, ["scope"]],
  ["GET", `${c}/messages/${id}/revisions`, []], ["GET", `${c}/messages/${id}/thread`, []],
  ["GET", `${c}/messages/${id}/reactions`, []],
  ["PUT", `${c}/messages/${id}/reactions/${reaction}`, []], ["DELETE", `${c}/messages/${id}/reactions/${reaction}`, []],
  ["GET", `${c}/pins`, []], ["PUT", `${c}/pins/${id}`, []], ["DELETE", `${c}/pins/${id}`, []],
  ["PUT", `${c}/read-cursor`, []], ["PUT", `${c}/inbox-settings`, []], ["POST", `${c}/owner`, []], ["GET", `${c}/unread`, []],
  // The member's own chat lists and MANAGED polls (Messaging owns both).
  ["GET", "messaging/chat-lists", []], ["POST", "messaging/chat-lists", []],
  ["PATCH", `messaging/chat-lists/${id}`, []], ["DELETE", `messaging/chat-lists/${id}`, []],
  ["PUT", `messaging/chat-lists/${id}/conversations/${id}`, []], ["DELETE", `messaging/chat-lists/${id}/conversations/${id}`, []],
  ["PUT", `${c}/messages/${id}/poll/vote`, []], ["POST", `${c}/messages/${id}/poll/close`, []],
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
  ["GET", "messaging/push/configuration", []],
  ["POST", "messaging/backups", []], ["GET", "messaging/backups/latest", []], ["DELETE", `messaging/backups/${id}`, []],
  ["POST", `messaging/backups/${id}/finalize`, []], ["POST", `messaging/backups/${id}/download-grants`, []],
  // ADR 0069: Status and Channels (owned by Messaging; Meeting only relays them).
  ["POST", st, []], ...(["feed", "mine", "audience", "eligible-contacts", "close-friends", "audience-lists", "mutes"] as const).map((x) => ["GET", `${st}/${x}`, []] as Route),
  ["POST", `${st}/delete`, []], ["POST", `${st}/rings`, []], ["POST", `${st}/audience-lists`, []],
  ["PUT", `${st}/audience`, []], ["PUT", `${st}/close-friends`, []],
  ["PATCH", `${st}/audience-lists/${id}`, []], ["DELETE", `${st}/audience-lists/${id}`, []],
  ["PUT", `${st}/mutes/${id}`, []], ["DELETE", `${st}/mutes/${id}`, []],
  ["GET", `${st}/${id}`, []], ["DELETE", `${st}/${id}`, []], ["POST", `${st}/${id}/view`, []],
  ["PUT", `${st}/${id}/reaction`, []], ["DELETE", `${st}/${id}/reaction`, []], ["GET", `${st}/${id}/viewers`, []],
  ["POST", `${st}/${id}/replies`, []], ["POST", `${st}/${id}/forwards`, []], ["POST", `${st}/${id}/reshares`, []],
  ...[st, ch].flatMap((base) => [
    ["POST", `${base}/media`, []], ["GET", `${base}/media/${id}`, []], ["DELETE", `${base}/media/${id}`, []],
    ["POST", `${base}/media/${id}/parts/${part}`, []], ["POST", `${base}/media/${id}/complete`, []],
    ["POST", `${base}/media/${id}/download-grants`, []],
  ] as Route[]),
  ["GET", ch, ["q"]], ["POST", ch, []], ["GET", `${ch}/following`, []],
  ["GET", `${ch}/${id}`, []], ["PATCH", `${ch}/${id}`, []], ["DELETE", `${ch}/${id}`, []],
  ["PUT", `${ch}/${id}/follow`, []], ["DELETE", `${ch}/${id}/follow`, []],
  ["GET", `${ch}/${id}/posts`, ["before"]], ["POST", `${ch}/${id}/posts`, []],
  ["DELETE", `${ch}/${id}/posts/${id}`, []], ["PUT", `${ch}/${id}/posts/${id}/reaction`, []], ["DELETE", `${ch}/${id}/posts/${id}/reaction`, []],
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

/**
 * What a chat row shows beyond its last message (WhatsApp, brief §3 and §18): a status reply
 * or share with a status icon and its context, a call as the latest activity (voice / video,
 * missed), a reaction to your status, "mentioned you in a status", and what a group says
 * before its first message. Everything here comes from what the client already holds:
 * decrypted or server-stored message text it already shows, the Hub's latest-activity card
 * (ids, kinds and times, never content), and the status rings / "My status" it already
 * loads. Nothing is sent anywhere and no content leaves the device.
 *
 * Pure (no imports): node tests run this file directly (tests/chat-indicators.test.mjs).
 */

/** The server writes a status reply as this chat message (services/messaging status.rs). */
export const STATUS_REPLY_PREFIX = "Replying to your status (";
const SHARE = /^Shared a status with you: cfmeet:\/\/status\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i;

export type StatusMessage =
  /** A private reply to a status: `what` the status was (photo, video, voice note or “its text”) and the reply. */
  | { kind: "status-reply"; what: string; text: string }
  /** A forwarded status (it still opens only for people in its audience). */
  | { kind: "status-share"; statusId: string };

/** A chat message the server wrote for a status reply or share; null for any other text. */
export function statusMessage(text: string | null | undefined): StatusMessage | null {
  if (!text) return null;
  if (text.startsWith(STATUS_REPLY_PREFIX)) {
    const rest = text.slice(STATUS_REPLY_PREFIX.length);
    const cut = rest.indexOf("): ");
    if (cut < 0) return null;
    return { kind: "status-reply", what: rest.slice(0, cut), text: rest.slice(cut + 3) };
  }
  const share = SHARE.exec(text.trim());
  return share ? { kind: "status-share", statusId: share[1].toLowerCase() } : null;
}

/** The chat row's line for a status reply or share (yours or theirs). */
export function statusMessagePreview(m: StatusMessage, mine: boolean): { context: string; text: string } {
  if (m.kind === "status-share") return { context: "Status", text: mine ? "You shared a status" : "Shared a status with you" };
  return { context: mine ? "You replied to a status" : `Replied to your status · ${m.what}`, text: m.text };
}

// ------------------------------------------------------------------ calls

/** The Hub's latest timeline card of a chat (`/inbox` latestActivity). */
export type LatestActivity = { entryId?: string; kind: string; occurredAt: string; missed?: boolean | null; callKind?: string | null } | null | undefined;

export type CallPreview = { video: boolean; missed: boolean; label: string; at: string };

/** A call card as the row's preview ("Missed voice call", "Video call"); null for anything else. */
export function callPreview(a: LatestActivity): CallPreview | null {
  if (!a || a.kind !== "CALL" || !a.occurredAt) return null;
  const video = a.callKind === "VIDEO";
  const missed = a.missed === true;
  return { video, missed, at: a.occurredAt, label: `${missed ? "Missed " : ""}${video ? "video" : "voice"} call`.replace(/^v/, "V") };
}

// ------------------------------------------------------------------ status reactions and mentions

/** One viewer of one of MY live statuses (from "My status"). */
export type MyStatusViewer = { principalId: string; reaction: string | null; reactedAt?: string | null };

export type StatusReaction = { emoji: string; at: string };

/** `principalId`'s latest reaction to one of my live statuses (newest first), or null. */
export function latestReactionBy(principalId: string | null | undefined, posts: readonly { viewers?: readonly MyStatusViewer[] }[]): StatusReaction | null {
  if (!principalId) return null;
  let best: StatusReaction | null = null;
  for (const p of posts) {
    for (const v of p.viewers ?? []) {
      if (v.principalId !== principalId || !v.reaction || !v.reactedAt) continue;
      if (!best || v.reactedAt > best.at) best = { emoji: v.reaction, at: v.reactedAt };
    }
  }
  return best;
}

// ------------------------------------------------------------------ what the row shows

export type RowOverlay =
  | { kind: "call"; call: CallPreview }
  | { kind: "status-reaction"; reaction: StatusReaction };

const time = (iso: string | null | undefined) => {
  const t = iso ? Date.parse(iso) : NaN;
  return Number.isFinite(t) ? t : -Infinity;
};

/**
 * Whether something newer than the chat's last message replaces its preview: the latest
 * call (any chat) or the other person's reaction to your status (a direct chat). A typing
 * indicator or an unsent draft still wins over both (the caller decides that).
 */
export function rowOverlay(args: { lastMessageAt: string | null; call: CallPreview | null; reaction: StatusReaction | null }): RowOverlay | null {
  const base = time(args.lastMessageAt);
  const call = args.call && time(args.call.at) > base ? args.call : null;
  const reaction = args.reaction && time(args.reaction.at) > base ? args.reaction : null;
  if (call && reaction) return time(call.at) >= time(reaction.at) ? { kind: "call", call } : { kind: "status-reaction", reaction };
  if (call) return { kind: "call", call };
  if (reaction) return { kind: "status-reaction", reaction };
  return null;
}

/** When the row last saw activity: its last message, or a newer call / reaction (list order). */
export function activityAt(lastActivityAt: string, ...more: (string | null | undefined)[]): string {
  let best = lastActivityAt;
  for (const m of more) if (m && time(m) > time(best)) best = m;
  return best;
}

/**
 * The chats in list order once calls and status reactions count as activity: latest first,
 * stable for ties (the engine's realtime order is kept otherwise).
 */
export function orderByActivity<T extends { id: string; lastActivityAt: string }>(chats: readonly T[], extra: (c: T) => (string | null | undefined)[]): T[] {
  const keyed = chats.map((c, i) => ({ c, i, at: time(activityAt(c.lastActivityAt, ...extra(c))) }));
  keyed.sort((a, b) => b.at - a.at || a.i - b.i);
  return keyed.map((k) => k.c);
}

// ------------------------------------------------------------------ groups

/**
 * What a group with no message yet says (WhatsApp: "You created this group"), from the
 * conversation it already has; null when it has messages or is not a group.
 */
export function groupActivityPreview(c: { contextClass: string; ownerPrincipalId: string | null; lastMessage: unknown }, latestLoaded: boolean, me: string | null, names: Record<string, string>): string | null {
  if (c.contextClass !== "GROUP" || c.lastMessage || latestLoaded) return null;
  if (!c.ownerPrincipalId) return "Group created";
  if (c.ownerPrincipalId === me) return "You created this group";
  const who = names[c.ownerPrincipalId]?.split(" ")[0];
  return who ? `${who} created this group` : "Group created";
}

/**
 * Unified Messaging on mobile (Section 17, MSG): MANAGED conversations only.
 *
 * Every Messaging operation goes through the Meeting Service first-party bridge
 * (`/api/v2/messaging/relay/{path}`), which verifies the CF Meet login and acts for the
 * account's own Messaging principal. The relay forwards only an allowlist of Messaging client
 * paths (services/meeting-service/src/services/unified_messaging/relay.rs); this module builds
 * the subset mobile uses, validates ids and query keys, and maps server records to views.
 *
 * Security rules:
 * - MANAGED conversations are server-readable: the UI says so and never calls them encrypted.
 * - PRIVATE_E2EE conversations need a registered device with MLS keys (web only today). The
 *   app lists them truthfully but never shows, sends or "downgrades" their content.
 * - Requests carry only the account bearer (never a meeting participant credential).
 *
 * Pure and dependency-free (runs under `node --test`).
 */

export type SecurityProfile = "MANAGED" | "PRIVATE_E2EE";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export class RelayPathError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "RelayPathError";
  }
}

function id(value: string): string {
  if (!UUID.test(value)) throw new RelayPathError("Invalid conversation id");
  return value.toLowerCase();
}

function query(params: Record<string, string | number | null | undefined>): string {
  const parts: string[] = [];
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "") continue;
    parts.push(`${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  }
  return parts.length ? `?${parts.join("&")}` : "";
}

export const PAGE_SIZE = 50;

/** Allowlisted relay paths (relative to `/api/v2/messaging/relay/`). */
export const relayPaths = {
  conversations: (cursor?: string | null) =>
    `v1/conversations${query({ limit: PAGE_SIZE, cursor: cursor ?? undefined })}`,
  createConversation: () => "v1/conversations",
  conversation: (conversationId: string) => `v1/conversations/${id(conversationId)}`,
  members: (conversationId: string) => `v1/conversations/${id(conversationId)}/members${query({ limit: 100 })}`,
  addMember: (conversationId: string) => `v1/conversations/${id(conversationId)}/members`,
  messagesAfter: (conversationId: string, afterSequence: number) => {
    if (!Number.isInteger(afterSequence) || afterSequence < 0) throw new RelayPathError("Invalid sequence");
    return `v1/conversations/${id(conversationId)}/messages${query({ afterSequence, limit: PAGE_SIZE })}`;
  },
  sendMessage: (conversationId: string) => `v1/conversations/${id(conversationId)}/messages`,
  readCursor: (conversationId: string) => `v1/conversations/${id(conversationId)}/read-cursor`,
  unread: (conversationId: string) => `v1/conversations/${id(conversationId)}/unread`,
};

/** The relay allowlist subset (method + path pattern + permitted query keys) mobile may call. */
const MOBILE_ALLOWLIST: [string, RegExp, string[]][] = [
  ["GET", /^v1\/conversations$/, ["limit", "cursor"]],
  ["POST", /^v1\/conversations$/, []],
  ["GET", /^v1\/conversations\/[0-9a-f-]{36}$/, []],
  ["GET", /^v1\/conversations\/[0-9a-f-]{36}\/members$/, ["limit", "cursor"]],
  ["POST", /^v1\/conversations\/[0-9a-f-]{36}\/members$/, []],
  ["GET", /^v1\/conversations\/[0-9a-f-]{36}\/messages$/, ["afterSequence", "beforeSequence", "limit"]],
  ["POST", /^v1\/conversations\/[0-9a-f-]{36}\/messages$/, []],
  ["PUT", /^v1\/conversations\/[0-9a-f-]{36}\/read-cursor$/, []],
  ["GET", /^v1\/conversations\/[0-9a-f-]{36}\/unread$/, []],
];

export function isMobileRelayPath(method: string, path: string): boolean {
  if (path.includes("#") || path.split("?").length > 2) return false;
  const [pathname, qs = ""] = path.split("?");
  const rule = MOBILE_ALLOWLIST.find(([m, re]) => m === method && re.test(pathname));
  if (!rule) return false;
  const keys = qs ? qs.split("&").map((kv) => decodeURIComponent(kv.split("=")[0])) : [];
  return keys.every((k) => rule[2].includes(k));
}

// ---- Server records (cfmeet.messaging.http-api, fields mobile reads) -----------------------

export interface ServerConversation {
  conversationId: string;
  contextClass: string;
  securityProfile: SecurityProfile;
  lifecycleState: string;
  createdAt: string;
  encryption?: { mlsGroupId: string | null; serverCanReadContent: boolean } | null;
  members?: { principalId: string }[];
}

export interface ServerMessage {
  messageId: string;
  conversationId: string;
  sequence: number;
  kind: string;
  securityProfile: SecurityProfile;
  senderPrincipalId: string;
  clientMessageId: string;
  lifecycle: string;
  createdAt: string;
  attachmentIds?: string[];
  managed?: { text: string | null } | null;
}

export interface ConversationSummary {
  id: string;
  isDirect: boolean;
  profile: SecurityProfile;
  /** Whether mobile can read and send in it (MANAGED only). */
  readableHere: boolean;
  memberIds: string[];
  createdAt: string;
  active: boolean;
}

export function summarizeConversation(c: ServerConversation): ConversationSummary {
  const managed = c.securityProfile === "MANAGED";
  return {
    id: c.conversationId,
    isDirect: c.contextClass === "DIRECT",
    profile: c.securityProfile,
    readableHere: managed,
    memberIds: (c.members ?? []).map((m) => m.principalId),
    createdAt: c.createdAt,
    active: (c.lifecycleState ?? "ACTIVE").toUpperCase() === "ACTIVE",
  };
}

/** A conversation's title from its members' names (excluding the viewer). */
export function conversationTitle(
  summary: Pick<ConversationSummary, "memberIds" | "isDirect">,
  me: string | null,
  names: Record<string, string>
): string {
  const others = summary.memberIds.filter((p) => p !== me);
  const labels = others.map((p) => names[p] ?? "Unknown member");
  if (labels.length === 0) return summary.isDirect ? "Direct conversation" : "Group conversation";
  if (labels.length <= 3) return labels.join(", ");
  return `${labels.slice(0, 3).join(", ")} +${labels.length - 3}`;
}

export interface ChatLine {
  key: string;
  sequence: number;
  senderPrincipalId: string;
  mine: boolean;
  createdAt: string;
  text: string | null;
  removed: boolean;
  hasAttachment: boolean;
}

/** Maps a page of MANAGED messages to chat lines (application messages only, in order). */
export function toChatLines(messages: ServerMessage[], me: string | null): ChatLine[] {
  return messages
    .filter((m) => m.kind === "APPLICATION" && m.securityProfile === "MANAGED")
    .sort((a, b) => a.sequence - b.sequence)
    .map((m) => {
      const removed = (m.lifecycle ?? "ACTIVE").toUpperCase() !== "ACTIVE";
      return {
        key: `s${m.sequence}`,
        sequence: m.sequence,
        senderPrincipalId: m.senderPrincipalId,
        mine: !!me && m.senderPrincipalId === me,
        createdAt: m.createdAt,
        text: removed ? null : (m.managed?.text ?? null),
        removed,
        hasAttachment: (m.attachmentIds ?? []).length > 0,
      };
    });
}

/** Merges new lines into existing ones by sequence (idempotent for repeated polls). */
export function mergeChatLines(existing: ChatLine[], incoming: ChatLine[]): ChatLine[] {
  const bySeq = new Map<number, ChatLine>();
  for (const l of existing) bySeq.set(l.sequence, l);
  for (const l of incoming) bySeq.set(l.sequence, l);
  return [...bySeq.values()].sort((a, b) => a.sequence - b.sequence);
}

/** Body of `POST v1/conversations` for a MANAGED conversation (disclosure acknowledged by the user). */
export function managedConversationBody(principalIds: string[]): Record<string, unknown> {
  if (principalIds.length === 0) throw new RelayPathError("Choose at least one person");
  return principalIds.length === 1
    ? {
        contextClass: "DIRECT",
        counterpartPrincipalId: principalIds[0],
        securityProfile: "MANAGED",
        managedDisclosureAcknowledged: true,
      }
    : { contextClass: "GROUP", securityProfile: "MANAGED", managedDisclosureAcknowledged: true };
}

export const MANAGED_DISCLOSURE =
  "Managed conversation: messages are protected in transit and at rest, and CF Meet can read them " +
  "to provide the service (for example retention and moderation). They are not end-to-end encrypted.";

export const PRIVATE_ON_WEB =
  "This is a private (end-to-end encrypted) conversation. It can be read only on a device registered " +
  "for private messaging, which the mobile app does not support yet. Open it in CF Meet on the web.";

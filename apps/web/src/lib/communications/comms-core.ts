/**
 * Pure rules of the web Communications experience (Contacts, Chats, timeline): navigation
 * links, Hub sync → refetch mapping, capability gating, meeting share lines and the
 * personal / organization split of who you can start a conversation with.
 *
 * No runtime imports: node tests run this file directly (tests/communications-core.test.mjs).
 */

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const CF_MEET_ID = /^CF-[A-Z2-7]{26}$/;

export function isUuid(value: unknown): value is string {
  return typeof value === "string" && UUID.test(value);
}

/** A CF Meet ID as typed by a person: trimmed, upper-cased, `CF-` + 26 base32 characters. */
export function normalizeCfMeetId(input: string): string | null {
  const v = input.trim().toUpperCase().replace(/\s+/g, "");
  const withPrefix = v.startsWith("CF-") ? v : `CF-${v}`;
  return CF_MEET_ID.test(withPrefix) ? withPrefix : null;
}

// ------------------------------------------------------------------ navigation hooks

export type CallKind = "VOICE" | "VIDEO";

/**
 * "Call this contact / conversation" — the navigation contract handed to the web Calling
 * client, which owns call launch (`POST /contacts/{id}/calls` or
 * `POST /conversations/{id}/calls`), ringing and media. This module never launches a call.
 */
export function callHref(target: { contactId: string } | { conversationId: string }, kind: CallKind = "VOICE"): string {
  const params = new URLSearchParams();
  if ("contactId" in target) {
    if (!isUuid(target.contactId)) throw new Error("Invalid contact id");
    params.set("contactId", target.contactId);
  } else {
    if (!isUuid(target.conversationId)) throw new Error("Invalid conversation id");
    params.set("conversationId", target.conversationId);
  }
  params.set("kind", kind);
  return `/calling?${params.toString()}`;
}

/** Opens a conversation in Messages. */
export function chatHref(conversationId: string): string {
  if (!isUuid(conversationId)) throw new Error("Invalid conversation id");
  return `/dashboard/messages?conversation=${conversationId}`;
}

/** The meeting room route of the web app (`/{meetingCode}`). */
export function meetingJoinHref(meetingCode: string): string {
  const code = meetingCode.trim();
  if (!/^[A-Za-z0-9-]{3,64}$/.test(code)) throw new Error("Invalid meeting code");
  return `/${encodeURIComponent(code)}`;
}

/**
 * The connect-invite token of a Hub QR / invite payload (`{public app}/connect/{token}`).
 * Only a token from the expected path is accepted; anything else is refused.
 */
export function inviteTokenFromPayload(payload: string): string | null {
  try {
    const url = new URL(payload);
    const m = /^\/connect\/([A-Za-z0-9_-]{16,256})$/.exec(url.pathname);
    return m && !url.search && !url.hash ? m[1] : null;
  } catch {
    return /^[A-Za-z0-9_-]{16,256}$/.test(payload) ? payload : null;
  }
}

// ------------------------------------------------------------------ meeting share (MSG-009)

/**
 * The chat line a member's own client composes when it shares a meeting in a MANAGED chat
 * (the server never writes chat messages for a user). The Hub card is the server record; the
 * line is a convenience that carries only the meeting code.
 */
export function meetingShareLine(meetingCode: string, origin: "INSTANT" | "SHARED"): string {
  return `${origin === "INSTANT" ? "Started a meeting" : "Shared a meeting"}: ${meetingCode}`;
}

// ------------------------------------------------------------------ Hub sync (§4)

export type HubArea = "profile" | "contacts" | "requests" | "blocks" | "privacy" | "timeline" | "inbox" | "devices" | "notifications" | "meetings" | "saved";

/** What to refetch for each Hub change-feed domain. */
export function areasForDomains(domains: readonly string[]): Set<HubArea> {
  const out = new Set<HubArea>();
  for (const d of domains) {
    switch (d) {
      case "CONTACT":
        out.add("contacts");
        out.add("requests");
        break;
      case "CONTACT_LIST":
        out.add("contacts");
        break;
      case "BLOCK":
        out.add("blocks");
        out.add("contacts");
        break;
      case "PRIVACY":
        out.add("privacy");
        break;
      case "PROFILE":
        out.add("profile");
        break;
      case "TIMELINE":
        out.add("timeline");
        out.add("inbox");
        break;
      case "MEETING_LINK":
        out.add("meetings");
        out.add("timeline");
        break;
      case "CONVERSATION_PREFERENCE":
      case "CLEAR_FOR_ME":
        out.add("inbox");
        out.add("timeline");
        break;
      case "DEVICE":
        out.add("devices");
        break;
      case "NOTIFICATION_PREFERENCE":
        out.add("notifications");
        break;
      case "SAVED_MESSAGE":
        out.add("saved");
        break;
      default:
        break;
    }
  }
  return out;
}

export type SyncStep = { kind: "apply"; areas: Set<HubArea>; position: number; more: boolean } | { kind: "resync"; position: number };

/**
 * One step of the Hub change feed: entries are applied in position order; a gap is never
 * treated as acknowledged — `resyncRequired` means reload everything from `latestPosition`.
 */
export function syncStep(after: number, changes: { resyncRequired: boolean; latestPosition: number; nextPosition: number; hasMore?: boolean; data: { position: number; domain: string }[] }): SyncStep {
  if (changes.resyncRequired) return { kind: "resync", position: changes.latestPosition };
  const ordered = [...changes.data].sort((a, b) => a.position - b.position);
  const position = Math.max(after, changes.nextPosition ?? after, ...ordered.map((c) => c.position));
  return { kind: "apply", areas: areasForDomains(ordered.map((c) => c.domain)), position, more: Boolean(changes.hasMore) };
}

// ------------------------------------------------------------------ capabilities

export type CommunicationsAreas = {
  contacts: boolean;
  managedMessaging: boolean;
  /** Only E2EE readiness — never used to label MANAGED as encrypted. */
  privateE2ee: boolean;
  calling: boolean;
  realtime: "SESSION_DEVICE" | "POLLING" | "NONE";
  reason: string | null;
};

export function communicationsAreas(
  capabilities:
    | {
        activationState?: string;
        contacts?: { available: boolean };
        messaging?: { managed?: { available: boolean; realtime?: string; reason?: string }; privateE2ee?: { available: boolean } };
        personalCalling?: { available: boolean; reason?: string };
      }
    | null
    | undefined,
): CommunicationsAreas {
  if (!capabilities) return { contacts: false, managedMessaging: false, privateE2ee: false, calling: false, realtime: "NONE", reason: "UNKNOWN" };
  const active = capabilities.activationState === undefined || capabilities.activationState === "ACTIVE";
  const realtime = capabilities.messaging?.managed?.realtime;
  return {
    contacts: active && capabilities.contacts?.available === true,
    managedMessaging: active && capabilities.messaging?.managed?.available === true,
    privateE2ee: active && capabilities.messaging?.privateE2ee?.available === true,
    calling: active && capabilities.personalCalling?.available === true,
    realtime: realtime === "SESSION_DEVICE" || realtime === "POLLING" ? realtime : "NONE",
    reason: active ? null : (capabilities.activationState ?? "UNKNOWN"),
  };
}

// ------------------------------------------------------------------ workspace awareness

export type ConversationPeople = "contacts" | "colleagues";

/**
 * Who a new conversation is started with in the selected workspace: in Personal, your
 * connections (Hub contacts, CONNECTED only); in an organization, that organization's
 * colleague directory. A coworker is not automatically a contact, and a contact is not
 * offered as a colleague.
 */
export function conversationPeopleSource(kind: "PERSONAL" | "ORGANIZATION"): ConversationPeople {
  return kind === "ORGANIZATION" ? "colleagues" : "contacts";
}

export function connectedContacts<T extends { relationship: string }>(contacts: readonly T[]): T[] {
  return contacts.filter((c) => c.relationship === "CONNECTED");
}

/** A display label for a contact: your alias first, then their display name, then the ID. */
export function contactLabel(c: { alias?: string | null; displayName?: string | null; cfMeetId: string }): string {
  return c.alias?.trim() || c.displayName?.trim() || c.cfMeetId;
}

/** Human duration of a call (`durationMs`). */
export function formatDuration(ms: number | null | undefined): string {
  if (!ms || ms < 0) return "";
  const s = Math.round(ms / 1000);
  const m = Math.floor(s / 60);
  const h = Math.floor(m / 60);
  if (h) return `${h}h ${m % 60}m`;
  if (m) return `${m}m ${s % 60}s`;
  return `${s}s`;
}

/**
 * The organization colleague directory of the SELECTED organization: the Messaging directory
 * spans every organization the caller belongs to, so it is narrowed to the ACTIVE members of
 * the workspace's organization (`GET /api/v2/organizations/{id}/members`). While the member
 * list is unknown nothing is offered (default deny).
 */
export function colleaguesOfOrganization<T extends { userId: string }>(
  directory: readonly T[],
  members: readonly { userId: string; status?: string }[] | null | undefined,
): T[] {
  if (!members) return [];
  const active = new Set(members.filter((m) => !m.status || m.status === "ACTIVE").map((m) => m.userId));
  return directory.filter((p) => active.has(p.userId));
}

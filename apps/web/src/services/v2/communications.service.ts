/**
 * Communications Hub client (Blueprint Sections 14 / 17, MSG-006 / ACC-009): CF Meet ID and
 * QR, invites, contact requests, contacts, blocks, privacy, the unified timeline, chat
 * context and meeting links. Every call goes through the Meeting Service's Account relay
 * (`/api/v2/communications/*`) with the account bearer session only — never a service
 * credential, principal id or `CF-Acting-Principal`. Mutations carry an `Idempotency-Key`
 * (the contract requires one; a retry of the same action reuses it).
 *
 * Endpoints are exactly those in packages/contracts/communications/http/v1.openapi.json.
 */
import axios, { type AxiosInstance } from "axios";
import { attachAccountSession } from "@/lib/auth/axios-session";
import { messagingApiUrls } from "@/lib/messaging/urls";
import { CommunicationsApiError } from "@/lib/communications/errors";
import type {
  ActivityPage,
  Block,
  BlockMetadata,
  Bootstrap,
  CallHistoryEntry,
  Changes,
  CommunicationsCapabilities,
  CommunicationsProfile,
  Contact,
  ContactNotifications,
  ContactRequest,
  ConversationContext,
  InboxEntry,
  Invite,
  InviteMetadata,
  MeetingLink,
  Page,
  Privacy,
  PrivacyView,
  PublicId,
  PublicProfile,
  ReportReceipt,
  Timeline,
} from "@/types/communications";

type Method = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

let instance: AxiosInstance | null = null;
function http(): AxiosInstance {
  if (!instance) {
    const urls = messagingApiUrls(process.env.NEXT_PUBLIC_API_URL || undefined);
    instance = axios.create({ baseURL: `${urls.v2}/communications`, timeout: 30_000 });
    attachAccountSession(instance);
  }
  return instance;
}

const uuid = () => crypto.randomUUID();
const q = (params: Record<string, string | number | boolean | null | undefined>) => {
  const s = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== "") s.set(k, String(v));
  const out = s.toString();
  return out ? `?${out}` : "";
};
const seg = (value: string) => encodeURIComponent(value);

export async function communicationsRequest<T>(method: Method, path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
  const mutation = method !== "GET";
  try {
    const r = await http().request<T>({
      method,
      url: path,
      data: mutation ? (body ?? {}) : undefined,
      headers: mutation || idempotencyKey ? { "Idempotency-Key": idempotencyKey ?? uuid() } : undefined,
    });
    return r.data;
  } catch (error) {
    if (axios.isAxiosError<{ error?: { code?: string; message?: string } }>(error)) {
      const status = error.response?.status ?? 0;
      const detail = error.response?.data?.error;
      const retry = Number(error.response?.headers?.["retry-after"]);
      throw new CommunicationsApiError(status, detail?.code ?? (status ? `HTTP_${status}` : "NETWORK_UNAVAILABLE"), detail?.message ?? null, Number.isFinite(retry) ? retry : null);
    }
    throw new CommunicationsApiError(0, "NETWORK_UNAVAILABLE", null, null);
  }
}

const call = communicationsRequest;

export type NotificationPreferences = { version: number; directMessages: boolean; groupMessages: boolean; incomingCalls: boolean; missedCalls: boolean; contactRequests: boolean; groupInvitations: boolean; meetingInvitations: boolean };
export type ContactList = { listId: string; name: string; position: number | null; memberCount: number; version: number; updatedAt: string };
export type SavedMessage = { savedId: string; conversationId: string; messageId: string; messageSequence: number; savedAt: string; available: boolean; unavailableReason: string | null };
/** Hub 1.4.0: fileName / mediaType / kind only for MANAGED files (null for end-to-end encrypted ones); `download` / `thumbnail` are relay paths of the access-checked grant operations, never URLs. */
type GrantRef = { method: "POST"; path: string };
export type ChatMediaItem = {
  attachmentId: string; messageId: string; state: string; sizeBytes: number | null; mediaClass: string; createdAt: string;
  securityProfile?: "MANAGED" | "PRIVATE_E2EE" | null; kind?: string | null; fileName?: string | null; mediaType?: string | null;
  metadataLocation?: "SERVER" | "ENCRYPTED_DESCRIPTOR" | null; download?: GrantRef | null; thumbnail?: (GrantRef & { mediaType: "image/png" }) | null;
};
/** Hub 1.4.0: title / avatarRef for MANAGED groups both people currently belong to (null for end-to-end encrypted groups). */
export type CommonGroup = { conversationId: string; securityProfile?: "MANAGED" | "PRIVATE_E2EE" | null; title?: string | null; avatarRef?: string | null; memberCount?: number | null };
/** Hub 1.9.0: `mentionsAvailable` is false where "Only @mentions" can't work (end-to-end encrypted chats); absent from an older Hub. */
export type ChatPreferences = { conversationId: string; notifications: "ALL" | "MENTIONS" | "MUTED"; mutedUntil: string | null; effectiveNotifications: string; mediaAutoDownload: "ALWAYS" | "WIFI" | "NEVER"; version: number; mentionsAvailable?: boolean };

export const communicationsService = {
  // ---- identity, capabilities, start
  me: () => call<CommunicationsProfile>("GET", "/me"),
  /** 1.9.0: the About line contacts see (null or "" clears it). */
  updateMe: (expectedVersion: number, about: string | null) => call<CommunicationsProfile>("PATCH", "/me", { expectedVersion, about }),
  capabilities: () => call<CommunicationsCapabilities>("GET", "/me/capabilities"),
  bootstrap: () => call<Bootstrap>("GET", "/bootstrap"),
  publicId: () => call<PublicId>("GET", "/me/public-id"),
  rotatePublicId: (expectedVersion: number) => call<PublicId>("POST", "/me/public-id/rotate", { expectedVersion }),
  /** A fresh single-use QR connect invite (creates one; the contract requires an Idempotency-Key on this GET). */
  qr: (idempotencyKey: string = uuid()) => call<Invite>("GET", "/me/qr", undefined, idempotencyKey),

  // ---- privacy
  privacy: () => call<PrivacyView>("GET", "/me/privacy"),
  updatePrivacy: (expectedVersion: number, privacy: Privacy) => call<PrivacyView>("PUT", "/me/privacy", { expectedVersion, privacy }),

  // ---- discovery and invites
  resolvePublicId: (cfMeetId: string) => call<PublicProfile>("POST", "/discovery/public-id/resolve", { cfMeetId }),
  /** 1.9.0: find someone by their verified email (only people who allow it). */
  resolvePhone: (phoneNumber: string) => call<PublicProfile>("POST", "/discovery/phone/resolve", { phoneNumber }),
  resolveEmail: (email: string) => call<PublicProfile>("POST", "/discovery/email/resolve", { email }),
  resolveInvite: (token: string, idempotencyKey?: string) => call<PublicProfile>("POST", "/discovery/invites/resolve", { token, purpose: "CONNECT" }, idempotencyKey),
  createInvite: (expiresInSeconds: number, maxUses: number) => call<Invite>("POST", "/invites", { purpose: "CONNECT", expiresInSeconds, maxUses }),
  invites: (cursor?: string | null) => call<Page<InviteMetadata>>("GET", `/invites${q({ limit: 50, cursor })}`),
  revokeInvite: (inviteId: string) => call<{ inviteId: string; revoked: true }>("DELETE", `/invites/${seg(inviteId)}`),

  // ---- contact requests
  requestContact: (cfMeetId: string) => call<ContactRequest>("POST", "/contact-requests", { cfMeetId }),
  incomingRequests: (cursor?: string | null) => call<Page<ContactRequest>>("GET", `/contact-requests/incoming${q({ limit: 50, cursor })}`),
  outgoingRequests: (cursor?: string | null) => call<Page<ContactRequest>>("GET", `/contact-requests/outgoing${q({ limit: 50, cursor })}`),
  acceptRequest: (requestId: string, expectedVersion: number) => call<ContactRequest>("POST", `/contact-requests/${seg(requestId)}/accept`, { expectedVersion }),
  declineRequest: (requestId: string, expectedVersion: number) => call<ContactRequest>("POST", `/contact-requests/${seg(requestId)}/decline`, { expectedVersion }),
  cancelRequest: (requestId: string, expectedVersion: number) => call<ContactRequest>("DELETE", `/contact-requests/${seg(requestId)}`, { expectedVersion }),

  // ---- where a call came from (a chat or a contact), for its details page
  callContext: (callId: string) => call<{ origin: "CONTACT" | "CONVERSATION" | "CALLING" | string; conversationId: string | null; actions?: { openConversation?: boolean } }>("GET", `/calls/${seg(callId)}/context`),

  // ---- account-wide chat settings
  notificationPreferences: () => call<NotificationPreferences>("GET", "/me/notification-preferences"),
  setNotificationPreferences: (p: NotificationPreferences) => {
    const { version, ...rest } = p;
    return call<NotificationPreferences>("PUT", "/me/notification-preferences", { expectedVersion: version, ...rest });
  },
  storageSummary: () => call<{ messaging: { attachmentCount: number | null; attachmentBytes: number | null; backupBytes: number | null; environmentQuotaBytes: number | null }; savedMessages: { count: number; limit: number } }>("GET", "/me/storage-summary"),
  recoveryState: () => call<{ devices?: { active: number; revoked: number }; messaging?: { securityReady?: boolean } }>("GET", "/me/recovery-state"),

  // ---- contact lists ("Family", "Work"…; private to you)
  contactLists: () => call<{ data: ContactList[]; limit: number }>("GET", "/contact-lists"),
  contactList: (listId: string, cursor?: string) => call<ContactList & { members: Contact[]; nextCursor: string | null }>("GET", `/contact-lists/${seg(listId)}${q({ limit: 200, cursor })}`),
  createContactList: (name: string) => call<ContactList>("POST", "/contact-lists", { name }),
  renameContactList: (listId: string, name: string, expectedVersion: number) => call<ContactList>("PATCH", `/contact-lists/${seg(listId)}`, { name, expectedVersion }),
  deleteContactList: (listId: string, expectedVersion: number) => call<unknown>("DELETE", `/contact-lists/${seg(listId)}`, { expectedVersion }),
  setContactListMember: (listId: string, contactId: string, member: boolean) => call<unknown>(member ? "PUT" : "DELETE", `/contact-lists/${seg(listId)}/members/${seg(contactId)}`),

  // ---- saved (starred) messages
  savedMessages: (cursor?: string | null) => call<{ data: SavedMessage[]; nextCursor: string | null }>("GET", `/saved-messages${q({ limit: 100, cursor })}`),
  saveMessage: (conversationId: string, messageId: string) => call<SavedMessage>("POST", "/saved-messages", { conversationId, messageId }),
  unsaveMessage: (savedId: string) => call<unknown>("DELETE", `/saved-messages/${seg(savedId)}`),

  // ---- chat info (WhatsApp "Media, links and docs", per-chat notifications, groups in common)
  conversationMedia: (conversationId: string, cursor?: string | null) =>
    call<{ data: ChatMediaItem[]; nextCursor: string | null }>("GET", `/conversations/${seg(conversationId)}/media${q({ limit: 60, cursor })}`),
  conversationPreferences: (conversationId: string) => call<ChatPreferences>("GET", `/conversations/${seg(conversationId)}/preferences`),
  setConversationPreferences: (conversationId: string, body: { expectedVersion: number; notifications: "ALL" | "MENTIONS" | "MUTED"; mutedUntil: string | null; mediaAutoDownload: "ALWAYS" | "WIFI" | "NEVER" }) =>
    call<ChatPreferences>("PUT", `/conversations/${seg(conversationId)}/preferences`, body),
  sharedGroups: (conversationId: string) => call<{ groups: CommonGroup[]; available?: boolean }>("GET", `/conversations/${seg(conversationId)}/shared-groups`),
  commonGroups: (contactId: string) => call<{ groups: CommonGroup[] }>("GET", `/contacts/${seg(contactId)}/common-groups`),

  // ---- contacts (connections)
  contacts: (cursor?: string | null) => call<Page<Contact>>("GET", `/contacts${q({ limit: 100, cursor })}`),
  contact: (contactId: string) => call<Contact>("GET", `/contacts/${seg(contactId)}`),
  updateContact: (contactId: string, patch: { expectedVersion: number; alias?: string | null; favorite?: boolean; notifications?: ContactNotifications }) =>
    call<Pick<Contact, "contactId" | "alias" | "favorite" | "notifications" | "version">>("PATCH", `/contacts/${seg(contactId)}`, patch),
  removeContact: (contactId: string) => call<{ contactId: string; relationship: "REVOKED" }>("DELETE", `/contacts/${seg(contactId)}`),
  profile: (cfMeetId: string) => call<PublicProfile>("GET", `/profiles/${seg(cfMeetId)}`),
  reportProfile: (cfMeetId: string, reason: "SPAM" | "HARASSMENT" | "IMPERSONATION" | "OTHER") => call<ReportReceipt>("POST", `/profiles/${seg(cfMeetId)}/reports`, { reason }),

  // ---- blocks (first-party clients block only here, DATA-006)
  blocks: (cursor?: string | null) => call<Page<BlockMetadata>>("GET", `/blocks${q({ limit: 100, cursor })}`),
  block: (cfMeetId: string) => call<Block>("PUT", `/profiles/${seg(cfMeetId)}/block`),
  unblock: (cfMeetId: string) => call<Block>("DELETE", `/profiles/${seg(cfMeetId)}/block`),

  // ---- chats overlay, context, timeline, meetings
  inbox: (cursor?: string | null) => call<Page<InboxEntry>>("GET", `/inbox${q({ limit: 100, cursor })}`),
  conversationContext: (conversationId: string) => call<ConversationContext>("GET", `/conversations/${seg(conversationId)}/context`),
  timeline: (conversationId: string, cursor?: string | null) => call<Timeline>("GET", `/conversations/${seg(conversationId)}/timeline${q({ limit: 100, cursor })}`),
  activity: (cursor?: string | null) => call<ActivityPage>("GET", `/activity${q({ limit: 50, cursor })}`),
  callHistory: (missed?: boolean, cursor?: string | null) => call<Page<CallHistoryEntry>>("GET", `/calls/history${q({ limit: 50, cursor, missed })}`),
  instantMeeting: (conversationId: string, title?: string, clientRequestId: string = uuid()) =>
    call<MeetingLink>("POST", `/conversations/${seg(conversationId)}/meetings/instant`, { clientRequestId, ...(title ? { title } : {}) }, clientRequestId),
  shareMeeting: (conversationId: string, meetingCode: string, clientRequestId: string = uuid()) =>
    call<MeetingLink>("POST", `/conversations/${seg(conversationId)}/meetings/share`, { clientRequestId, meetingCode }, clientRequestId),
  conversationMeetings: (conversationId: string) => call<Page<MeetingLink>>("GET", `/conversations/${seg(conversationId)}/meetings${q({ limit: 50 })}`),
  clearForMe: (conversationId: string, throughSequence: number) =>
    call<{ clearedThroughSequence: number }>("POST", `/conversations/${seg(conversationId)}/clear-for-me`, { throughSequence }),

  // ---- multi-device sync (§4)
  sync: (after: number) => call<Changes>("GET", `/sync${q({ after, limit: 200 })}`),
};

export type CommunicationsService = typeof communicationsService;

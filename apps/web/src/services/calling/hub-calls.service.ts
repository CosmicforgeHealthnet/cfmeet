/**
 * Communications Hub call launch (CAL-014): the production-verified launch path for calls
 * started from a contact or a conversation. The Hub applies the personal communications
 * policy (blocks, privacy, account state, launch rate limit), creates the canonical Call in
 * Calling and keeps the call card on the timeline; it never proxies ringing or media — the
 * caller's own registered endpoint does (signaling + media credentials through Calling).
 *
 * Served contract: packages/contracts/communications/http/v1.openapi.json
 *   POST /api/v2/communications/contacts/{contactId}/calls        (startContactCall)
 *   POST /api/v2/communications/conversations/{conversationId}/calls (startConversationCall)
 *   GET  /api/v2/communications/contacts                          (listCommunicationsContacts)
 *   GET  /api/v2/communications/calls/history                     (listCommunicationsCallHistory)
 */

import { meetingCallingApi } from "./meeting-api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type { CallKind } from "@/types/calling";

/** CallLaunch (additionalProperties: false). */
export interface HubCallLaunch {
  kind: CallKind;
  clientRequestId: string;
  endpointId: string;
}

/** CallLaunched. */
export interface HubCallLaunched {
  schemaVersion: 1;
  callId: string;
  kind: string;
  state: string;
  version?: number;
  origin: "CONTACT" | "CONVERSATION";
  conversationId?: string | null;
  contactId?: string | null;
  replayed: boolean;
  next?: string;
}

/** Contact (subset the call launcher shows). */
export interface HubContact {
  contactId: string;
  cfMeetId: string;
  displayName: string;
  avatarReference: string | null;
  alias: string | null;
  favorite: boolean;
  relationship: unknown;
}

export interface HubContactsPage {
  schemaVersion: 1;
  data: HubContact[];
  nextCursor: string | null;
}

function launch(path: string, kind: CallKind, endpointId: string, clientRequestId = newIdempotencyKey()) {
  const body: HubCallLaunch = { kind, clientRequestId, endpointId };
  // The body's clientRequestId is the idempotency anchor; the header repeats it.
  return meetingCallingApi.post<HubCallLaunched>(path, body, { headers: { "Idempotency-Key": clientRequestId } }).then((r) => r.data);
}

export const hubCallsService = {
  callContact: (contactId: string, kind: CallKind, endpointId: string, clientRequestId?: string) =>
    launch(`/communications/contacts/${encodeURIComponent(contactId)}/calls`, kind, endpointId, clientRequestId),

  callConversation: (conversationId: string, kind: CallKind, endpointId: string, clientRequestId?: string) =>
    launch(`/communications/conversations/${encodeURIComponent(conversationId)}/calls`, kind, endpointId, clientRequestId),

  contacts: async (cursor?: string): Promise<HubContactsPage> =>
    (await meetingCallingApi.get<HubContactsPage>("/communications/contacts", { params: { limit: 100, ...(cursor ? { cursor } : {}) } })).data,
};

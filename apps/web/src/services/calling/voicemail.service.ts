/**
 * Self-service voicemail (CAL-006) through the same-origin adapter. The adapter binds every
 * request to the signed-in principal (Calling's owner view); mailbox administration of the
 * shared scope is not part of this service.
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type {
  MediaAccessResult,
  ReadStateRequest,
  VoicemailMailboxList,
  VoicemailMessage,
  VoicemailMessageFilters,
  VoicemailMessageList,
} from "@/types/calling";

export const voicemailService = {
  /** The mailboxes the signed-in user owns (GET /v1/enterprise/voicemail/mailboxes, owner-filtered by the adapter). */
  listMailboxes: async (): Promise<VoicemailMailboxList> =>
    (await callingApi.get<VoicemailMailboxList>("/v1/enterprise/voicemail/mailboxes")).data,

  listMessages: async (filters: VoicemailMessageFilters): Promise<VoicemailMessageList> =>
    (await callingApi.get<VoicemailMessageList>("/v1/enterprise/voicemail/messages", { params: filters })).data,

  getMessage: async (messageId: string): Promise<VoicemailMessage> =>
    (await callingApi.get<VoicemailMessage>(`/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}`)).data,

  deleteMessage: async (messageId: string, expectedVersion: number): Promise<void> => {
    await callingApi.delete(`/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}`, {
      params: { expectedVersion },
      headers: { "Idempotency-Key": newIdempotencyKey() },
    });
  },

  /** A short-lived signed URL (at most 900 s); never stored. */
  getMediaAccess: async (messageId: string): Promise<MediaAccessResult> =>
    (await callingApi.post<MediaAccessResult>(`/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}/media-access`, {})).data,

  setReadState: async (messageId: string, body: ReadStateRequest): Promise<VoicemailMessage> =>
    (await callingApi.put<VoicemailMessage>(`/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}/read-state`, body, {
      headers: { "Idempotency-Key": newIdempotencyKey() },
    })).data,
};

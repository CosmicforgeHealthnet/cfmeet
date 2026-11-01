/**
 * Voicemail API service.
 * Manages voicemail mailboxes and messages.
 */

import callingApi from "./api";
import type {
  VoicemailMailbox,
  VoicemailMailboxList,
  CreateMailboxRequest,
  UpdateMailboxRequest,
  VoicemailMessage,
  VoicemailMessageList,
  VoicemailMessageFilters,
  MediaAccessResult,
  ReadStateRequest,
} from "@/types/calling";

export const voicemailService = {
  // -------------------------------------------------------------------------
  // Mailbox operations
  // -------------------------------------------------------------------------

  /**
   * List all voicemail mailboxes
   */
  listMailboxes: async (): Promise<VoicemailMailboxList> => {
    const response = await callingApi.get<VoicemailMailboxList>(
      "/v1/enterprise/voicemail/mailboxes"
    );
    return response.data;
  },

  /**
   * Get a specific mailbox
   */
  getMailbox: async (mailboxId: string): Promise<VoicemailMailbox> => {
    const response = await callingApi.get<VoicemailMailbox>(
      `/v1/enterprise/voicemail/mailboxes/${encodeURIComponent(mailboxId)}`
    );
    return response.data;
  },

  /**
   * Create a new voicemail mailbox
   */
  createMailbox: async (body: CreateMailboxRequest): Promise<VoicemailMailbox> => {
    const response = await callingApi.post<VoicemailMailbox>(
      "/v1/enterprise/voicemail/mailboxes",
      body
    );
    return response.data;
  },

  /**
   * Update a mailbox's settings
   */
  updateMailbox: async (
    mailboxId: string,
    body: UpdateMailboxRequest
  ): Promise<VoicemailMailbox> => {
    const response = await callingApi.patch<VoicemailMailbox>(
      `/v1/enterprise/voicemail/mailboxes/${encodeURIComponent(mailboxId)}`,
      body
    );
    return response.data;
  },

  /**
   * Delete (disable) a mailbox
   */
  deleteMailbox: async (mailboxId: string): Promise<void> => {
    await callingApi.delete(
      `/v1/enterprise/voicemail/mailboxes/${encodeURIComponent(mailboxId)}`
    );
  },

  // -------------------------------------------------------------------------
  // Message operations
  // -------------------------------------------------------------------------

  /**
   * List messages in a mailbox
   */
  listMessages: async (
    filters: VoicemailMessageFilters
  ): Promise<VoicemailMessageList> => {
    const response = await callingApi.get<VoicemailMessageList>(
      "/v1/enterprise/voicemail/messages",
      { params: filters }
    );
    return response.data;
  },

  /**
   * Get a specific message
   */
  getMessage: async (messageId: string): Promise<VoicemailMessage> => {
    const response = await callingApi.get<VoicemailMessage>(
      `/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}`
    );
    return response.data;
  },

  /**
   * Delete a voicemail message
   */
  deleteMessage: async (messageId: string): Promise<void> => {
    await callingApi.delete(
      `/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}`
    );
  },

  /**
   * Get short-lived access URL for message audio
   */
  getMediaAccess: async (messageId: string): Promise<MediaAccessResult> => {
    const response = await callingApi.post<MediaAccessResult>(
      `/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}/media-access`,
      {}
    );
    return response.data;
  },

  /**
   * Mark a message as read or unread
   */
  setReadState: async (
    messageId: string,
    body: ReadStateRequest
  ): Promise<VoicemailMessage> => {
    const response = await callingApi.put<VoicemailMessage>(
      `/v1/enterprise/voicemail/messages/${encodeURIComponent(messageId)}/read-state`,
      body
    );
    return response.data;
  },
};

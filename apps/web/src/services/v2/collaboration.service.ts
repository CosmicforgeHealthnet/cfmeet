/**
 * V2 Collaboration Service
 * Handles media controls, chat, reactions, hand raise, and spotlights
 */

import apiV2 from "./api";
import type {
  MediaLockResponse,
  StopScreenShareResponse,
  SpotlightRequest,
  SpotlightListResponse,
  CollaborationStateResponse,
  CollaborationPolicyUpdateRequest,
  CollaborationPolicyResponse,
  MediaCredentialRequest,
  MediaCredentialResponse,
  MessageListQuery,
  MessagePage,
  MessageView,
  SendMessageRequest,
  ChatPolicyUpdateRequest,
  ChangedResponse,
  ReactionRequest,
  ReactionResponse,
  HandRaiseResponse,
  RaisedHandsResponse,
} from "@/types/v2";

export const collaborationService = {
  // -------------------------------------------------------------------------
  // Collaboration State
  // -------------------------------------------------------------------------

  /**
   * Get full collaboration state for a conference
   */
  getState: async (conferenceId: string) => {
    const response = await apiV2.get<CollaborationStateResponse>(
      `/conferences/${conferenceId}/collaboration`,
    );
    return response.data;
  },

  /**
   * Update collaboration policy
   */
  updatePolicy: async (conferenceId: string, data: CollaborationPolicyUpdateRequest) => {
    const response = await apiV2.patch<CollaborationPolicyResponse>(
      `/conferences/${conferenceId}/collaboration-policy`,
      data,
    );
    return response.data;
  },

  /**
   * Get/refresh media credential
   */
  getMediaCredential: async (conferenceId: string, data?: MediaCredentialRequest) => {
    const response = await apiV2.post<MediaCredentialResponse>(
      `/conferences/${conferenceId}/media-credential`,
      data ?? {},
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Media Locks
  // -------------------------------------------------------------------------

  /**
   * Lock audio for a participant
   */
  lockAudio: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.post<MediaLockResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/audio-lock`,
    );
    return response.data;
  },

  /**
   * Unlock audio for a participant
   */
  unlockAudio: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.delete<MediaLockResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/audio-lock`,
    );
    return response.data;
  },

  /**
   * Lock video for a participant
   */
  lockVideo: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.post<MediaLockResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/video-lock`,
    );
    return response.data;
  },

  /**
   * Unlock video for a participant
   */
  unlockVideo: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.delete<MediaLockResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/video-lock`,
    );
    return response.data;
  },

  /**
   * Lock screen share for a participant
   */
  lockScreenShare: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.post<MediaLockResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/screen-share-lock`,
    );
    return response.data;
  },

  /**
   * Unlock screen share for a participant
   */
  unlockScreenShare: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.delete<MediaLockResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/screen-share-lock`,
    );
    return response.data;
  },

  /**
   * Stop screen share for a participant
   */
  stopScreenShare: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.post<StopScreenShareResponse>(
      `/conferences/${conferenceId}/screen-share/${participantId}/stop`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Spotlights
  // -------------------------------------------------------------------------

  /**
   * Add a participant to spotlight
   */
  addSpotlight: async (conferenceId: string, data: SpotlightRequest) => {
    const response = await apiV2.post<SpotlightListResponse>(
      `/conferences/${conferenceId}/spotlights`,
      data,
    );
    return response.data;
  },

  /**
   * Remove a participant from spotlight
   */
  removeSpotlight: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.delete<SpotlightListResponse>(
      `/conferences/${conferenceId}/spotlights/${participantId}`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Chat
  // -------------------------------------------------------------------------

  /**
   * Get chat messages
   */
  getMessages: async (conferenceId: string, query?: MessageListQuery) => {
    const response = await apiV2.get<MessagePage>(
      `/conferences/${conferenceId}/messages`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Send a chat message
   */
  sendMessage: async (conferenceId: string, data: SendMessageRequest) => {
    const response = await apiV2.post<MessageView>(
      `/conferences/${conferenceId}/messages`,
      data,
    );
    return response.data;
  },

  /**
   * Delete a chat message
   */
  deleteMessage: async (conferenceId: string, messageId: string) => {
    const response = await apiV2.delete<MessageView>(
      `/conferences/${conferenceId}/messages/${messageId}`,
    );
    return response.data;
  },

  /**
   * Update chat policy
   */
  updateChatPolicy: async (conferenceId: string, data: ChatPolicyUpdateRequest) => {
    const response = await apiV2.patch<CollaborationPolicyResponse>(
      `/conferences/${conferenceId}/chat-policy`,
      data,
    );
    return response.data;
  },

  /**
   * Restrict chat for a participant
   */
  restrictChat: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.post<ChangedResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/chat-restriction`,
    );
    return response.data;
  },

  /**
   * Unrestrict chat for a participant
   */
  unrestrictChat: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.delete<ChangedResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/chat-restriction`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Reactions
  // -------------------------------------------------------------------------

  /**
   * Send a reaction
   */
  sendReaction: async (conferenceId: string, data: ReactionRequest) => {
    const response = await apiV2.post<ReactionResponse>(
      `/conferences/${conferenceId}/reactions`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Hand Raise
  // -------------------------------------------------------------------------

  /**
   * Get raised hands
   */
  getRaisedHands: async (conferenceId: string) => {
    const response = await apiV2.get<RaisedHandsResponse>(
      `/conferences/${conferenceId}/hand-raise`,
    );
    return response.data;
  },

  /**
   * Raise hand (self)
   */
  raiseHand: async (conferenceId: string) => {
    const response = await apiV2.post<HandRaiseResponse>(
      `/conferences/${conferenceId}/hand-raise`,
    );
    return response.data;
  },

  /**
   * Lower hand (self)
   */
  lowerHand: async (conferenceId: string) => {
    const response = await apiV2.delete<HandRaiseResponse>(
      `/conferences/${conferenceId}/hand-raise`,
    );
    return response.data;
  },

  /**
   * Lower someone else's hand (moderation)
   */
  lowerOtherHand: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.delete<HandRaiseResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/hand-raise`,
    );
    return response.data;
  },
};

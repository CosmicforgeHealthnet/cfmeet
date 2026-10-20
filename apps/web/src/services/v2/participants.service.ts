/**
 * V2 Participants Service
 * Handles participant management, waiting room, and moderation
 */

import apiV2 from "./api";
import type {
  ParticipantResponse,
  WaitingRoomResponse,
  AdmitRequest,
  DenyRequest,
  BulkAdmissionResponse,
  RoleChangeRequest,
  RemoveParticipantRequest,
  BanParticipantRequest,
  ModerationResponse,
  MuteRequest,
  MuteResponse,
} from "@/types/v2";

export const participantsService = {
  // -------------------------------------------------------------------------
  // Roster
  // -------------------------------------------------------------------------

  /**
   * Get participant roster for a conference
   */
  list: async (conferenceId: string) => {
    const response = await apiV2.get<ParticipantResponse[]>(
      `/conferences/${conferenceId}/participants`,
    );
    return response.data;
  },

  /**
   * Get current participant (self)
   */
  me: async (conferenceId: string) => {
    const response = await apiV2.get<ParticipantResponse>(
      `/conferences/${conferenceId}/participants/me`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Waiting Room
  // -------------------------------------------------------------------------

  /**
   * Get waiting room participants
   */
  waitingRoom: async (conferenceId: string) => {
    const response = await apiV2.get<WaitingRoomResponse>(
      `/conferences/${conferenceId}/waiting-room`,
    );
    return response.data;
  },

  /**
   * Admit participants from waiting room
   */
  admit: async (conferenceId: string, data: AdmitRequest) => {
    const response = await apiV2.post<BulkAdmissionResponse>(
      `/conferences/${conferenceId}/waiting-room/admit`,
      data,
    );
    return response.data;
  },

  /**
   * Deny participants from waiting room
   */
  deny: async (conferenceId: string, data: DenyRequest) => {
    const response = await apiV2.post<BulkAdmissionResponse>(
      `/conferences/${conferenceId}/waiting-room/deny`,
      data,
    );
    return response.data;
  },

  /**
   * Return a participant to waiting room
   */
  returnToWaiting: async (conferenceId: string, participantId: string) => {
    const response = await apiV2.post<ParticipantResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/return-to-waiting`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Role Management
  // -------------------------------------------------------------------------

  /**
   * Change participant role
   */
  changeRole: async (
    conferenceId: string,
    participantId: string,
    data: RoleChangeRequest,
  ) => {
    const response = await apiV2.patch<ParticipantResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/role`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Moderation
  // -------------------------------------------------------------------------

  /**
   * Remove participant from conference
   */
  remove: async (
    conferenceId: string,
    participantId: string,
    data?: RemoveParticipantRequest,
  ) => {
    const response = await apiV2.post<ModerationResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/remove`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Ban participant
   */
  ban: async (
    conferenceId: string,
    participantId: string,
    data?: BanParticipantRequest,
  ) => {
    const response = await apiV2.post<ModerationResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/ban`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Server-side mute a participant
   */
  mute: async (
    conferenceId: string,
    participantId: string,
    data: MuteRequest,
  ) => {
    const response = await apiV2.post<MuteResponse>(
      `/conferences/${conferenceId}/participants/${participantId}/mute`,
      data,
    );
    return response.data;
  },
};

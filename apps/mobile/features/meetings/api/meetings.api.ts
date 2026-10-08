import { api, publicApi } from "@/lib/api/client";
import type {
  Meeting,
  MeetingsResponse,
  CreateMeetingRequest,
  CreateMeetingResponse,
  UpdateMeetingRequest,
  InstantMeetingResponse,
  JoinMeetingRequest,
  JoinMeetingResponse,
  PublicMeetingInfo,
  ParticipantsResponse,
  WaitingParticipantsResponse,
  ChatMessagesResponse,
  SendChatMessageRequest,
  ChatMessage,
  MediaStateRequest,
} from "../types";

export const meetingsApi = {
  async getMeetings(page = 1, limit = 10): Promise<MeetingsResponse> {
    const response = await api.get<MeetingsResponse>("/meetings", {
      params: { page, limit },
    });
    return response.data;
  },

  async getMeeting(id: string): Promise<Meeting> {
    const response = await api.get<{ meeting: Meeting }>(`/meetings/${id}`);
    return response.data.meeting;
  },

  async createMeeting(data: CreateMeetingRequest): Promise<CreateMeetingResponse> {
    const response = await api.post<CreateMeetingResponse>("/meetings", data);
    return response.data;
  },

  async updateMeeting(id: string, data: UpdateMeetingRequest): Promise<Meeting> {
    const response = await api.put<{ meeting: Meeting }>(`/meetings/${id}`, data);
    return response.data.meeting;
  },

  async deleteMeeting(id: string): Promise<void> {
    await api.delete(`/meetings/${id}`);
  },

  async createInstantMeeting(): Promise<InstantMeetingResponse> {
    const response = await api.post<InstantMeetingResponse>("/meetings/instant");
    return response.data;
  },

  async endMeeting(id: string): Promise<void> {
    await api.post(`/meetings/${id}/end`);
  },

  async getPublicMeeting(identifier: string): Promise<PublicMeetingInfo> {
    const response = await publicApi.get<{ meeting: PublicMeetingInfo }>(
      `/meetings/public/${identifier}`
    );
    return response.data.meeting;
  },

  async joinMeeting(identifier: string, data: JoinMeetingRequest): Promise<JoinMeetingResponse> {
    const response = await api.post<JoinMeetingResponse>(
      `/meetings/join/${identifier}`,
      data
    );
    return response.data;
  },

  async joinMeetingAsGuest(identifier: string, data: JoinMeetingRequest): Promise<JoinMeetingResponse> {
    const response = await publicApi.post<JoinMeetingResponse>(
      `/meetings/join/${identifier}`,
      data
    );
    return response.data;
  },

  async getParticipants(meetingId: string): Promise<ParticipantsResponse> {
    const response = await api.get<ParticipantsResponse>(
      `/meetings/${meetingId}/participants`
    );
    return response.data;
  },

  async getWaitingParticipants(meetingId: string): Promise<WaitingParticipantsResponse> {
    const response = await api.get<WaitingParticipantsResponse>(
      `/meetings/${meetingId}/waiting`
    );
    return response.data;
  },

  async admitParticipant(meetingId: string, participantId: string): Promise<void> {
    await api.post(`/meetings/${meetingId}/waiting/${participantId}/admit`);
  },

  async denyParticipant(meetingId: string, participantId: string): Promise<void> {
    await api.post(`/meetings/${meetingId}/waiting/${participantId}/deny`);
  },

  async getChatMessages(meetingId: string): Promise<ChatMessagesResponse> {
    const response = await api.get<ChatMessagesResponse>(
      `/meetings/${meetingId}/chat`
    );
    return response.data;
  },

  async sendChatMessage(meetingId: string, data: SendChatMessageRequest): Promise<ChatMessage> {
    const response = await api.post<{ message: ChatMessage }>(
      `/meetings/${meetingId}/chat`,
      data
    );
    return response.data.message;
  },

  async updateAudioState(participantId: string, data: MediaStateRequest): Promise<void> {
    await api.patch(`/participants/${participantId}/audio`, data);
  },

  async updateVideoState(participantId: string, data: MediaStateRequest): Promise<void> {
    await api.patch(`/participants/${participantId}/video`, data);
  },

  async startScreenShare(meetingId: string): Promise<void> {
    await api.post(`/meetings/${meetingId}/screen-share/start`);
  },

  async stopScreenShare(meetingId: string): Promise<void> {
    await api.post(`/meetings/${meetingId}/screen-share/stop`);
  },
};

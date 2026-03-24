import api, { publicApi } from "./api";
import type {
  MeetingsParams,
  MeetingsResponse,
  MeetingResponse,
  CreateMeetingRequest,
  CreateMeetingResponse,
  UpdateMeetingRequest,
  JoinMeetingResponse,
  JoinMeetingRequest,
  WaitingListResponse,
  ParticipantActionResponse,
  ParticipantsListResponse,
  ChatMessage,
  InstantMeetingRequest,
  InstantMeetingResponse,
} from "@/types/meeting";

export const meetingService = {
  // GET /meetings — list meetings
  getMeetings: async (params?: MeetingsParams) => {
    const response = await api.get<MeetingsResponse>("/meetings", { params });
    return response.data;
  },

  // GET /meetings/:id — get meeting by ID
  getMeeting: async (id: string) => {
    const response = await api.get<MeetingResponse>(`/meetings/${id}`);
    return response.data;
  },

  // GET /meetings/public/:meeting_identifier — get public meeting info
  getPublicMeeting: async (meetingIdentifier: string) => {
    const response = await publicApi.get<MeetingResponse>(
      `/meetings/public/${meetingIdentifier}`,
    );
    return response.data;
  },

  // POST /meetings — create a meeting
  createMeeting: async (data: CreateMeetingRequest) => {
    const response = await api.post<CreateMeetingResponse>("/meetings", data);
    return response.data;
  },

  // POST /meetings/instant — create an instant public meeting (1 hour)
  createInstantMeeting: async (data?: InstantMeetingRequest) => {
    const response = await api.post<InstantMeetingResponse>(
      "/meetings/instant",
      data ?? {},
    );
    return response.data;
  },

  // POST /meetings/join/:meeting_identifier — join a meeting
  joinMeeting: async (meetingIdentifier: string, data: JoinMeetingRequest) => {
    const response = await publicApi.post<JoinMeetingResponse>(
      `/meetings/join/${meetingIdentifier}`,
      data,
    );
    return response.data;
  },

  // PUT /meetings/:id — update a meeting (host only)
  updateMeeting: async (id: string, data: UpdateMeetingRequest) => {
    const response = await api.put<MeetingResponse>(`/meetings/${id}`, data);
    return response.data;
  },

  // DELETE /meetings/:id — delete a meeting (host only)
  deleteMeeting: async (id: string) => {
    const response = await api.delete<{ success: boolean }>(`/meetings/${id}`);
    return response.data;
  },

  // POST /meetings/:id/end — end a meeting (host only)
  endMeeting: async (id: string) => {
    const response = await api.post<MeetingResponse>(`/meetings/${id}/end`);
    return response.data;
  },

  // GET /meetings/:id/participants — list participants
  getParticipants: async (meetingId: string) => {
    const response = await api.get<ParticipantsListResponse>(
      `/meetings/${meetingId}/participants`,
    );
    return response.data;
  },

  // GET /meetings/:id/waiting — list waiting room participants (host only)
  getWaitingParticipants: async (meetingId: string) => {
    const response = await api.get<WaitingListResponse>(
      `/meetings/${meetingId}/waiting`,
    );
    return response.data;
  },

  // POST /meetings/:id/waiting/:participant_id/admit — admit participant (host only)
  admitParticipant: async (meetingId: string, participantId: string) => {
    const response = await api.post<ParticipantActionResponse>(
      `/meetings/${meetingId}/waiting/${participantId}/admit`,
    );
    return response.data;
  },

  // POST /meetings/:id/waiting/:participant_id/deny — deny participant (host only)
  denyParticipant: async (meetingId: string, participantId: string) => {
    const response = await api.post<ParticipantActionResponse>(
      `/meetings/${meetingId}/waiting/${participantId}/deny`,
    );
    return response.data;
  },

  // MEDIA CONTROL

  // POST /meetings/:id/screen-share/start
  startScreenShare: async (meetingId: string) => {
    const response = await api.post<{ success: boolean; message: string }>(
      `/meetings/${meetingId}/screen-share/start`,
    );
    return response.data;
  },

  // POST /meetings/:id/screen-share/stop
  stopScreenShare: async (meetingId: string) => {
    const response = await api.post<{ success: boolean; message: string }>(
      `/meetings/${meetingId}/screen-share/stop`,
    );
    return response.data;
  },

  // PATCH /participants/:id/audio
  updateAudioState: async (participantId: string, isMuted: boolean) => {
    const response = await api.patch<{ success: boolean; message: string }>(
      `/participants/${participantId}/audio`,
      { is_muted: isMuted },
    );
    return response.data;
  },

  // PATCH /participants/:id/video
  updateVideoState: async (participantId: string, isVideoOn: boolean) => {
    const response = await api.patch<{ success: boolean; message: string }>(
      `/participants/${participantId}/video`,
      { is_video_on: isVideoOn },
    );
    return response.data;
  },

  // CHAT

  // GET /meetings/:id/chat — get chat messages
  getChatMessages: async (meetingId: string) => {
    const response = await api.get<{ success: boolean; data: ChatMessage[] }>(
      `/meetings/${meetingId}/chat`,
    );
    return response.data;
  },

  // POST /meetings/:id/chat — send a chat message
  sendChatMessage: async (
    meetingId: string,
    participantId: string,
    message: string,
  ) => {
    const response = await api.post<{
      success: boolean;
      message: string;
      data: ChatMessage;
    }>(`/meetings/${meetingId}/chat`, {
      participant_id: participantId,
      message,
    });
    return response.data;
  },
};

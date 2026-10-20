/**
 * CF Meet V2 API Services (Mobile)
 * Full V2 API consumption for meeting spaces, conferences, and engagement features
 */

import { apiV2, publicApiV2 } from "@/lib/api/client";
import type {
  // Meeting Spaces
  MeetingSpaceResponse,
  MeetingSpaceListResponse,
  CreateMeetingSpaceRequest,
  InstantMeetingRequest,
  InstantMeetingResponse,
  // Conferences
  ConferenceResponse,
  ConferenceListResponse,
  CreateConferenceRequest,
  ConferenceActionResponse,
  // Join
  JoinConferenceRequest,
  JoinConferenceResponse,
  LeaveConferenceRequest,
  LeaveConferenceResponse,
  ResolveResponse,
  PreJoinResponse,
  // Participants
  ParticipantResponse,
  ParticipantListResponse,
  WaitingRoomResponse,
  AdmitRequest,
  DenyRequest,
  BulkAdmissionResponse,
  RoleChangeRequest,
  ModerationResponse,
  RemoveParticipantRequest,
  MuteRequest,
  MuteResponse,
  // Chat
  MessagePage,
  MessageListQuery,
  SendMessageRequest,
  MessageView,
  // Reactions & Hand Raise
  ReactionRequest,
  ReactionResponse,
  HandRaiseResponse,
  RaisedHandsResponse,
  // Breakouts
  BreakoutListResponse,
  CreateBreakoutsRequest,
  BreakoutListCreatedResponse,
  AssignBreakoutsRequest,
  AssignmentResult,
  BreakoutResponse,
  BreakoutAnnouncementRequest,
  AnnouncementResponse,
  CloseBreakoutRequest,
  CloseBreakoutsResponse,
  MediaContextResponse,
  // Polls
  PollListResponse,
  CreatePollRequest,
  PollView,
  PollTransitionResponse,
  SubmitPollResponseRequest,
  PollResponseReceipt,
  PollResults,
  // Q&A
  QuestionListResponse,
  SubmitQuestionRequest,
  QuestionView,
  QuestionActionResponse,
  AnswerQuestionRequest,
  // Collaboration
  CollaborationStateResponse,
  SpotlightRequest,
  SpotlightListResponse,
  MediaLockResponse,
} from "./types";

const V2_BASE = "";

// ---------------------------------------------------------------------------
// Meeting Spaces API
// ---------------------------------------------------------------------------

export const meetingSpacesApi = {
  async list(): Promise<MeetingSpaceListResponse> {
    const response = await apiV2.get<MeetingSpaceListResponse>(`${V2_BASE}/spaces`);
    return response.data;
  },

  async get(id: string): Promise<MeetingSpaceResponse> {
    const response = await apiV2.get<MeetingSpaceResponse>(`${V2_BASE}/spaces/${id}`);
    return response.data;
  },

  async create(data: CreateMeetingSpaceRequest): Promise<MeetingSpaceResponse> {
    const response = await apiV2.post<MeetingSpaceResponse>(`${V2_BASE}/spaces`, data);
    return response.data;
  },

  async archive(id: string): Promise<MeetingSpaceResponse> {
    const response = await apiV2.post<MeetingSpaceResponse>(`${V2_BASE}/spaces/${id}/archive`);
    return response.data;
  },

  async resolve(codeOrId: string): Promise<ResolveResponse> {
    const response = await publicApiV2.get<ResolveResponse>(`${V2_BASE}/resolve/${codeOrId}`);
    return response.data;
  },

  async instant(data: InstantMeetingRequest = {}): Promise<InstantMeetingResponse> {
    const response = await apiV2.post<InstantMeetingResponse>(`${V2_BASE}/instant`, data);
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Conferences API
// ---------------------------------------------------------------------------

export const conferencesApi = {
  async list(spaceId: string): Promise<ConferenceListResponse> {
    const response = await apiV2.get<ConferenceListResponse>(
      `${V2_BASE}/spaces/${spaceId}/conferences`
    );
    return response.data;
  },

  async get(spaceId: string, conferenceId: string): Promise<ConferenceResponse> {
    const response = await apiV2.get<ConferenceResponse>(
      `${V2_BASE}/spaces/${spaceId}/conferences/${conferenceId}`
    );
    return response.data;
  },

  async create(spaceId: string, data: CreateConferenceRequest = {}): Promise<ConferenceResponse> {
    const response = await apiV2.post<ConferenceResponse>(
      `${V2_BASE}/spaces/${spaceId}/conferences`,
      data
    );
    return response.data;
  },

  async start(conferenceId: string): Promise<ConferenceActionResponse> {
    const response = await apiV2.post<ConferenceActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/start`
    );
    return response.data;
  },

  async end(conferenceId: string): Promise<ConferenceActionResponse> {
    const response = await apiV2.post<ConferenceActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/end`
    );
    return response.data;
  },

  async lock(conferenceId: string): Promise<ConferenceActionResponse> {
    const response = await apiV2.post<ConferenceActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/lock`
    );
    return response.data;
  },

  async unlock(conferenceId: string): Promise<ConferenceActionResponse> {
    const response = await apiV2.post<ConferenceActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/unlock`
    );
    return response.data;
  },

  async preJoin(conferenceId: string): Promise<PreJoinResponse> {
    const response = await publicApiV2.get<PreJoinResponse>(
      `${V2_BASE}/conferences/${conferenceId}/pre-join`
    );
    return response.data;
  },

  async join(conferenceId: string, data: JoinConferenceRequest = {}): Promise<JoinConferenceResponse> {
    const response = await apiV2.post<JoinConferenceResponse>(
      `${V2_BASE}/conferences/${conferenceId}/join`,
      data
    );
    return response.data;
  },

  async joinAsGuest(conferenceId: string, data: JoinConferenceRequest): Promise<JoinConferenceResponse> {
    const response = await publicApiV2.post<JoinConferenceResponse>(
      `${V2_BASE}/conferences/${conferenceId}/join`,
      data
    );
    return response.data;
  },

  async leave(conferenceId: string, data: LeaveConferenceRequest = {}): Promise<LeaveConferenceResponse> {
    const response = await apiV2.post<LeaveConferenceResponse>(
      `${V2_BASE}/conferences/${conferenceId}/leave`,
      data
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Participants API
// ---------------------------------------------------------------------------

export const participantsApi = {
  async list(conferenceId: string): Promise<ParticipantListResponse> {
    const response = await apiV2.get<ParticipantListResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants`
    );
    return response.data;
  },

  async get(conferenceId: string, participantId: string): Promise<ParticipantResponse> {
    const response = await apiV2.get<ParticipantResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}`
    );
    return response.data;
  },

  async changeRole(
    conferenceId: string,
    participantId: string,
    data: RoleChangeRequest
  ): Promise<ParticipantResponse> {
    const response = await apiV2.patch<ParticipantResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/role`,
      data
    );
    return response.data;
  },

  async remove(
    conferenceId: string,
    participantId: string,
    data?: RemoveParticipantRequest
  ): Promise<ModerationResponse> {
    const response = await apiV2.post<ModerationResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/remove`,
      data || {}
    );
    return response.data;
  },

  async mute(
    conferenceId: string,
    participantId: string,
    data: MuteRequest
  ): Promise<MuteResponse> {
    const response = await apiV2.post<MuteResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/mute`,
      data
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Waiting Room API
// ---------------------------------------------------------------------------

export const waitingRoomApi = {
  async list(conferenceId: string): Promise<WaitingRoomResponse> {
    const response = await apiV2.get<WaitingRoomResponse>(
      `${V2_BASE}/conferences/${conferenceId}/waiting`
    );
    return response.data;
  },

  async admit(conferenceId: string, data: AdmitRequest): Promise<BulkAdmissionResponse> {
    const response = await apiV2.post<BulkAdmissionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/waiting/admit`,
      data
    );
    return response.data;
  },

  async deny(conferenceId: string, data: DenyRequest): Promise<BulkAdmissionResponse> {
    const response = await apiV2.post<BulkAdmissionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/waiting/deny`,
      data
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Chat API
// ---------------------------------------------------------------------------

export const chatApi = {
  async getMessages(conferenceId: string, query?: MessageListQuery): Promise<MessagePage> {
    const response = await apiV2.get<MessagePage>(
      `${V2_BASE}/conferences/${conferenceId}/chat`,
      { params: query }
    );
    return response.data;
  },

  async sendMessage(conferenceId: string, data: SendMessageRequest): Promise<MessageView> {
    const response = await apiV2.post<MessageView>(
      `${V2_BASE}/conferences/${conferenceId}/chat`,
      data
    );
    return response.data;
  },

  async deleteMessage(conferenceId: string, messageId: string): Promise<void> {
    await apiV2.delete(`${V2_BASE}/conferences/${conferenceId}/chat/${messageId}`);
  },
};

// ---------------------------------------------------------------------------
// Reactions & Hand Raise API
// ---------------------------------------------------------------------------

export const reactionsApi = {
  async sendReaction(conferenceId: string, data: ReactionRequest): Promise<ReactionResponse> {
    const response = await apiV2.post<ReactionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/reactions`,
      data
    );
    return response.data;
  },

  async raiseHand(conferenceId: string): Promise<HandRaiseResponse> {
    const response = await apiV2.post<HandRaiseResponse>(
      `${V2_BASE}/conferences/${conferenceId}/hand/raise`
    );
    return response.data;
  },

  async lowerHand(conferenceId: string): Promise<HandRaiseResponse> {
    const response = await apiV2.post<HandRaiseResponse>(
      `${V2_BASE}/conferences/${conferenceId}/hand/lower`
    );
    return response.data;
  },

  async lowerOthersHand(conferenceId: string, participantId: string): Promise<HandRaiseResponse> {
    const response = await apiV2.post<HandRaiseResponse>(
      `${V2_BASE}/conferences/${conferenceId}/hand/${participantId}/lower`
    );
    return response.data;
  },

  async getRaisedHands(conferenceId: string): Promise<RaisedHandsResponse> {
    const response = await apiV2.get<RaisedHandsResponse>(
      `${V2_BASE}/conferences/${conferenceId}/hand`
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Breakouts API
// ---------------------------------------------------------------------------

export const breakoutsApi = {
  async list(conferenceId: string): Promise<BreakoutListResponse> {
    const response = await apiV2.get<BreakoutListResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts`
    );
    return response.data;
  },

  async create(conferenceId: string, data: CreateBreakoutsRequest): Promise<BreakoutListCreatedResponse> {
    const response = await apiV2.post<BreakoutListCreatedResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts`,
      data
    );
    return response.data;
  },

  async assign(conferenceId: string, data: AssignBreakoutsRequest): Promise<AssignmentResult> {
    const response = await apiV2.post<AssignmentResult>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/assign`,
      data
    );
    return response.data;
  },

  async open(conferenceId: string, breakoutId: string): Promise<BreakoutResponse> {
    const response = await apiV2.post<BreakoutResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/${breakoutId}/open`
    );
    return response.data;
  },

  async close(
    conferenceId: string,
    breakoutId: string,
    data?: CloseBreakoutRequest
  ): Promise<BreakoutResponse> {
    const response = await apiV2.post<BreakoutResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/${breakoutId}/close`,
      data || {}
    );
    return response.data;
  },

  async closeAll(conferenceId: string, data?: CloseBreakoutRequest): Promise<CloseBreakoutsResponse> {
    const response = await apiV2.post<CloseBreakoutsResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/close-all`,
      data || {}
    );
    return response.data;
  },

  async announce(conferenceId: string, data: BreakoutAnnouncementRequest): Promise<AnnouncementResponse> {
    const response = await apiV2.post<AnnouncementResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/announce`,
      data
    );
    return response.data;
  },

  async enter(conferenceId: string, breakoutId: string): Promise<MediaContextResponse> {
    const response = await apiV2.post<MediaContextResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/${breakoutId}/enter`
    );
    return response.data;
  },

  async leave(conferenceId: string): Promise<MediaContextResponse> {
    const response = await apiV2.post<MediaContextResponse>(
      `${V2_BASE}/conferences/${conferenceId}/breakouts/leave`
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Polls API
// ---------------------------------------------------------------------------

export const pollsApi = {
  async list(conferenceId: string): Promise<PollListResponse> {
    const response = await apiV2.get<PollListResponse>(
      `${V2_BASE}/conferences/${conferenceId}/polls`
    );
    return response.data;
  },

  async get(conferenceId: string, pollId: string): Promise<PollView> {
    const response = await apiV2.get<PollView>(
      `${V2_BASE}/conferences/${conferenceId}/polls/${pollId}`
    );
    return response.data;
  },

  async create(conferenceId: string, data: CreatePollRequest): Promise<PollView> {
    const response = await apiV2.post<PollView>(
      `${V2_BASE}/conferences/${conferenceId}/polls`,
      data
    );
    return response.data;
  },

  async open(conferenceId: string, pollId: string): Promise<PollTransitionResponse> {
    const response = await apiV2.post<PollTransitionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/polls/${pollId}/open`
    );
    return response.data;
  },

  async close(conferenceId: string, pollId: string): Promise<PollTransitionResponse> {
    const response = await apiV2.post<PollTransitionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/polls/${pollId}/close`
    );
    return response.data;
  },

  async submitResponse(
    conferenceId: string,
    pollId: string,
    data: SubmitPollResponseRequest
  ): Promise<PollResponseReceipt> {
    const response = await apiV2.post<PollResponseReceipt>(
      `${V2_BASE}/conferences/${conferenceId}/polls/${pollId}/respond`,
      data
    );
    return response.data;
  },

  async getResults(conferenceId: string, pollId: string): Promise<PollResults> {
    const response = await apiV2.get<PollResults>(
      `${V2_BASE}/conferences/${conferenceId}/polls/${pollId}/results`
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Q&A API
// ---------------------------------------------------------------------------

export const qnaApi = {
  async list(conferenceId: string): Promise<QuestionListResponse> {
    const response = await apiV2.get<QuestionListResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna`
    );
    return response.data;
  },

  async submit(conferenceId: string, data: SubmitQuestionRequest): Promise<QuestionView> {
    const response = await apiV2.post<QuestionView>(
      `${V2_BASE}/conferences/${conferenceId}/qna`,
      data
    );
    return response.data;
  },

  async vote(conferenceId: string, questionId: string): Promise<QuestionActionResponse> {
    const response = await apiV2.post<QuestionActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna/${questionId}/vote`
    );
    return response.data;
  },

  async unvote(conferenceId: string, questionId: string): Promise<QuestionActionResponse> {
    const response = await apiV2.delete<QuestionActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna/${questionId}/vote`
    );
    return response.data;
  },

  async answer(
    conferenceId: string,
    questionId: string,
    data: AnswerQuestionRequest
  ): Promise<QuestionActionResponse> {
    const response = await apiV2.post<QuestionActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna/${questionId}/answer`,
      data
    );
    return response.data;
  },

  async dismiss(conferenceId: string, questionId: string): Promise<QuestionActionResponse> {
    const response = await apiV2.post<QuestionActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna/${questionId}/dismiss`
    );
    return response.data;
  },

  async pin(conferenceId: string, questionId: string): Promise<QuestionActionResponse> {
    const response = await apiV2.post<QuestionActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna/${questionId}/pin`
    );
    return response.data;
  },

  async unpin(conferenceId: string, questionId: string): Promise<QuestionActionResponse> {
    const response = await apiV2.delete<QuestionActionResponse>(
      `${V2_BASE}/conferences/${conferenceId}/qna/${questionId}/pin`
    );
    return response.data;
  },
};

// ---------------------------------------------------------------------------
// Collaboration API
// ---------------------------------------------------------------------------

export const collaborationApi = {
  async getState(conferenceId: string): Promise<CollaborationStateResponse> {
    const response = await apiV2.get<CollaborationStateResponse>(
      `${V2_BASE}/conferences/${conferenceId}/collaboration`
    );
    return response.data;
  },

  async spotlight(conferenceId: string, data: SpotlightRequest): Promise<SpotlightListResponse> {
    const response = await apiV2.post<SpotlightListResponse>(
      `${V2_BASE}/conferences/${conferenceId}/spotlight`,
      data
    );
    return response.data;
  },

  async removeSpotlight(conferenceId: string, participantId: string): Promise<SpotlightListResponse> {
    const response = await apiV2.delete<SpotlightListResponse>(
      `${V2_BASE}/conferences/${conferenceId}/spotlight/${participantId}`
    );
    return response.data;
  },

  async lockAudio(conferenceId: string, participantId: string): Promise<MediaLockResponse> {
    const response = await apiV2.post<MediaLockResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/lock/audio`
    );
    return response.data;
  },

  async unlockAudio(conferenceId: string, participantId: string): Promise<MediaLockResponse> {
    const response = await apiV2.delete<MediaLockResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/lock/audio`
    );
    return response.data;
  },

  async lockVideo(conferenceId: string, participantId: string): Promise<MediaLockResponse> {
    const response = await apiV2.post<MediaLockResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/lock/video`
    );
    return response.data;
  },

  async unlockVideo(conferenceId: string, participantId: string): Promise<MediaLockResponse> {
    const response = await apiV2.delete<MediaLockResponse>(
      `${V2_BASE}/conferences/${conferenceId}/participants/${participantId}/lock/video`
    );
    return response.data;
  },
};

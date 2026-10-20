/**
 * CF Meet V2 React Query Hooks (Mobile)
 * Complete hook implementations for V2 API
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { QUERY_KEYS } from "@/lib/constants";
import { getApiError } from "@/lib/api/client";
import {
  meetingSpacesApi,
  conferencesApi,
  participantsApi,
  waitingRoomApi,
  chatApi,
  reactionsApi,
  breakoutsApi,
  pollsApi,
  qnaApi,
  collaborationApi,
} from "./api";
import type {
  CreateMeetingSpaceRequest,
  InstantMeetingRequest,
  JoinConferenceRequest,
  CreateConferenceRequest,
  AdmitRequest,
  DenyRequest,
  RoleChangeRequest,
  SendMessageRequest,
  CreateBreakoutsRequest,
  AssignBreakoutsRequest,
  CloseBreakoutRequest,
  BreakoutAnnouncementRequest,
  CreatePollRequest,
  SubmitPollResponseRequest,
  SubmitQuestionRequest,
  AnswerQuestionRequest,
  ReactionRequest,
  SpotlightRequest,
  TrackKind,
} from "./types";

// ---------------------------------------------------------------------------
// Meeting Spaces Hooks
// ---------------------------------------------------------------------------

export function useSpaces() {
  return useQuery({
    queryKey: QUERY_KEYS.V2.SPACES,
    queryFn: () => meetingSpacesApi.list(),
    refetchInterval: 10000,
  });
}

export function useSpace(id: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.SPACE(id),
    queryFn: () => meetingSpacesApi.get(id),
    enabled: !!id,
  });
}

export function useCreateSpace() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateMeetingSpaceRequest) => {
      try {
        return await meetingSpacesApi.create(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.SPACES });
    },
  });
}

export function useArchiveSpace() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (id: string) => {
      try {
        return await meetingSpacesApi.archive(id);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.SPACES });
    },
  });
}

export function useResolve(codeOrId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.RESOLVE(codeOrId),
    queryFn: () => meetingSpacesApi.resolve(codeOrId),
    enabled: !!codeOrId,
    retry: false,
  });
}

export function useCreateInstantMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: InstantMeetingRequest = {}) => {
      try {
        return await meetingSpacesApi.instant(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.SPACES });
    },
  });
}

// ---------------------------------------------------------------------------
// Conferences Hooks
// ---------------------------------------------------------------------------

export function useConferences(spaceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.CONFERENCES(spaceId),
    queryFn: () => conferencesApi.list(spaceId),
    enabled: !!spaceId,
  });
}

export function useConference(spaceId: string, conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.CONFERENCE(conferenceId),
    queryFn: () => conferencesApi.get(spaceId, conferenceId),
    enabled: !!spaceId && !!conferenceId,
  });
}

export function useCreateConference(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateConferenceRequest = {}) => {
      try {
        return await conferencesApi.create(spaceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.CONFERENCES(spaceId) });
    },
  });
}

export function usePreJoin(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.PRE_JOIN(conferenceId),
    queryFn: () => conferencesApi.preJoin(conferenceId),
    enabled: !!conferenceId,
    retry: false,
  });
}

export function useJoinConference() {
  return useMutation({
    mutationFn: async ({
      conferenceId,
      data,
      isGuest = false,
    }: {
      conferenceId: string;
      data: JoinConferenceRequest;
      isGuest?: boolean;
    }) => {
      try {
        if (isGuest) {
          return await conferencesApi.joinAsGuest(conferenceId, data);
        }
        return await conferencesApi.join(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useLeaveConference(conferenceId: string) {
  return useMutation({
    mutationFn: async () => {
      try {
        return await conferencesApi.leave(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useStartConference(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await conferencesApi.start(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.CONFERENCE(conferenceId) });
    },
  });
}

export function useEndConference(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await conferencesApi.end(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.CONFERENCE(conferenceId) });
    },
  });
}

export function useLockConference(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await conferencesApi.lock(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.CONFERENCE(conferenceId) });
    },
  });
}

export function useUnlockConference(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await conferencesApi.unlock(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.CONFERENCE(conferenceId) });
    },
  });
}

// ---------------------------------------------------------------------------
// Participants Hooks
// ---------------------------------------------------------------------------

export function useParticipants(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId),
    queryFn: () => participantsApi.list(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

export function useChangeRole(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ participantId, data }: { participantId: string; data: RoleChangeRequest }) => {
      try {
        return await participantsApi.changeRole(conferenceId, participantId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

export function useRemoveParticipant(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ participantId, reason }: { participantId: string; reason?: string }) => {
      try {
        return await participantsApi.remove(conferenceId, participantId, reason ? { reason } : undefined);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

export function useMuteParticipant(conferenceId: string) {
  return useMutation({
    mutationFn: async ({ participantId, trackKind }: { participantId: string; trackKind: TrackKind }) => {
      try {
        return await participantsApi.mute(conferenceId, participantId, { trackKind });
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

// ---------------------------------------------------------------------------
// Waiting Room Hooks
// ---------------------------------------------------------------------------

export function useWaitingRoom(conferenceId: string, enabled = true) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.WAITING_ROOM(conferenceId),
    queryFn: () => waitingRoomApi.list(conferenceId),
    enabled: !!conferenceId && enabled,
    refetchInterval: 3000,
  });
}

export function useAdmitParticipants(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: AdmitRequest) => {
      try {
        return await waitingRoomApi.admit(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.WAITING_ROOM(conferenceId) });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

export function useDenyParticipants(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: DenyRequest) => {
      try {
        return await waitingRoomApi.deny(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.WAITING_ROOM(conferenceId) });
    },
  });
}

// ---------------------------------------------------------------------------
// Chat Hooks
// ---------------------------------------------------------------------------

export function useMessages(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.MESSAGES(conferenceId),
    queryFn: () => chatApi.getMessages(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 2000,
  });
}

export function useSendMessage(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: SendMessageRequest) => {
      try {
        return await chatApi.sendMessage(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.MESSAGES(conferenceId) });
    },
  });
}

export function useDeleteMessage(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (messageId: string) => {
      try {
        return await chatApi.deleteMessage(conferenceId, messageId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.MESSAGES(conferenceId) });
    },
  });
}

// ---------------------------------------------------------------------------
// Reactions & Hand Raise Hooks
// ---------------------------------------------------------------------------

export function useSendReaction(conferenceId: string) {
  return useMutation({
    mutationFn: async (data: ReactionRequest) => {
      try {
        return await reactionsApi.sendReaction(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useRaiseHand(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await reactionsApi.raiseHand(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.RAISED_HANDS(conferenceId) });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.COLLABORATION(conferenceId) });
    },
  });
}

export function useLowerHand(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await reactionsApi.lowerHand(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.RAISED_HANDS(conferenceId) });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.COLLABORATION(conferenceId) });
    },
  });
}

export function useLowerOthersHand(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await reactionsApi.lowerOthersHand(conferenceId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.RAISED_HANDS(conferenceId) });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.COLLABORATION(conferenceId) });
    },
  });
}

export function useRaisedHands(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.RAISED_HANDS(conferenceId),
    queryFn: () => reactionsApi.getRaisedHands(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

// ---------------------------------------------------------------------------
// Breakouts Hooks
// ---------------------------------------------------------------------------

export function useBreakouts(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId),
    queryFn: () => breakoutsApi.list(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

export function useCreateBreakouts(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateBreakoutsRequest) => {
      try {
        return await breakoutsApi.create(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

export function useAssignBreakouts(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: AssignBreakoutsRequest) => {
      try {
        return await breakoutsApi.assign(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

export function useOpenBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (breakoutId: string) => {
      try {
        return await breakoutsApi.open(conferenceId, breakoutId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

export function useCloseBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ breakoutId, data }: { breakoutId: string; data?: CloseBreakoutRequest }) => {
      try {
        return await breakoutsApi.close(conferenceId, breakoutId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

export function useCloseAllBreakouts(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data?: CloseBreakoutRequest) => {
      try {
        return await breakoutsApi.closeAll(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

export function useAnnounceBreakouts(conferenceId: string) {
  return useMutation({
    mutationFn: async (data: BreakoutAnnouncementRequest) => {
      try {
        return await breakoutsApi.announce(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useEnterBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (breakoutId: string) => {
      try {
        return await breakoutsApi.enter(conferenceId, breakoutId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

export function useLeaveBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await breakoutsApi.leave(conferenceId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.BREAKOUTS(conferenceId) });
    },
  });
}

// ---------------------------------------------------------------------------
// Polls Hooks
// ---------------------------------------------------------------------------

export function usePolls(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.POLLS(conferenceId),
    queryFn: () => pollsApi.list(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

export function usePoll(conferenceId: string, pollId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.POLL(conferenceId, pollId),
    queryFn: () => pollsApi.get(conferenceId, pollId),
    enabled: !!conferenceId && !!pollId,
  });
}

export function useCreatePoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreatePollRequest) => {
      try {
        return await pollsApi.create(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.POLLS(conferenceId) });
    },
  });
}

export function useOpenPoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (pollId: string) => {
      try {
        return await pollsApi.open(conferenceId, pollId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: (_, pollId) => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.POLLS(conferenceId) });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.POLL(conferenceId, pollId) });
    },
  });
}

export function useClosePoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (pollId: string) => {
      try {
        return await pollsApi.close(conferenceId, pollId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: (_, pollId) => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.POLLS(conferenceId) });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.POLL(conferenceId, pollId) });
    },
  });
}

export function useSubmitPollResponse(conferenceId: string, pollId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: SubmitPollResponseRequest) => {
      try {
        return await pollsApi.submitResponse(conferenceId, pollId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.POLL_RESULTS(conferenceId, pollId) });
    },
  });
}

export function usePollResults(conferenceId: string, pollId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.POLL_RESULTS(conferenceId, pollId),
    queryFn: () => pollsApi.getResults(conferenceId, pollId),
    enabled: !!conferenceId && !!pollId,
    refetchInterval: 3000,
  });
}

// ---------------------------------------------------------------------------
// Q&A Hooks
// ---------------------------------------------------------------------------

export function useQuestions(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.QNA(conferenceId),
    queryFn: () => qnaApi.list(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

export function useSubmitQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: SubmitQuestionRequest) => {
      try {
        return await qnaApi.submit(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

export function useVoteQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (questionId: string) => {
      try {
        return await qnaApi.vote(conferenceId, questionId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

export function useUnvoteQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (questionId: string) => {
      try {
        return await qnaApi.unvote(conferenceId, questionId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

export function useAnswerQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ questionId, data }: { questionId: string; data: AnswerQuestionRequest }) => {
      try {
        return await qnaApi.answer(conferenceId, questionId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

export function useDismissQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (questionId: string) => {
      try {
        return await qnaApi.dismiss(conferenceId, questionId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

export function usePinQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (questionId: string) => {
      try {
        return await qnaApi.pin(conferenceId, questionId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

export function useUnpinQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (questionId: string) => {
      try {
        return await qnaApi.unpin(conferenceId, questionId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.QNA(conferenceId) });
    },
  });
}

// ---------------------------------------------------------------------------
// Collaboration Hooks
// ---------------------------------------------------------------------------

export function useCollaborationState(conferenceId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.V2.COLLABORATION(conferenceId),
    queryFn: () => collaborationApi.getState(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

export function useSpotlight(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: SpotlightRequest) => {
      try {
        return await collaborationApi.spotlight(conferenceId, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.COLLABORATION(conferenceId) });
    },
  });
}

export function useRemoveSpotlight(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await collaborationApi.removeSpotlight(conferenceId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.COLLABORATION(conferenceId) });
    },
  });
}

export function useLockAudio(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await collaborationApi.lockAudio(conferenceId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

export function useUnlockAudio(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await collaborationApi.unlockAudio(conferenceId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

export function useLockVideo(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await collaborationApi.lockVideo(conferenceId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

export function useUnlockVideo(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await collaborationApi.unlockVideo(conferenceId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.V2.PARTICIPANTS(conferenceId) });
    },
  });
}

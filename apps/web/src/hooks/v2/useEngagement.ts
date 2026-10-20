/**
 * V2 Engagement Hooks
 * React Query hooks for polls and Q&A
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { engagementService } from "@/services/v2";
import type {
  CreatePollRequest,
  SubmitPollResponseRequest,
  SubmitQuestionRequest,
  AnswerQuestionRequest,
} from "@/types/v2";

// Query Keys
export const engagementKeys = {
  all: ["v2", "engagement"] as const,
  polls: (conferenceId: string) => [...engagementKeys.all, "polls", conferenceId] as const,
  poll: (conferenceId: string, pollId: string) =>
    [...engagementKeys.polls(conferenceId), pollId] as const,
  pollResults: (conferenceId: string, pollId: string) =>
    [...engagementKeys.poll(conferenceId, pollId), "results"] as const,
  questions: (conferenceId: string) =>
    [...engagementKeys.all, "questions", conferenceId] as const,
};

// -------------------------------------------------------------------------
// Poll Queries
// -------------------------------------------------------------------------

/**
 * List polls for a conference
 */
export function usePolls(conferenceId: string) {
  return useQuery({
    queryKey: engagementKeys.polls(conferenceId),
    queryFn: () => engagementService.listPolls(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 3000,
  });
}

/**
 * Get a single poll
 */
export function usePoll(conferenceId: string, pollId: string) {
  return useQuery({
    queryKey: engagementKeys.poll(conferenceId, pollId),
    queryFn: () => engagementService.getPoll(conferenceId, pollId),
    enabled: !!conferenceId && !!pollId,
    refetchInterval: 2000,
  });
}

/**
 * Get poll results
 */
export function usePollResults(conferenceId: string, pollId: string) {
  return useQuery({
    queryKey: engagementKeys.pollResults(conferenceId, pollId),
    queryFn: () => engagementService.getPollResults(conferenceId, pollId),
    enabled: !!conferenceId && !!pollId,
    refetchInterval: 2000,
  });
}

// -------------------------------------------------------------------------
// Poll Mutations
// -------------------------------------------------------------------------

/**
 * Create a poll
 */
export function useCreatePoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: CreatePollRequest) =>
      engagementService.createPoll(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: engagementKeys.polls(conferenceId) });
    },
  });
}

/**
 * Open a poll
 */
export function useOpenPoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (pollId: string) => engagementService.openPoll(conferenceId, pollId),
    onSuccess: (_, pollId) => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.poll(conferenceId, pollId),
      });
      queryClient.invalidateQueries({ queryKey: engagementKeys.polls(conferenceId) });
    },
  });
}

/**
 * Close a poll
 */
export function useClosePoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (pollId: string) => engagementService.closePoll(conferenceId, pollId),
    onSuccess: (_, pollId) => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.poll(conferenceId, pollId),
      });
      queryClient.invalidateQueries({ queryKey: engagementKeys.polls(conferenceId) });
    },
  });
}

/**
 * Archive a poll
 */
export function useArchivePoll(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (pollId: string) =>
      engagementService.archivePoll(conferenceId, pollId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: engagementKeys.polls(conferenceId) });
    },
  });
}

/**
 * Submit poll response
 */
export function useSubmitPollResponse(conferenceId: string, pollId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: SubmitPollResponseRequest) =>
      engagementService.submitPollResponse(conferenceId, pollId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.pollResults(conferenceId, pollId),
      });
    },
  });
}

// -------------------------------------------------------------------------
// Q&A Queries
// -------------------------------------------------------------------------

/**
 * List questions for a conference
 */
export function useQuestions(conferenceId: string) {
  return useQuery({
    queryKey: engagementKeys.questions(conferenceId),
    queryFn: () => engagementService.listQuestions(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 3000,
  });
}

// -------------------------------------------------------------------------
// Q&A Mutations
// -------------------------------------------------------------------------

/**
 * Submit a question
 */
export function useSubmitQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: SubmitQuestionRequest) =>
      engagementService.submitQuestion(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Vote for a question
 */
export function useVoteQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (questionId: string) =>
      engagementService.voteQuestion(conferenceId, questionId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Remove vote from a question
 */
export function useUnvoteQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (questionId: string) =>
      engagementService.unvoteQuestion(conferenceId, questionId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Answer a question
 */
export function useAnswerQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      questionId,
      data,
    }: {
      questionId: string;
      data: AnswerQuestionRequest;
    }) => engagementService.answerQuestion(conferenceId, questionId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Dismiss a question
 */
export function useDismissQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (questionId: string) =>
      engagementService.dismissQuestion(conferenceId, questionId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Archive a question
 */
export function useArchiveQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (questionId: string) =>
      engagementService.archiveQuestion(conferenceId, questionId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Pin a question
 */
export function usePinQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (questionId: string) =>
      engagementService.pinQuestion(conferenceId, questionId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * Unpin a question
 */
export function useUnpinQuestion(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (questionId: string) =>
      engagementService.unpinQuestion(conferenceId, questionId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: engagementKeys.questions(conferenceId),
      });
    },
  });
}

/**
 * V2 Engagement Service
 * Handles polls and Q&A features
 */

import apiV2 from "./api";
import type {
  // Polls
  PollView,
  PollListResponse,
  CreatePollRequest,
  PollTransitionResponse,
  SubmitPollResponseRequest,
  PollResponseReceipt,
  PollResults,
  // Q&A
  QuestionView,
  QuestionListResponse,
  SubmitQuestionRequest,
  QuestionActionResponse,
  AnswerQuestionRequest,
} from "@/types/v2";

export const engagementService = {
  // -------------------------------------------------------------------------
  // Polls
  // -------------------------------------------------------------------------

  /**
   * List polls for a conference
   */
  listPolls: async (conferenceId: string) => {
    const response = await apiV2.get<PollListResponse>(
      `/conferences/${conferenceId}/polls`,
    );
    return response.data;
  },

  /**
   * Get a single poll
   */
  getPoll: async (conferenceId: string, pollId: string) => {
    const response = await apiV2.get<PollView>(
      `/conferences/${conferenceId}/polls/${pollId}`,
    );
    return response.data;
  },

  /**
   * Create a new poll
   */
  createPoll: async (conferenceId: string, data: CreatePollRequest) => {
    const response = await apiV2.post<PollView>(
      `/conferences/${conferenceId}/polls`,
      data,
    );
    return response.data;
  },

  /**
   * Open a poll (DRAFT → OPEN)
   */
  openPoll: async (conferenceId: string, pollId: string) => {
    const response = await apiV2.post<PollTransitionResponse>(
      `/conferences/${conferenceId}/polls/${pollId}/open`,
    );
    return response.data;
  },

  /**
   * Close a poll (OPEN → CLOSED)
   */
  closePoll: async (conferenceId: string, pollId: string) => {
    const response = await apiV2.post<PollTransitionResponse>(
      `/conferences/${conferenceId}/polls/${pollId}/close`,
    );
    return response.data;
  },

  /**
   * Archive a poll (CLOSED → ARCHIVED)
   */
  archivePoll: async (conferenceId: string, pollId: string) => {
    const response = await apiV2.post<PollTransitionResponse>(
      `/conferences/${conferenceId}/polls/${pollId}/archive`,
    );
    return response.data;
  },

  /**
   * Submit poll response
   */
  submitPollResponse: async (
    conferenceId: string,
    pollId: string,
    data: SubmitPollResponseRequest,
  ) => {
    const response = await apiV2.post<PollResponseReceipt>(
      `/conferences/${conferenceId}/polls/${pollId}/responses`,
      data,
    );
    return response.data;
  },

  /**
   * Get poll results
   */
  getPollResults: async (conferenceId: string, pollId: string) => {
    const response = await apiV2.get<PollResults>(
      `/conferences/${conferenceId}/polls/${pollId}/results`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Q&A
  // -------------------------------------------------------------------------

  /**
   * List questions for a conference
   */
  listQuestions: async (conferenceId: string) => {
    const response = await apiV2.get<QuestionListResponse>(
      `/conferences/${conferenceId}/questions`,
    );
    return response.data;
  },

  /**
   * Submit a question
   */
  submitQuestion: async (conferenceId: string, data: SubmitQuestionRequest) => {
    const response = await apiV2.post<QuestionView>(
      `/conferences/${conferenceId}/questions`,
      data,
    );
    return response.data;
  },

  /**
   * Vote for a question
   */
  voteQuestion: async (conferenceId: string, questionId: string) => {
    const response = await apiV2.post<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/vote`,
    );
    return response.data;
  },

  /**
   * Remove vote from a question
   */
  unvoteQuestion: async (conferenceId: string, questionId: string) => {
    const response = await apiV2.delete<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/vote`,
    );
    return response.data;
  },

  /**
   * Answer a question
   */
  answerQuestion: async (
    conferenceId: string,
    questionId: string,
    data: AnswerQuestionRequest,
  ) => {
    const response = await apiV2.post<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/answer`,
      data,
    );
    return response.data;
  },

  /**
   * Dismiss a question
   */
  dismissQuestion: async (conferenceId: string, questionId: string) => {
    const response = await apiV2.post<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/dismiss`,
    );
    return response.data;
  },

  /**
   * Archive a question
   */
  archiveQuestion: async (conferenceId: string, questionId: string) => {
    const response = await apiV2.post<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/archive`,
    );
    return response.data;
  },

  /**
   * Pin a question
   */
  pinQuestion: async (conferenceId: string, questionId: string) => {
    const response = await apiV2.post<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/pin`,
    );
    return response.data;
  },

  /**
   * Unpin a question
   */
  unpinQuestion: async (conferenceId: string, questionId: string) => {
    const response = await apiV2.delete<QuestionActionResponse>(
      `/conferences/${conferenceId}/questions/${questionId}/pin`,
    );
    return response.data;
  },
};

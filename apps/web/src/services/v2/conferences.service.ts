/**
 * V2 Conferences Service
 * Handles conference lifecycle management
 */

import apiV2 from "./api";
import type {
  ConferenceResponse,
  ConferenceListResponse,
  ConferenceActionResponse,
  CreateConferenceRequest,
  RescheduleConferenceRequest,
  ListConferencesQuery,
  JoinConferenceRequestV2,
  JoinConferenceResponseV2,
  LeaveConferenceRequest,
  LeaveConferenceResponse,
  PreJoinResponse,
} from "@/types/v2";

export const conferencesService = {
  // -------------------------------------------------------------------------
  // Conference CRUD
  // -------------------------------------------------------------------------

  /**
   * List conferences for a meeting space
   */
  list: async (spaceId: string, query?: ListConferencesQuery) => {
    const response = await apiV2.get<ConferenceListResponse>(
      `/meeting-spaces/${spaceId}/conferences`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Get a single conference by ID
   */
  get: async (id: string) => {
    const response = await apiV2.get<ConferenceResponse>(`/conferences/${id}`);
    return response.data;
  },

  /**
   * Create a new conference in a meeting space
   */
  create: async (spaceId: string, data?: CreateConferenceRequest) => {
    const response = await apiV2.post<ConferenceResponse>(
      `/meeting-spaces/${spaceId}/conferences`,
      data ?? {},
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Conference Lifecycle
  // -------------------------------------------------------------------------

  /**
   * Prepare conference (CREATED → PREPARING → WAITING)
   */
  prepare: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/prepare`,
    );
    return response.data;
  },

  /**
   * Start conference (→ LIVE)
   */
  start: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/start`,
    );
    return response.data;
  },

  /**
   * Lock conference (prevent new joins)
   */
  lock: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/lock`,
    );
    return response.data;
  },

  /**
   * Unlock conference (allow new joins)
   */
  unlock: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/unlock`,
    );
    return response.data;
  },

  /**
   * End conference (→ ENDING → ENDED)
   */
  end: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/end`,
    );
    return response.data;
  },

  /**
   * Cancel an unstarted conference
   */
  cancel: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/cancel`,
    );
    return response.data;
  },

  /**
   * Reschedule a conference
   */
  reschedule: async (id: string, data: RescheduleConferenceRequest) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${id}/reschedule`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Join / Leave
  // -------------------------------------------------------------------------

  /**
   * Get pre-join information (requirements, consent, etc.)
   */
  preJoin: async (id: string) => {
    const response = await apiV2.get<PreJoinResponse>(`/conferences/${id}/pre-join`);
    return response.data;
  },

  /**
   * Join a conference
   */
  join: async (id: string, data?: JoinConferenceRequestV2) => {
    const response = await apiV2.post<JoinConferenceResponseV2>(
      `/conferences/${id}/join`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Leave a conference
   */
  leave: async (id: string, data?: LeaveConferenceRequest) => {
    const response = await apiV2.post<LeaveConferenceResponse>(
      `/conferences/${id}/leave`,
      data ?? {},
    );
    return response.data;
  },
};

/**
 * V2 Meeting Spaces Service
 * Handles all meeting space CRUD operations
 */

import apiV2, { publicApiV2 } from "./api";
import type {
  MeetingSpaceResponse,
  MeetingSpaceListResponse,
  CreateMeetingSpaceRequest,
  UpdateMeetingSpaceRequest,
  DuplicateMeetingSpaceRequest,
  OwnerTransferRequest,
  InstantMeetingRequestV2,
  InstantMeetingResponseV2,
  ResolveResponse,
  RecurrenceResponse,
  SetRecurrenceRequest,
  RecurrenceExceptionRequest,
  RecurrenceExceptionResponse,
} from "@/types/v2";

export const spacesService = {
  // -------------------------------------------------------------------------
  // Meeting Spaces CRUD
  // -------------------------------------------------------------------------

  /**
   * List all meeting spaces the user has access to
   */
  list: async () => {
    const response = await apiV2.get<MeetingSpaceListResponse>("/meeting-spaces");
    return response.data;
  },

  /**
   * Get a single meeting space by ID
   */
  get: async (id: string) => {
    const response = await apiV2.get<MeetingSpaceResponse>(`/meeting-spaces/${id}`);
    return response.data;
  },

  /**
   * Create a new meeting space
   */
  create: async (data: CreateMeetingSpaceRequest) => {
    const response = await apiV2.post<MeetingSpaceResponse>("/meeting-spaces", data);
    return response.data;
  },

  /**
   * Update an existing meeting space
   */
  update: async (id: string, data: UpdateMeetingSpaceRequest) => {
    const response = await apiV2.patch<MeetingSpaceResponse>(`/meeting-spaces/${id}`, data);
    return response.data;
  },

  /**
   * Archive a meeting space
   */
  archive: async (id: string, version?: number) => {
    const response = await apiV2.post<MeetingSpaceResponse>(
      `/meeting-spaces/${id}/archive`,
      version ? { version } : {},
    );
    return response.data;
  },

  /**
   * Duplicate a meeting space
   */
  duplicate: async (id: string, data?: DuplicateMeetingSpaceRequest) => {
    const response = await apiV2.post<MeetingSpaceResponse>(
      `/meeting-spaces/${id}/duplicate`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Transfer ownership of a meeting space
   */
  transferOwnership: async (id: string, data: OwnerTransferRequest) => {
    const response = await apiV2.post<MeetingSpaceResponse>(
      `/meeting-spaces/${id}/owner-transfer`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Instant Meeting
  // -------------------------------------------------------------------------

  /**
   * Create an instant meeting (space + live conference)
   */
  createInstant: async (data?: InstantMeetingRequestV2) => {
    const response = await apiV2.post<InstantMeetingResponseV2>(
      "/instant-meetings",
      data ?? {},
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Resolve
  // -------------------------------------------------------------------------

  /**
   * Resolve any meeting identifier (code, ID, legacy) to a meeting space
   */
  resolve: async (identifier: string) => {
    const response = await publicApiV2.get<ResolveResponse>(`/resolve/${identifier}`);
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Recurrence
  // -------------------------------------------------------------------------

  /**
   * Get recurrence rule for a meeting space
   */
  getRecurrence: async (spaceId: string) => {
    const response = await apiV2.get<RecurrenceResponse>(
      `/meeting-spaces/${spaceId}/recurrence`,
    );
    return response.data;
  },

  /**
   * Set or update recurrence rule
   */
  setRecurrence: async (spaceId: string, data: SetRecurrenceRequest) => {
    const response = await apiV2.put<RecurrenceResponse>(
      `/meeting-spaces/${spaceId}/recurrence`,
      data,
    );
    return response.data;
  },

  /**
   * Delete recurrence rule
   */
  deleteRecurrence: async (spaceId: string) => {
    const response = await apiV2.delete<{ deleted: boolean }>(
      `/meeting-spaces/${spaceId}/recurrence`,
    );
    return response.data;
  },

  /**
   * Get recurrence exceptions
   */
  getExceptions: async (spaceId: string) => {
    const response = await apiV2.get<RecurrenceExceptionResponse[]>(
      `/meeting-spaces/${spaceId}/recurrence/exceptions`,
    );
    return response.data;
  },

  /**
   * Create or update a recurrence exception
   */
  setException: async (spaceId: string, data: RecurrenceExceptionRequest) => {
    const response = await apiV2.put<RecurrenceExceptionResponse>(
      `/meeting-spaces/${spaceId}/recurrence/exceptions`,
      data,
    );
    return response.data;
  },
};

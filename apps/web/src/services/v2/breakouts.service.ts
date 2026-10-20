/**
 * V2 Breakouts Service
 * Handles breakout room management
 */

import apiV2 from "./api";
import type {
  BreakoutListResponse,
  CreateBreakoutsRequest,
  BreakoutListCreatedResponse,
  AssignBreakoutsRequest,
  AssignmentResult,
  MoveParticipantRequest,
  MediaContextResponse,
  RoomAssignmentsPatch,
  BreakoutView,
  BreakoutResponse,
  BreakoutAnnouncementRequest,
  AnnouncementResponse,
  CloseBreakoutRequest,
  CloseBreakoutsResponse,
} from "@/types/v2";

export const breakoutsService = {
  /**
   * List breakout rooms for a conference
   */
  list: async (conferenceId: string) => {
    const response = await apiV2.get<BreakoutListResponse>(
      `/conferences/${conferenceId}/breakouts`,
    );
    return response.data;
  },

  /**
   * Create breakout rooms
   */
  create: async (conferenceId: string, data: CreateBreakoutsRequest) => {
    const response = await apiV2.post<BreakoutListCreatedResponse>(
      `/conferences/${conferenceId}/breakouts`,
      data,
    );
    return response.data;
  },

  /**
   * Assign participants to breakout rooms
   */
  assign: async (conferenceId: string, data: AssignBreakoutsRequest) => {
    const response = await apiV2.post<AssignmentResult>(
      `/conferences/${conferenceId}/breakouts/assign`,
      data,
    );
    return response.data;
  },

  /**
   * Move a participant between rooms
   */
  moveParticipant: async (conferenceId: string, data: MoveParticipantRequest) => {
    const response = await apiV2.post<MediaContextResponse>(
      `/conferences/${conferenceId}/breakouts/move`,
      data,
    );
    return response.data;
  },

  /**
   * Leave current breakout (return to main or assigned room)
   */
  leave: async (conferenceId: string) => {
    const response = await apiV2.post<MediaContextResponse>(
      `/conferences/${conferenceId}/breakouts/leave`,
    );
    return response.data;
  },

  /**
   * Send announcement to all breakout rooms
   */
  announceAll: async (conferenceId: string, data: BreakoutAnnouncementRequest) => {
    const response = await apiV2.post<AnnouncementResponse>(
      `/conferences/${conferenceId}/breakouts/announcement`,
      data,
    );
    return response.data;
  },

  /**
   * Close all breakout rooms
   */
  closeAll: async (conferenceId: string, data?: CloseBreakoutRequest) => {
    const response = await apiV2.post<CloseBreakoutsResponse>(
      `/conferences/${conferenceId}/breakouts/close-all`,
      data ?? {},
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Single Breakout Operations
  // -------------------------------------------------------------------------

  /**
   * Patch assignments for a single breakout room
   */
  patchAssignments: async (
    conferenceId: string,
    breakoutId: string,
    data: RoomAssignmentsPatch,
  ) => {
    const response = await apiV2.patch<BreakoutView>(
      `/conferences/${conferenceId}/breakouts/${breakoutId}/assignments`,
      data,
    );
    return response.data;
  },

  /**
   * Open a breakout room
   */
  open: async (conferenceId: string, breakoutId: string) => {
    const response = await apiV2.post<BreakoutResponse>(
      `/conferences/${conferenceId}/breakouts/${breakoutId}/open`,
    );
    return response.data;
  },

  /**
   * Enter a breakout room (privileged visit)
   */
  enter: async (conferenceId: string, breakoutId: string) => {
    const response = await apiV2.post<MediaContextResponse>(
      `/conferences/${conferenceId}/breakouts/${breakoutId}/enter`,
    );
    return response.data;
  },

  /**
   * Send announcement to a single breakout room
   */
  announce: async (
    conferenceId: string,
    breakoutId: string,
    data: BreakoutAnnouncementRequest,
  ) => {
    const response = await apiV2.post<AnnouncementResponse>(
      `/conferences/${conferenceId}/breakouts/${breakoutId}/announcement`,
      data,
    );
    return response.data;
  },

  /**
   * Close a single breakout room
   */
  close: async (
    conferenceId: string,
    breakoutId: string,
    data?: CloseBreakoutRequest,
  ) => {
    const response = await apiV2.post<CloseBreakoutsResponse>(
      `/conferences/${conferenceId}/breakouts/${breakoutId}/close`,
      data ?? {},
    );
    return response.data;
  },
};

/**
 * V2 Members Service
 * Handles meeting space membership management
 */

import apiV2 from "./api";
import type {
  MemberResponse,
  MemberListResponse,
  AddMemberRequest,
  UpdateMemberRequest,
} from "@/types/v2";

export const membersService = {
  /**
   * List all members of a meeting space
   */
  list: async (spaceId: string) => {
    const response = await apiV2.get<MemberListResponse>(
      `/meeting-spaces/${spaceId}/members`,
    );
    return response.data;
  },

  /**
   * Add a member to a meeting space
   */
  add: async (spaceId: string, data: AddMemberRequest) => {
    const response = await apiV2.post<MemberResponse>(
      `/meeting-spaces/${spaceId}/members`,
      data,
    );
    return response.data;
  },

  /**
   * Update a member's role
   */
  update: async (spaceId: string, memberId: string, data: UpdateMemberRequest) => {
    const response = await apiV2.patch<MemberResponse>(
      `/meeting-spaces/${spaceId}/members/${memberId}`,
      data,
    );
    return response.data;
  },

  /**
   * Revoke a member's access
   */
  revoke: async (spaceId: string, memberId: string) => {
    const response = await apiV2.delete<MemberResponse>(
      `/meeting-spaces/${spaceId}/members/${memberId}`,
    );
    return response.data;
  },
};

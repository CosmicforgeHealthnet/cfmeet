/**
 * V2 Invitations Service
 * Handles meeting invitation management
 */

import apiV2, { publicApiV2 } from "./api";
import type {
  InvitationResponse,
  CreateInvitationRequest,
  CreateInvitationResponse,
  ResolveInvitationResponse,
} from "@/types/v2";

export const invitationsService = {
  /**
   * List all invitations for a meeting space
   */
  list: async (spaceId: string) => {
    const response = await apiV2.get<InvitationResponse[]>(
      `/meeting-spaces/${encodeURIComponent(spaceId)}/invitations`,
    );
    return response.data;
  },

  /**
   * Create a new invitation
   */
  create: async (spaceId: string, data?: CreateInvitationRequest) => {
    const response = await apiV2.post<CreateInvitationResponse>(
      `/meeting-spaces/${encodeURIComponent(spaceId)}/invitations`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Revoke an invitation
   */
  revoke: async (spaceId: string, invitationId: string) => {
    const response = await apiV2.post<InvitationResponse>(
      `/meeting-spaces/${encodeURIComponent(spaceId)}/invitations/${encodeURIComponent(invitationId)}/revoke`,
      {},
    );
    return response.data;
  },

  /**
   * Resolve an invitation token (public endpoint)
   */
  resolve: async (token: string) => {
    const response = await publicApiV2.post<ResolveInvitationResponse>(
      `/invitations/${encodeURIComponent(token)}/resolve`,
      {},
    );
    return response.data;
  },
};

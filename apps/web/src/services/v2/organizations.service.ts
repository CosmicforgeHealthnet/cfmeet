/**
 * V2 Organizations Service
 * Handles multi-tenant organization management
 */

import apiV2 from "./api";
import type {
  OrganizationResponse,
  CreateOrganizationRequest,
  UpdateOrganizationRequest,
  OrganizationMemberResponse,
  UpsertOrganizationMemberRequest,
  MyOrganizationResponse,
} from "@/types/v2";

export const organizationsService = {
  // -------------------------------------------------------------------------
  // Admin Operations
  // -------------------------------------------------------------------------

  /**
   * Create an organization (platform admin)
   */
  create: async (data: CreateOrganizationRequest) => {
    const response = await apiV2.post<OrganizationResponse>(
      "/admin/organizations",
      data,
    );
    return response.data;
  },

  /**
   * Update organization status (platform admin)
   */
  update: async (orgId: string, data: UpdateOrganizationRequest) => {
    const response = await apiV2.patch<OrganizationResponse>(
      `/admin/organizations/${orgId}`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Member Management
  // -------------------------------------------------------------------------

  /**
   * List organization members
   */
  listMembers: async (orgId: string) => {
    const response = await apiV2.get<OrganizationMemberResponse[]>(
      `/organizations/${orgId}/members`,
    );
    return response.data;
  },

  /**
   * Add or update organization member
   */
  upsertMember: async (
    orgId: string,
    userId: string,
    data: UpsertOrganizationMemberRequest,
  ) => {
    const response = await apiV2.put<OrganizationMemberResponse>(
      `/organizations/${orgId}/members/${userId}`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // My Organizations
  // -------------------------------------------------------------------------

  /**
   * Get organizations the current user belongs to
   */
  mine: async () => {
    const response = await apiV2.get<MyOrganizationResponse[]>("/me/organizations");
    return response.data;
  },
};

/**
 * V2 Profiles Service
 * Handles profile policies, registration, and backstage
 */

import apiV2, { publicApiV2 } from "./api";
import type {
  ProfilePolicyResponse,
  ProfilePolicyUpdateRequest,
  RegistrationRequest,
  RegistrationView,
  RegistrationCreated,
  RegistrationListResponse,
  BackstageResponse,
} from "@/types/v2";

export const profilesService = {
  // -------------------------------------------------------------------------
  // Profile Policy
  // -------------------------------------------------------------------------

  /**
   * Get profile policy for a meeting space
   */
  getPolicy: async (spaceId: string) => {
    const response = await apiV2.get<ProfilePolicyResponse>(
      `/meeting-spaces/${spaceId}/profile-policy`,
    );
    return response.data;
  },

  /**
   * Update profile policy
   */
  updatePolicy: async (spaceId: string, data: ProfilePolicyUpdateRequest) => {
    const response = await apiV2.patch<ProfilePolicyResponse>(
      `/meeting-spaces/${spaceId}/profile-policy`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Registration
  // -------------------------------------------------------------------------

  /**
   * Register for a webinar/town hall (public endpoint)
   */
  register: async (spaceId: string, data: RegistrationRequest) => {
    const response = await publicApiV2.post<RegistrationCreated>(
      `/meeting-spaces/${spaceId}/registrations`,
      data,
    );
    return response.data;
  },

  /**
   * List registrations (organizer view)
   */
  listRegistrations: async (spaceId: string) => {
    const response = await apiV2.get<RegistrationListResponse>(
      `/meeting-spaces/${spaceId}/registrations`,
    );
    return response.data;
  },

  /**
   * Get a single registration
   */
  getRegistration: async (spaceId: string, registrationId: string) => {
    const response = await apiV2.get<RegistrationView>(
      `/meeting-spaces/${spaceId}/registrations/${registrationId}`,
    );
    return response.data;
  },

  /**
   * Cancel a registration
   */
  cancelRegistration: async (spaceId: string, registrationId: string) => {
    const response = await apiV2.delete<RegistrationView>(
      `/meeting-spaces/${spaceId}/registrations/${registrationId}`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Backstage
  // -------------------------------------------------------------------------

  /**
   * Enter backstage
   */
  enterBackstage: async (conferenceId: string) => {
    const response = await apiV2.post<BackstageResponse>(
      `/conferences/${conferenceId}/backstage`,
    );
    return response.data;
  },

  /**
   * Leave backstage
   */
  leaveBackstage: async (conferenceId: string) => {
    const response = await apiV2.delete<BackstageResponse>(
      `/conferences/${conferenceId}/backstage`,
    );
    return response.data;
  },
};

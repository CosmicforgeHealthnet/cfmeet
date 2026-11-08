import api from "./api";
import type { UserProfile, UpdateProfileRequest } from "@/types/user";

/**
 * Legacy v1 `/users/me` reads used by the dashboard shell. Profile editing, sessions,
 * password change and deactivation live in account.service.ts (v2 profile, ACC-026).
 */
export const userService = {
  getProfile: async () => {
    const response = await api.get<UserProfile>("/users/me");
    return response.data;
  },

  updateProfile: async (data: UpdateProfileRequest) => {
    const response = await api.patch<UserProfile>("/users/me", data);
    return response.data;
  },
};

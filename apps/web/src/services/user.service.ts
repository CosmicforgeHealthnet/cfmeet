import api from "./api";
import type { UserProfile, UpdateProfileRequest } from "@/types/user";
import type { MessageResponse } from "@/types/auth";

export const userService = {
  getProfile: async () => {
    const response = await api.get<UserProfile>("/users/me");
    return response.data;
  },

  updateProfile: async (data: UpdateProfileRequest) => {
    const response = await api.patch<UserProfile>("/users/me", data);
    return response.data;
  },

  deactivateAccount: async () => {
    const response = await api.post<MessageResponse>("/users/me/deactivate");
    return response.data;
  },
};

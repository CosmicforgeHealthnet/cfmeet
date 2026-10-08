import { api } from "@/lib/api/client";
import type { User, UpdateUserRequest, UpdateUserResponse } from "../types";

export const userApi = {
  async getMe(): Promise<User> {
    const response = await api.get<{ user: User }>("/users/me");
    return response.data.user;
  },

  async updateMe(data: UpdateUserRequest): Promise<UpdateUserResponse> {
    const response = await api.put<UpdateUserResponse>("/users/me", data);
    return response.data;
  },
};

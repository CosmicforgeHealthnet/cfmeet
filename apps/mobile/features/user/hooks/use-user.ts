import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { QUERY_KEYS } from "@/lib/constants";
import { userApi } from "../api/user.api";
import { userStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";
import { getApiError } from "@/lib/api/client";
import type { UpdateUserRequest } from "../types";

export function useUser() {
  const { isAuthenticated } = useAuthStore();

  return useQuery({
    queryKey: QUERY_KEYS.USER,
    queryFn: () => userApi.getMe(),
    enabled: isAuthenticated,
  });
}

export function useUpdateUser() {
  const queryClient = useQueryClient();
  const { setUser } = useAuthStore();

  return useMutation({
    mutationFn: async (data: UpdateUserRequest) => {
      try {
        return await userApi.updateMe(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: async (response) => {
      const user = response.user;
      setUser({
        id: user.id,
        email: user.email,
        username: user.username,
        role: user.role,
        status: user.status,
      });
      await userStorage.setUserInfo({
        id: user.id,
        email: user.email,
        username: user.username,
        role: user.role,
        status: user.status,
      });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.USER });
    },
  });
}

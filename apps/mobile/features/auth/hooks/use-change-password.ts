import { useMutation } from "@tanstack/react-query";
import { authApi } from "../api/auth.api";
import { getApiError } from "@/lib/api/client";
import type { ChangePasswordRequest } from "../types";

export function useChangePassword() {
  return useMutation({
    mutationFn: async (data: ChangePasswordRequest) => {
      try {
        return await authApi.changePassword(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

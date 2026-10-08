import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { getApiError } from "@/lib/api/client";
import type { RegisterRequest, RegisterResponse } from "../types";

export function useRegister() {
  return useMutation({
    mutationFn: async (data: RegisterRequest): Promise<RegisterResponse> => {
      try {
        return await authApi.register(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: (response) => {
      router.replace({
        pathname: "/verify",
        params: { email: response.user.email },
      });
    },
  });
}

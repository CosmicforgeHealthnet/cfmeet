import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { tokenStorage, userStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";
import { getApiError } from "@/lib/api/client";
import type { VerifyEmailRequest, VerifyEmailResponse } from "../types";

export function useVerifyEmail() {
  const { setUser, setIsAuthenticated } = useAuthStore();

  return useMutation({
    mutationFn: async (data: VerifyEmailRequest): Promise<VerifyEmailResponse> => {
      try {
        return await authApi.verifyEmail(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: async (response) => {
      await tokenStorage.setTokens(response.access_token, response.refresh_token);
      await userStorage.setUserInfo(response.user);

      setUser(response.user);
      setIsAuthenticated(true);

      router.replace("/(tabs)");
    },
  });
}

export function useResendVerification() {
  return useMutation({
    mutationFn: async (email: string) => {
      try {
        return await authApi.resendVerification({ email });
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { getApiError } from "@/lib/api/client";
import type { VerifyEmailRequest, VerifyEmailResponse } from "../types";

export function useVerifyEmail() {
  return useMutation({
    mutationFn: async (data: VerifyEmailRequest): Promise<VerifyEmailResponse> => {
      try {
        return await authApi.verifyEmail(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      // After email verification, redirect to login
      router.replace("/login");
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

import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { getApiError } from "@/lib/api/client";
import type {
  ForgotPasswordRequest,
  VerifyResetTokenRequest,
  ResetPasswordRequest,
} from "../types";

export function useForgotPassword() {
  return useMutation({
    mutationFn: async (data: ForgotPasswordRequest) => {
      try {
        return await authApi.forgotPassword(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useVerifyResetToken() {
  return useMutation({
    mutationFn: async (data: VerifyResetTokenRequest) => {
      try {
        return await authApi.verifyResetToken(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useResetPassword() {
  return useMutation({
    mutationFn: async (data: ResetPasswordRequest) => {
      try {
        return await authApi.resetPassword(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      router.replace("/login");
    },
  });
}

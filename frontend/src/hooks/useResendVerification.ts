import { useMutation } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";
import type { ResendVerificationRequest, ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useResendVerification() {
  return useMutation({
    mutationFn: (data: ResendVerificationRequest) =>
      authService.resendVerification(data),
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to resend code";
      console.error("Resend verification error:", message);
    },
  });
}

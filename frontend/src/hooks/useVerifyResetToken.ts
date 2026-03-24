import { useMutation } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";
import type { VerifyResetTokenRequest, ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useVerifyResetToken() {
  return useMutation({
    mutationFn: (data: VerifyResetTokenRequest) =>
      authService.verifyResetToken(data),
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Token verification failed";
      console.error("Verify reset token error:", message);
    },
  });
}

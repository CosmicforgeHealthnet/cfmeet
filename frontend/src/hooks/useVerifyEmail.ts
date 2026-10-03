import { useMutation } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";
import type { VerifyEmailRequest, ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useVerifyEmail() {
  return useMutation({
    mutationFn: (data: VerifyEmailRequest) => authService.verifyEmail(data),
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Verification failed";
      console.error("Verify email error:", message);
    },
  });
}

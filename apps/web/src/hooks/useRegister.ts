import { useMutation } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";
import type { RegisterRequest, ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useRegister() {
  return useMutation({
    mutationFn: (data: RegisterRequest) => authService.register(data),
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Registration failed";
      console.error("Register error:", message);
    },
  });
}

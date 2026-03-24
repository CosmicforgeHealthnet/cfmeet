import { useMutation } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";
import type { LoginRequest, ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

interface LoginParams {
  data: LoginRequest;
  rememberMe: boolean;
}

export function useLogin() {
  return useMutation({
    mutationFn: ({ data, rememberMe }: LoginParams) =>
      authService.login(data, rememberMe),
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Login failed";
      console.error("Login error:", message);
    },
  });
}

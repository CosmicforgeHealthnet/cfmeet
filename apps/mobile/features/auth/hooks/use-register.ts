import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { ApiRequestError, decodeApiError } from "@/lib/api/errors";
import type { RegisterResponse } from "@/lib/api/generated/meeting-service";
import type { RegisterRequest } from "../types";

export function useRegister() {
  return useMutation({
    mutationFn: async (data: RegisterRequest): Promise<RegisterResponse> => {
      try {
        return await authApi.register(data);
      } catch (error) {
        throw new ApiRequestError(decodeApiError(error));
      }
    },
    onSuccess: (response, variables) => {
      router.replace({
        pathname: "/verify",
        params: { email: response.email ?? variables.email },
      });
    },
  });
}

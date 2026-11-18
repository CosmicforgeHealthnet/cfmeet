import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { ApiRequestError, decodeApiError } from "@/lib/api/errors";
import type { RegisterRequest } from "../types";

import type { RegisterResponse } from "../legacy-contract";

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

import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { tokenStorage, userStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";
import { getApiError } from "@/lib/api/client";
import type { LoginRequest, LoginResponse } from "../types";

export function useLogin() {
  const { setUser, setIsAuthenticated } = useAuthStore();

  return useMutation({
    mutationFn: async (data: LoginRequest): Promise<LoginResponse> => {
      console.log("=== LOGIN REQUEST ===");
      console.log("Data:", JSON.stringify(data, null, 2));
      try {
        const response = await authApi.login(data);
        console.log("=== LOGIN SUCCESS ===");
        console.log("Response:", JSON.stringify(response, null, 2));
        return response;
      } catch (error) {
        console.log("=== LOGIN ERROR ===");
        console.log("Error:", error);
        const apiError = getApiError(error);
        console.log("API Error:", JSON.stringify(apiError, null, 2));
        throw new Error(apiError.message);
      }
    },
    onSuccess: async (response, variables) => {
      await tokenStorage.setTokens(response.access_token, response.refresh_token);
      await userStorage.setUserInfo(response.user);
      await userStorage.setRememberMe(variables.remember_me ?? false);

      setUser(response.user);
      setIsAuthenticated(true);

      if (response.user.status === "pending") {
        router.replace({
          pathname: "/verify",
          params: { email: response.user.email },
        });
      } else {
        router.replace("/(tabs)");
      }
    },
  });
}

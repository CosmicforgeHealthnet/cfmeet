import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { getApiError } from "@/lib/api/client";
import { userStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";
import type { MessageResponse, VerifyEmailRequest } from "@/lib/api/generated/meeting-service";

export function useVerifyEmail() {
  return useMutation({
    mutationFn: async (data: VerifyEmailRequest): Promise<MessageResponse> => {
      try {
        return await authApi.verifyEmail(data);
      } catch (error) {
        throw new Error(getApiError(error).message);
      }
    },
    onSuccess: async () => {
      // A user who signed in before verifying keeps the session and continues to the app.
      const { isAuthenticated, user, setUser } = useAuthStore.getState();
      if (isAuthenticated && user) {
        const verified = { ...user, status: "active" };
        setUser(verified);
        await userStorage.setUserInfo(verified);
        router.replace("/(tabs)");
      } else {
        router.replace("/login");
      }
    },
  });
}

export function useResendVerification() {
  return useMutation({
    mutationFn: async (email: string) => {
      try {
        return await authApi.resendVerification({ email });
      } catch (error) {
        throw new Error(getApiError(error).message);
      }
    },
  });
}

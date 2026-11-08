import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { tokenStorage, userStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";
import { decodeApiError, ApiRequestError } from "@/lib/api/errors";
import { routeAfterSignIn } from "@/lib/auth/account-status";
import type { LoginResponse } from "@/lib/api/generated/meeting-service";
import type { LoginRequest } from "../types";

/**
 * Email + password sign-in (ACC-012): nothing about the request, the response or the tokens is
 * logged; tokens go only to SecureStore. Unverified accounts (either status spelling, or a
 * `403 EMAIL_VERIFICATION_REQUIRED` when the deployment enforces verification) are routed to
 * the verification screen with the email the user typed — the login body has no email.
 */
export function useLogin() {
  const { setUser, setIsAuthenticated } = useAuthStore();

  return useMutation({
    mutationFn: async (data: LoginRequest): Promise<LoginResponse> => {
      let response: LoginResponse;
      try {
        response = await authApi.login({ email: data.email.trim(), password: data.password });
      } catch (error) {
        const decoded = decodeApiError(error);
        if (decoded.kind === "verification_required") {
          router.replace({ pathname: "/verify", params: { email: data.email.trim().toLowerCase() } });
        }
        throw new ApiRequestError(decoded);
      }
      if (routeAfterSignIn(response.user.status, data.email).kind === "blocked") {
        // Nothing is stored for an inactive or suspended account.
        throw new Error("This account is not active. Contact support to restore access.");
      }
      return response;
    },
    onSuccess: async (response, variables) => {
      const next = routeAfterSignIn(response.user.status, variables.email);
      await tokenStorage.setTokens(response.access_token, response.refresh_token);
      const user = { ...response.user, email: variables.email.trim().toLowerCase() };
      await userStorage.setUserInfo(user);
      await userStorage.setRememberMe(variables.remember_me ?? false);
      setUser(user);
      setIsAuthenticated(true);

      if (next.kind === "verify") {
        router.replace({ pathname: "/verify", params: { email: next.email } });
      } else {
        router.replace("/(tabs)");
      }
    },
  });
}

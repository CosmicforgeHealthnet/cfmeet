import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { authApi } from "../api/auth.api";
import { clearAllStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";
import { queryClient } from "@/lib/api/query-client";
import { useLiveSession } from "@/features/room/live-session";

export function useLogout() {
  const { logout } = useAuthStore();

  return useMutation({
    mutationFn: async () => {
      try {
        await authApi.logout();
      } catch {
        // Continue with logout even if API call fails
      }
    },
    onSettled: async () => {
      await clearAllStorage();
      await useLiveSession.getState().clear();
      logout();
      queryClient.clear();
      router.replace("/login");
    },
  });
}

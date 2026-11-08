import { useEffect } from "react";
import { router } from "expo-router";
import { tokenStorage, userStorage, clearAllStorage } from "@/lib/storage/secure-store";
import { onSessionEnded } from "@/lib/api/client";
import { queryClient } from "@/lib/api/query-client";
import { useAuthStore } from "@/store/auth.store";
import { useLiveSession } from "@/features/room/live-session";

interface AuthProviderProps {
  children: React.ReactNode;
}

export function AuthProvider({ children }: AuthProviderProps) {
  const { setUser, setIsAuthenticated, setIsLoading } = useAuthStore();

  useEffect(() => {
    // The server refused the refresh token (revoked / expired): sign out locally, drop every
    // cached response, and go to sign-in. Network failures never end the session.
    onSessionEnded(async () => {
      await clearAllStorage();
      await useLiveSession.getState().clear();
      useAuthStore.getState().logout();
      queryClient.clear();
      router.replace("/login");
    });

    const initAuth = async () => {
      try {
        const [accessToken, userInfo] = await Promise.all([
          tokenStorage.getAccessToken(),
          userStorage.getUserInfo(),
        ]);

        if (accessToken && userInfo) {
          setUser(userInfo);
          setIsAuthenticated(true);
        } else {
          setIsAuthenticated(false);
        }
      } catch {
        setIsAuthenticated(false);
      } finally {
        setIsLoading(false);
      }
    };

    initAuth();
  }, [setUser, setIsAuthenticated, setIsLoading]);

  return <>{children}</>;
}

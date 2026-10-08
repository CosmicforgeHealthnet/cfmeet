import { useEffect } from "react";
import { tokenStorage, userStorage } from "@/lib/storage/secure-store";
import { useAuthStore } from "@/store/auth.store";

interface AuthProviderProps {
  children: React.ReactNode;
}

export function AuthProvider({ children }: AuthProviderProps) {
  const { setUser, setIsAuthenticated, setIsLoading } = useAuthStore();

  useEffect(() => {
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

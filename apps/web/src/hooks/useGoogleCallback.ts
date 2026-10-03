import { useMemo, useEffect } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { cookieStore, storageStore } from "@/store";

interface GoogleCallbackParams {
  error: string | null;
  accessToken: string | null;
  refreshToken: string | null;
  userId: string | null;
  username: string | null;
  role: string | null;
  status: string | null;
}

interface GoogleCallbackResult {
  error: string;
  isLoading: boolean;
  params: GoogleCallbackParams;
}

export function useGoogleCallback(): GoogleCallbackResult {
  const searchParams = useSearchParams();
  const router = useRouter();

  // Extract all parameters from URL
  const params: GoogleCallbackParams = useMemo(
    () => ({
      error: searchParams.get("error"),
      accessToken: searchParams.get("access_token"),
      refreshToken: searchParams.get("refresh_token"),
      userId: searchParams.get("user_id"),
      username: searchParams.get("username"),
      role: searchParams.get("role"),
      status: searchParams.get("status"),
    }),
    [searchParams],
  );

  // Validate parameters and determine error state
  const error = useMemo(() => {
    if (params.error) return params.error;
    if (
      !params.accessToken ||
      !params.refreshToken ||
      !params.userId ||
      !params.username ||
      !params.role ||
      !params.status
    ) {
      return "Invalid callback parameters. Please try again.";
    }
    return "";
  }, [params]);

  // Handle successful authentication
  useEffect(() => {
    if (error) return;

    const { accessToken, refreshToken, userId, username, role, status } =
      params;

    // Store tokens and user info
    cookieStore.setTokens(accessToken!, refreshToken!, true);
    storageStore.setUser({
      id: userId!,
      username: username!,
      role: role!,
      status: status!,
    });

    // Redirect based on user status
    if (status === "active") {
      router.replace("/dashboard");
    } else if (status === "pendingverification") {
      // For Google OAuth, this shouldn't happen, but handle it gracefully
      router.replace("/verify");
    } else {
      // Handle inactive or other statuses
      router.replace("/login");
    }
  }, [error, params, router]);

  return {
    error,
    isLoading: !error,
    params,
  };
}

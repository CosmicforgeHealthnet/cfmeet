import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { userService } from "@/services/user.service";
import type { UpdateProfileRequest } from "@/types/user";
import { AxiosError } from "axios";
import { ApiErrorResponse } from "@/types/auth";

import { storageStore } from "@/store";

export function useProfile() {
  // Only fetch profile for authenticated users (not guests)
  // Guests have tokens but no stored user info
  const isAuthenticatedUser =
    typeof window !== "undefined" && !!storageStore.getUser();

  return useQuery({
    queryKey: ["user-profile"],
    queryFn: () => userService.getProfile(),
    staleTime: Infinity, // never becomes stale
    gcTime: 1000 * 60 * 10, // keep cache for 10 mins
    refetchOnWindowFocus: false, // User switched tabs? Don't refetch.
    refetchOnReconnect: false, // User switched tabs? Don't refetch.
    refetchOnMount: false, // Component remounted? Don't refetch
    enabled: isAuthenticatedUser,
    retry: false,
  });
}

export function useUpdateProfile() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: UpdateProfileRequest) => userService.updateProfile(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["user-profile"] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      console.error(
        "Profile update error:",
        error.response?.data?.error?.message,
      );
    },
  });
}

export function useDeactivateAccount() {
  return useMutation({
    mutationFn: () => userService.deactivateAccount(),
    onError: (error: AxiosError<ApiErrorResponse>) => {
      console.error(
        "Deactivation error:",
        error.response?.data?.error?.message,
      );
    },
  });
}

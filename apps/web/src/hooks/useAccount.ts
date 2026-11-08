import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { accountService } from "@/services/account.service";
import { storageStore } from "@/store";
import type { UpdateAccountProfileRequest } from "@/types/generated/meeting-service";

export const accountKeys = {
  profile: () => ["account", "profile"] as const,
  sessions: () => ["account", "sessions"] as const,
};

/** `GET /api/v2/me/profile` (Section 9). */
export function useAccountProfile() {
  return useQuery({ queryKey: accountKeys.profile(), queryFn: () => accountService.getProfile(), retry: 1 });
}

export function useUpdateAccountProfile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (patch: UpdateAccountProfileRequest) => accountService.updateProfile(patch),
    onSuccess: (profile) => {
      queryClient.setQueryData(accountKeys.profile(), profile);
      // The dashboard shell still reads the v1 /users/me summary.
      void queryClient.invalidateQueries({ queryKey: ["user-profile"] });
      const user = storageStore.getUser();
      if (user && user.username !== profile.username) storageStore.setUser({ ...user, username: profile.username });
    },
  });
}

/** `GET /api/v2/me/sessions` (Section 2, ACC-007). */
export function useAccountSessions() {
  return useQuery({ queryKey: accountKeys.sessions(), queryFn: () => accountService.listSessions(), retry: 1 });
}

export function useRevokeAccountSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (sessionId: string) => accountService.revokeSession(sessionId),
    onSettled: () => queryClient.invalidateQueries({ queryKey: accountKeys.sessions() }),
  });
}

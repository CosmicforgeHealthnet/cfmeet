import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { profilesService } from "@/services/v2/profiles.service";
import type { ProfilePolicyUpdateRequest } from "@/types/v2";
import { spaceKeys } from "./useSpaces";

export const profilePolicyKeys = {
  all: ["v2", "profile-policy"] as const,
  space: (spaceId: string) => [...profilePolicyKeys.all, spaceId] as const,
};

export function useProfilePolicy(spaceId: string, enabled = true) {
  return useQuery({
    queryKey: profilePolicyKeys.space(spaceId),
    queryFn: () => profilesService.getPolicy(spaceId),
    enabled: !!spaceId && enabled,
  });
}

export function useUpdateProfilePolicy(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: ProfilePolicyUpdateRequest) =>
      profilesService.updatePolicy(spaceId, data),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: profilePolicyKeys.space(spaceId) }),
        queryClient.invalidateQueries({ queryKey: spaceKeys.detail(spaceId) }),
      ]);
    },
  });
}

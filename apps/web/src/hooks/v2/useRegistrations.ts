import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { profilesService } from "@/services/v2/profiles.service";

export const registrationKeys = {
  all: ["v2", "registrations"] as const,
  list: (spaceId: string) => [...registrationKeys.all, "list", spaceId] as const,
  detail: (spaceId: string, registrationId: string) =>
    [...registrationKeys.all, "detail", spaceId, registrationId] as const,
};

export function useRegistrations(spaceId: string, enabled = true) {
  return useQuery({
    queryKey: registrationKeys.list(spaceId),
    queryFn: () => profilesService.listRegistrations(spaceId),
    enabled: !!spaceId && enabled,
  });
}

export function useRegistration(spaceId: string, registrationId: string) {
  return useQuery({
    queryKey: registrationKeys.detail(spaceId, registrationId),
    queryFn: () => profilesService.getRegistration(spaceId, registrationId),
    enabled: !!spaceId && !!registrationId,
  });
}

export function useCancelRegistration(spaceId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (registrationId: string) =>
      profilesService.cancelRegistration(spaceId, registrationId),
    onSuccess: async (_result, registrationId) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: registrationKeys.list(spaceId) }),
        queryClient.invalidateQueries({
          queryKey: registrationKeys.detail(spaceId, registrationId),
        }),
      ]);
    },
  });
}

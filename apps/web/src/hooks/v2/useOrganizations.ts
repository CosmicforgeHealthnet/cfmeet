import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { organizationsService } from "@/services/v2/organizations.service";
import type { UpsertOrganizationMemberRequest } from "@/types/v2";

export const organizationKeys = {
  all: ["v2", "organizations"] as const,
  mine: () => [...organizationKeys.all, "mine"] as const,
  members: (organizationId: string) =>
    [...organizationKeys.all, "members", organizationId] as const,
};

export function useMyOrganizations() {
  return useQuery({
    queryKey: organizationKeys.mine(),
    queryFn: () => organizationsService.mine(),
  });
}

export function useOrganizationMembers(organizationId: string, enabled: boolean) {
  return useQuery({
    queryKey: organizationKeys.members(organizationId),
    queryFn: () => organizationsService.listMembers(organizationId),
    enabled: !!organizationId && enabled,
  });
}

export function useUpsertOrganizationMember(organizationId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      userId,
      data,
    }: {
      userId: string;
      data: UpsertOrganizationMemberRequest;
    }) => organizationsService.upsertMember(organizationId, userId, data),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: organizationKeys.members(organizationId) }),
        queryClient.invalidateQueries({ queryKey: organizationKeys.mine() }),
      ]);
    },
  });
}

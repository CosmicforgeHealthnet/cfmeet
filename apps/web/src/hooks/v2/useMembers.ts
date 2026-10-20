/**
 * V2 Members Hooks
 * React Query hooks for meeting space membership
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { membersService } from "@/services/v2";
import type { AddMemberRequest, UpdateMemberRequest } from "@/types/v2";

// Query Keys
export const memberKeys = {
  all: ["v2", "members"] as const,
  lists: () => [...memberKeys.all, "list"] as const,
  list: (spaceId: string) => [...memberKeys.lists(), spaceId] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * List members of a meeting space
 */
export function useMembers(spaceId: string) {
  return useQuery({
    queryKey: memberKeys.list(spaceId),
    queryFn: () => membersService.list(spaceId),
    enabled: !!spaceId,
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Add a member to a meeting space
 */
export function useAddMember(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: AddMemberRequest) => membersService.add(spaceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: memberKeys.list(spaceId) });
    },
  });
}

/**
 * Update a member's role
 */
export function useUpdateMember(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      memberId,
      data,
    }: {
      memberId: string;
      data: UpdateMemberRequest;
    }) => membersService.update(spaceId, memberId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: memberKeys.list(spaceId) });
    },
  });
}

/**
 * Revoke a member's access
 */
export function useRevokeMember(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (memberId: string) => membersService.revoke(spaceId, memberId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: memberKeys.list(spaceId) });
    },
  });
}

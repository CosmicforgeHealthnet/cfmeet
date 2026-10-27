/**
 * V2 Invitations Hooks
 * React Query hooks for meeting invitations
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invitationsService } from "@/services/v2/invitations.service";
import type { CreateInvitationRequest } from "@/types/v2";

// Query Keys
export const invitationKeys = {
  all: ["v2", "invitations"] as const,
  lists: () => [...invitationKeys.all, "list"] as const,
  list: (spaceId: string) => [...invitationKeys.lists(), spaceId] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * List invitations for a meeting space
 */
export function useInvitations(spaceId: string) {
  return useQuery({
    queryKey: invitationKeys.list(spaceId),
    queryFn: () => invitationsService.list(spaceId),
    enabled: !!spaceId,
    refetchInterval: 30000,
  });
}

/**
 * Resolve an invitation token
 */
export function useResolveInvitation() {
  return useMutation({
    mutationFn: (token: string) => invitationsService.resolve(token),
    gcTime: 0,
    retry: false,
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Create an invitation
 */
export function useCreateInvitation(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    gcTime: 0,
    mutationFn: (data?: CreateInvitationRequest) =>
      invitationsService.create(spaceId, data),
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: invitationKeys.list(spaceId) });
    },
  });
}

/**
 * Revoke an invitation
 */
export function useRevokeInvitation(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (invitationId: string) =>
      invitationsService.revoke(spaceId, invitationId),
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: invitationKeys.list(spaceId) });
    },
  });
}

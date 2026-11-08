/**
 * React Query hooks for first-party organization administration (Section 17).
 *
 * Every per-organization query lives under `workspaceQueryKey(organizationId, ...)`, so the
 * WorkspaceProvider drops it when the membership disappears. The organization itself (and
 * therefore `viewer.capabilities`, which gates every control) is never served stale from a
 * previous visit: staleTime 0 and a refetch on focus.
 */
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { businessOrganizationsService as service } from "@/services/v2/business-organizations.service";
import { workspaceKeys, workspaceQueryKey } from "@/context/WorkspaceContext";
import { provisioningSettled } from "@/lib/organizations/organization-core";
import type {
  CreateBusinessOrganizationRequest,
  CreateOrganizationInvitationRequest,
  UpdateBusinessMembershipRequest,
  UpdateBusinessOrganizationRequest,
} from "@/types/generated/meeting-service";

export const businessOrgKeys = {
  detail: (id: string) => workspaceQueryKey(id, "organization"),
  provisioning: (id: string) => workspaceQueryKey(id, "organization", "provisioning"),
  members: (id: string) => workspaceQueryKey(id, "organization", "members"),
  invitations: (id: string, status: string) => workspaceQueryKey(id, "organization", "invitations", status),
  audit: (id: string) => workspaceQueryKey(id, "organization", "audit"),
};

const noRetryOn = (codes: number[]) => (count: number, error: unknown) => {
  const status = (error as { response?: { status?: number } })?.response?.status ?? 0;
  return count < 1 && !codes.includes(status);
};

export function useBusinessOrganization(id: string) {
  return useQuery({
    queryKey: businessOrgKeys.detail(id),
    queryFn: () => service.get(id),
    enabled: !!id,
    staleTime: 0,
    refetchOnWindowFocus: true,
    retry: noRetryOn([401, 403, 404]),
  });
}

export function useOrganizationProvisioning(id: string, enabled = true) {
  return useQuery({
    queryKey: businessOrgKeys.provisioning(id),
    queryFn: () => service.provisioning(id),
    enabled: !!id && enabled,
    retry: noRetryOn([401, 403, 404]),
    // Poll while a provisioning workflow is still running.
    refetchInterval: (query) => (query.state.data && !provisioningSettled(query.state.data.provisioningState) ? 5_000 : false),
  });
}

export function useBusinessOrganizationMembers(id: string, enabled: boolean) {
  return useQuery({
    queryKey: businessOrgKeys.members(id),
    queryFn: () => service.members(id),
    enabled: !!id && enabled,
    retry: noRetryOn([401, 403, 404]),
  });
}

export function useOrganizationInvitations(id: string, status: string, enabled: boolean) {
  return useQuery({
    queryKey: businessOrgKeys.invitations(id, status),
    queryFn: () => service.invitations(id, status || undefined),
    enabled: !!id && enabled,
    retry: noRetryOn([401, 403, 404]),
  });
}

export function useOrganizationAudit(id: string, enabled: boolean) {
  return useInfiniteQuery({
    queryKey: businessOrgKeys.audit(id),
    queryFn: ({ pageParam }) => service.auditEvents(id, pageParam),
    initialPageParam: null as number | null,
    getNextPageParam: (last) => (typeof last.nextCursor === "number" ? last.nextCursor : undefined),
    enabled: !!id && enabled,
    retry: noRetryOn([401, 403, 404]),
  });
}

/** Re-reads the organization and the caller's workspace list / capabilities after a change. */
function useRefreshOrganization(id: string) {
  const queryClient = useQueryClient();
  return async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: workspaceQueryKey(id) }),
      queryClient.invalidateQueries({ queryKey: workspaceKeys.list() }),
      queryClient.invalidateQueries({ queryKey: workspaceKeys.capabilities(id) }),
    ]);
  };
}

export function useCreateBusinessOrganization() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ body, idempotencyKey }: { body: CreateBusinessOrganizationRequest; idempotencyKey: string }) =>
      service.create(body, idempotencyKey),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: workspaceKeys.list() }),
  });
}

export function useUpdateBusinessOrganization(id: string) {
  const queryClient = useQueryClient();
  const refresh = useRefreshOrganization(id);
  return useMutation({
    mutationFn: (body: UpdateBusinessOrganizationRequest) => service.update(id, body),
    onSuccess: (org) => {
      queryClient.setQueryData(businessOrgKeys.detail(id), org);
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.list() });
    },
    // VERSION_CONFLICT and friends: reload the authoritative state.
    onError: () => void refresh(),
  });
}

export function useUpdateMembership(id: string) {
  const refresh = useRefreshOrganization(id);
  return useMutation({
    mutationFn: ({ membershipId, body }: { membershipId: string; body: UpdateBusinessMembershipRequest }) =>
      service.updateMembership(id, membershipId, body),
    onSettled: () => refresh(),
  });
}

export function useLeaveOrganization(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => service.leave(id),
    onSuccess: async () => {
      // The membership is gone: nothing of this organization stays cached.
      queryClient.removeQueries({ queryKey: workspaceQueryKey(id) });
      queryClient.removeQueries({ queryKey: workspaceKeys.capabilities(id) });
      await queryClient.invalidateQueries({ queryKey: workspaceKeys.list() });
    },
  });
}

export function useTransferOwnership(id: string) {
  const refresh = useRefreshOrganization(id);
  return useMutation({
    mutationFn: ({ toMembershipId, expectedVersion }: { toMembershipId: string; expectedVersion: number }) =>
      service.transferOwnership(id, { toMembershipId, expectedVersion }),
    onSettled: () => refresh(),
  });
}

export function useCreateInvitation(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ body, idempotencyKey }: { body: CreateOrganizationInvitationRequest; idempotencyKey: string }) =>
      service.invite(id, body, idempotencyKey),
    onSettled: () => queryClient.invalidateQueries({ queryKey: workspaceQueryKey(id, "organization", "invitations") }),
  });
}

export function useRevokeInvitation(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (invitationId: string) => service.revokeInvitation(id, invitationId),
    onSettled: () => queryClient.invalidateQueries({ queryKey: workspaceQueryKey(id, "organization", "invitations") }),
  });
}

export function useOrganizationLifecycle(id: string) {
  const refresh = useRefreshOrganization(id);
  return useMutation({
    mutationFn: ({ action, expectedVersion }: { action: "suspend" | "reactivate" | "archive"; expectedVersion: number }) =>
      service.lifecycle(id, action, expectedVersion),
    onSettled: () => refresh(),
  });
}

export function useInvitationDecision() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ token, decision }: { token: string; decision: "accept" | "decline" }) =>
      decision === "accept" ? service.acceptInvitation(token) : service.declineInvitation(token),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: workspaceKeys.list() }),
  });
}

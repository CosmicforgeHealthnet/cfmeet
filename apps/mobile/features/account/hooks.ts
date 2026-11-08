import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { accountApi } from "@/features/account/api";
import { organizationsApi } from "@/features/organizations/api";
import { WORKSPACES_KEY } from "@/features/workspace/workspace";
import type { UpdateAccountProfileRequest } from "@/lib/api/generated/meeting-service";

export const accountKeys = {
  profile: ["me", "profile"] as const,
  sessions: ["me", "sessions"] as const,
  organizations: ["me", "organizations"] as const,
  members: (orgId: string) => ["org", orgId, "members"] as const,
};

export function useProfile() {
  return useQuery({ queryKey: accountKeys.profile, queryFn: accountApi.profile });
}

export function useUpdateProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (data: UpdateAccountProfileRequest) => accountApi.updateProfile(data),
    onSuccess: (profile) => qc.setQueryData(accountKeys.profile, profile),
  });
}

export function useSessions() {
  return useQuery({ queryKey: accountKeys.sessions, queryFn: accountApi.sessions });
}

export function useRevokeSession() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (sessionId: string | "others") =>
      sessionId === "others" ? accountApi.revokeAllSessions() : accountApi.revokeSession(sessionId),
    onSettled: () => void qc.invalidateQueries({ queryKey: accountKeys.sessions }),
  });
}

export function useMyOrganizations() {
  return useQuery({ queryKey: accountKeys.organizations, queryFn: organizationsApi.myOrganizations });
}

export function useOrganizationMembers(orgId: string) {
  return useQuery({ queryKey: accountKeys.members(orgId), queryFn: () => organizationsApi.members(orgId), enabled: !!orgId });
}

function useRefreshMemberships() {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: accountKeys.organizations });
    void qc.invalidateQueries({ queryKey: WORKSPACES_KEY });
  };
}

export function useInvitationDecision() {
  const refresh = useRefreshMemberships();
  return useMutation({
    mutationFn: ({ token, accept }: { token: string; accept: boolean }) =>
      accept ? organizationsApi.acceptInvitation(token) : organizationsApi.declineInvitation(token),
    onSuccess: refresh,
  });
}

export function useLeaveOrganization() {
  const refresh = useRefreshMemberships();
  return useMutation({ mutationFn: (orgId: string) => organizationsApi.leave(orgId), onSuccess: refresh });
}

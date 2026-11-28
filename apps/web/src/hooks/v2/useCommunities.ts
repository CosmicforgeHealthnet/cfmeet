"use client";

import { useMemo } from "react";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { communitiesService as api } from "@/services/v2/communities.service";
import { communityConversationIds, type Community } from "@/lib/communities";

export const communityKeys = {
  all: ["v2", "communities"] as const,
  list: ["v2", "communities", "list"] as const,
  one: (id: string) => ["v2", "communities", "one", id] as const,
  groups: (id: string) => ["v2", "communities", "groups", id] as const,
  members: (id: string) => ["v2", "communities", "members", id] as const,
  bans: (id: string) => ["v2", "communities", "bans", id] as const,
  invitations: (id: string) => ["v2", "communities", "invitations", id] as const,
  links: (id: string) => ["v2", "communities", "links", id] as const,
  requests: (id: string) => ["v2", "communities", "requests", id] as const,
  myInvitations: ["v2", "communities", "my-invitations"] as const,
  myRequests: ["v2", "communities", "my-requests"] as const,
  preferences: ["v2", "communities", "preferences"] as const,
};

/** Every community of the user (all pages; a person is in a bounded number of them). */
export function useCommunities(enabled = true) {
  return useQuery({
    queryKey: communityKeys.list,
    enabled,
    retry: 1,
    refetchInterval: 60_000,
    queryFn: async () => {
      const out: Community[] = [];
      let cursor: string | null = null;
      for (let i = 0; i < 20; i++) {
        const page = await api.list(cursor, 100);
        out.push(...page.communities);
        cursor = page.nextCursor;
        if (!cursor) break;
      }
      return out;
    },
  });
}

export function useCommunity(id: string) {
  return useQuery({ queryKey: communityKeys.one(id), queryFn: () => api.get(id), enabled: !!id, retry: false });
}

export function useCommunityGroups(id: string, enabled = true) {
  return useQuery({ queryKey: communityKeys.groups(id), queryFn: () => api.groups(id).then((r) => r.groups), enabled: !!id && enabled, retry: false });
}

export function useCommunityMembers(id: string, enabled = true) {
  return useInfiniteQuery({
    queryKey: communityKeys.members(id),
    enabled: !!id && enabled,
    retry: false,
    initialPageParam: "",
    queryFn: ({ pageParam }) => api.members(id, pageParam || null, 100),
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
}

export function useCommunityBans(id: string, enabled: boolean) {
  return useQuery({ queryKey: communityKeys.bans(id), queryFn: () => api.bans(id).then((r) => r.bans), enabled: !!id && enabled, retry: false });
}

export function useCommunityInvitations(id: string, enabled: boolean) {
  return useQuery({ queryKey: communityKeys.invitations(id), queryFn: () => api.invitations(id).then((r) => r.invitations), enabled: !!id && enabled, retry: false });
}

export function useCommunityLinks(id: string, enabled: boolean) {
  return useQuery({ queryKey: communityKeys.links(id), queryFn: () => api.inviteLinks(id).then((r) => r.inviteLinks), enabled: !!id && enabled, retry: false });
}

export function useCommunityRequests(id: string, enabled: boolean) {
  return useQuery({ queryKey: communityKeys.requests(id), queryFn: () => api.joinRequests(id).then((r) => r.requests), enabled: !!id && enabled, retry: false, refetchInterval: 60_000 });
}

export function useMyCommunityInvitations() {
  return useQuery({ queryKey: communityKeys.myInvitations, queryFn: () => api.myInvitations().then((r) => r.invitations), retry: 1, refetchInterval: 60_000 });
}

export function useMyCommunityRequests() {
  return useQuery({ queryKey: communityKeys.myRequests, queryFn: () => api.myRequests().then((r) => r.requests), retry: 1 });
}

export function useCommunityPreferences(enabled = true) {
  return useQuery({ queryKey: communityKeys.preferences, queryFn: api.preferences, enabled, retry: 1, staleTime: 60_000 });
}

/** Refreshes everything Communities shows (after any change). */
export function useCommunitiesRefresh() {
  const qc = useQueryClient();
  return () => qc.invalidateQueries({ queryKey: communityKeys.all });
}

/**
 * The chat list's view of communities: the conversations of the user's communities and
 * whether they are shown separately (only under the Communities filter). A server without
 * Communities yet simply has none.
 */
export function useCommunityChats(enabled: boolean) {
  const communities = useCommunities(enabled);
  const preferences = useCommunityPreferences(enabled);
  const ids = useMemo(() => communityConversationIds(communities.data ?? []), [communities.data]);
  return { ids, separate: preferences.data?.chatListMode === "SEPARATE", communities: communities.data ?? [] };
}

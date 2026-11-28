"use client";

import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { channelsService, type DiscoverQuery } from "@/services/v2/channels.service";

/** Query keys of the channel screens (ADR 0072). */
export const channelKeys = {
  all: ["channels-v2"] as const,
  channel: (id: string) => ["channels-v2", "channel", id] as const,
  posts: (id: string) => ["channels-v2", "posts", id] as const,
  discover: (q: DiscoverQuery) => ["channels-v2", "discover", q] as const,
  following: () => ["channels-v2", "following"] as const,
  starred: () => ["channels-v2", "starred"] as const,
  scheduled: (id: string) => ["channels-v2", "scheduled", id] as const,
  admins: (id: string) => ["channels-v2", "admins", id] as const,
  invites: (id: string) => ["channels-v2", "invites", id] as const,
  verification: (id: string) => ["channels-v2", "verification", id] as const,
  moderation: (id: string) => ["channels-v2", "moderation", id] as const,
  audit: (id: string) => ["channels-v2", "audit", id] as const,
  analytics: (id: string, from: string, to: string) => ["channels-v2", "analytics", id, from, to] as const,
};

export function useChannelV2(id: string) {
  return useQuery({ queryKey: channelKeys.channel(id), queryFn: () => channelsService.get(id), retry: false });
}

export function useChannelPostsV2(id: string, enabled = true) {
  return useInfiniteQuery({
    queryKey: channelKeys.posts(id),
    enabled,
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) => (await channelsService.posts(id, pageParam)).posts,
    getNextPageParam: (last) => (last.length >= 30 ? last[last.length - 1].createdAt : undefined),
    retry: false,
    refetchInterval: 30_000,
  });
}

export function useDiscoverChannels(q: DiscoverQuery) {
  return useInfiniteQuery({
    queryKey: channelKeys.discover(q),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => channelsService.discover({ ...q, cursor: pageParam }),
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
}

export function useChannelSuggestions(q: string) {
  return useQuery({ queryKey: ["channels-v2", "suggest", q], queryFn: async () => (await channelsService.suggestions(q)).suggestions, staleTime: 30_000 });
}

export function useStarredChannelPosts() {
  return useInfiniteQuery({
    queryKey: channelKeys.starred(),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => channelsService.starred(pageParam),
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
}

export function useScheduledPosts(id: string, enabled: boolean) {
  return useQuery({ queryKey: channelKeys.scheduled(id), queryFn: async () => (await channelsService.scheduled(id)).posts, enabled });
}

export function useChannelAdmins(id: string, enabled: boolean) {
  return useQuery({ queryKey: channelKeys.admins(id), queryFn: () => channelsService.admins(id), enabled });
}

export function useChannelInvites(id: string, enabled: boolean) {
  return useQuery({ queryKey: channelKeys.invites(id), queryFn: async () => (await channelsService.invites(id)).invites, enabled });
}

export function useChannelVerification(id: string, enabled: boolean) {
  return useQuery({ queryKey: channelKeys.verification(id), queryFn: () => channelsService.verification(id), enabled });
}

export function useChannelModeration(id: string, enabled: boolean) {
  return useQuery({ queryKey: channelKeys.moderation(id), queryFn: () => channelsService.moderation(id), enabled });
}

export function useChannelAudit(id: string, enabled: boolean) {
  return useInfiniteQuery({
    queryKey: channelKeys.audit(id),
    enabled,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => channelsService.audit(id, pageParam),
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
}

export function useChannelAnalytics(id: string, from: string, to: string) {
  return useQuery({ queryKey: channelKeys.analytics(id, from, to), queryFn: () => channelsService.analytics(id, from, to) });
}

/** Refresh everything a channel change can touch (and the Updates list). */
export function useChannelsRefresh() {
  const qc = useQueryClient();
  return (id?: string) => Promise.all([
    qc.invalidateQueries({ queryKey: channelKeys.all }),
    qc.invalidateQueries({ queryKey: ["v2", "social"] }),
    id ? qc.invalidateQueries({ queryKey: channelKeys.channel(id) }) : Promise.resolve(),
  ]);
}

export function useFollowChannel() {
  const refresh = useChannelsRefresh();
  return useMutation({
    mutationFn: ({ id, on }: { id: string; on: boolean }) => channelsService.follow(id, on),
    onSuccess: (_, { id }) => void refresh(id),
  });
}

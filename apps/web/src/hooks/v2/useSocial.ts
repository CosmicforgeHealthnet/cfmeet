"use client";

import { useEffect, useState } from "react";
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { mediaUrl, socialService, type ChannelPost, type MediaView, type StatusRing } from "@/services/v2/social.service";
import { matchContacts, ringBatches } from "@/lib/status-privacy";
import { messagingService } from "@/services/v2/messaging.service";

export const socialKeys = {
  all: ["v2", "social"] as const,
  feed: ["v2", "social", "status-feed"] as const,
  viewers: (postId: string) => ["v2", "social", "viewers", postId] as const,
  following: ["v2", "social", "channels", "following"] as const,
  directory: (q: string) => ["v2", "social", "channels", "directory", q] as const,
  channel: (id: string) => ["v2", "social", "channels", "one", id] as const,
  posts: (id: string) => ["v2", "social", "channels", "posts", id] as const,
  mine: ["v2", "social", "status-mine"] as const,
  audience: ["v2", "social", "status-audience"] as const,
  eligible: ["v2", "social", "status-eligible"] as const,
  closeFriends: ["v2", "social", "status-close-friends"] as const,
  lists: ["v2", "social", "status-lists"] as const,
  mutes: ["v2", "social", "status-mutes"] as const,
  rings: (ids: string) => ["v2", "social", "status-rings", ids] as const,
  replies: (statusId: string) => ["v2", "social", "status-replies", statusId] as const,
};

/** The saved default audience and Allow sharing (server-side: the same on every device). */
export function useAudiencePreference() {
  return useQuery({ queryKey: socialKeys.audience, queryFn: socialService.getAudience, staleTime: 60_000, retry: 1 });
}

/** Mutual, unblocked contacts (the people a status can reach), named. */
export function useEligibleContacts(enabled = true) {
  return useQuery({ queryKey: socialKeys.eligible, queryFn: socialService.eligibleContacts, enabled, staleTime: 60_000, retry: 1 });
}

export function useCloseFriends(enabled = true) {
  return useQuery({ queryKey: socialKeys.closeFriends, queryFn: socialService.closeFriends, enabled, staleTime: 60_000, retry: 1 });
}

/** The caller's named (CUSTOM) lists; Close Friends has its own hook. */
export function useAudienceLists(enabled = true) {
  return useQuery({
    queryKey: socialKeys.lists,
    queryFn: async () => (await socialService.audienceLists()).filter((l) => l.kind === "CUSTOM"),
    enabled, staleTime: 60_000, retry: 1,
  });
}

export function useStatusMutes() {
  return useQuery({ queryKey: socialKeys.mutes, queryFn: socialService.mutes, staleTime: 60_000, retry: 1 });
}

/** "My status": live statuses with audience, unique viewers (times, reactions) and sharing. */
export function useMyStatuses() {
  return useQuery({ queryKey: socialKeys.mine, queryFn: socialService.myStatuses, refetchInterval: 60_000, retry: 1 });
}

/**
 * "My status" for the chat list's "Reacted to your status" previews (ADR 0074): the same
 * cached data as My status, read when Chats opens and on focus — no interval of its own, so
 * the chat list adds no polling (My status keeps its own refresh while it is open).
 */
export function useMyStatusesForChats(enabled = true) {
  return useQuery({ queryKey: socialKeys.mine, queryFn: socialService.myStatuses, enabled, staleTime: 60_000, refetchOnWindowFocus: true, retry: false });
}

/** The private replies to one of my live statuses (author only, ADR 0074). */
export function useStatusReplies(statusId: string | null) {
  return useQuery({ queryKey: socialKeys.replies(statusId ?? ""), queryFn: () => socialService.statusReplies(statusId!), enabled: !!statusId, staleTime: 30_000, retry: 1 });
}

/**
 * Status rings for the people on screen (only those whose statuses the caller may see come
 * back). One request per 200 people; refreshed with the rest of Updates (invalidation after
 * a view / publish / delete), on focus and at most every two minutes while visible: no
 * aggressive polling (ADR 0069: no realtime status push yet).
 */
export function useStatusRings(principalIds: (string | null | undefined)[]) {
  const batches = ringBatches(principalIds);
  const key = batches.map((b) => b.join(",")).join("|");
  const q = useQuery({
    queryKey: socialKeys.rings(key),
    queryFn: async () => (await Promise.all(batches.map((b) => socialService.rings(b)))).flat(),
    enabled: batches.length > 0,
    staleTime: 60_000,
    refetchInterval: 120_000,
    refetchOnWindowFocus: true,
    retry: false,
  });
  const map = new Map<string, StatusRing>();
  for (const r of q.data ?? []) map.set(r.principalId, r);
  return map;
}

export function useStatusFeed() {
  return useQuery({ queryKey: socialKeys.feed, queryFn: socialService.feed, refetchInterval: 60_000, retry: 1 });
}

export function useStatusViewers(postId: string | null) {
  return useQuery({ queryKey: socialKeys.viewers(postId ?? ""), queryFn: () => socialService.viewers(postId!), enabled: !!postId, retry: false });
}

export function useFollowedChannels() {
  return useQuery({ queryKey: socialKeys.following, queryFn: socialService.following, refetchInterval: 60_000, retry: 1 });
}

export function useChannelDirectory(q: string, enabled = true) {
  return useQuery({ queryKey: socialKeys.directory(q), queryFn: () => socialService.directory(q), enabled, retry: false });
}

export function useChannel(id: string) {
  return useQuery({ queryKey: socialKeys.channel(id), queryFn: () => socialService.channel(id), enabled: !!id, retry: false });
}

export function useChannelPosts(id: string) {
  return useInfiniteQuery({
    queryKey: socialKeys.posts(id),
    queryFn: ({ pageParam }) => socialService.posts(id, pageParam || undefined),
    initialPageParam: "",
    getNextPageParam: (last: ChannelPost[]) => (last.length === 30 ? last[last.length - 1].createdAt : undefined),
    enabled: !!id,
    refetchInterval: 30_000,
  });
}

/** Refresh everything Updates shows after a change. */
export function useSocialRefresh() {
  const client = useQueryClient();
  return () => client.invalidateQueries({ queryKey: socialKeys.all });
}

export function useFollow() {
  const refresh = useSocialRefresh();
  return useMutation({ mutationFn: ({ id, on }: { id: string; on: boolean }) => socialService.follow(id, on), onSettled: refresh });
}

/** An object URL for a media item (downloaded in pieces once per page). */
export function useMediaUrl(media: MediaView | null | undefined) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const mediaId = media?.mediaId;
  useEffect(() => {
    if (!media) return;
    let live = true;
    mediaUrl(media).then((u) => { if (live) { setUrl(u); setFailed(false); } }, () => { if (live) setFailed(true); });
    return () => { live = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mediaId]);
  return { url: media ? url : null, failed };
}

/**
 * Status rings for Communications Hub contacts (known by CF Meet ID, not by principal):
 * mutual contacts' principals are named through the names API and matched to contacts by
 * name and nickname only when the match is unique (display-only; ambiguity shows no ring).
 * Returns cfMeetId → ring.
 */
export function useContactStatusRings(contacts: { cfMeetId: string; displayName: string; alias: string | null }[], enabled = true) {
  const eligible = useQuery({
    queryKey: [...socialKeys.eligible, "named"] as const,
    queryFn: async () => {
      const ids = (await socialService.eligibleContacts()).map((p) => p.principalId ?? p.userId);
      const out: { principalId: string; displayName: string; alias?: string | null }[] = [];
      for (let i = 0; i < ids.length; i += 256) out.push(...(await messagingService.names(ids.slice(i, i + 256))).principals);
      return out;
    },
    enabled: enabled && contacts.length > 0,
    staleTime: 5 * 60_000,
    retry: false,
  });
  const byContact = matchContacts(contacts, eligible.data ?? []);
  const rings = useStatusRings([...byContact.values()]);
  const out = new Map<string, StatusRing>();
  for (const [cfMeetId, principal] of byContact) {
    const r = rings.get(principal);
    if (r) out.set(cfMeetId, r);
  }
  return out;
}

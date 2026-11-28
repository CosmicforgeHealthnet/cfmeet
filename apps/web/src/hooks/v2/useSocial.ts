"use client";

import { useEffect, useState } from "react";
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { mediaUrl, socialService, type ChannelPost, type MediaView } from "@/services/v2/social.service";

export const socialKeys = {
  all: ["v2", "social"] as const,
  feed: ["v2", "social", "status-feed"] as const,
  viewers: (postId: string) => ["v2", "social", "viewers", postId] as const,
  following: ["v2", "social", "channels", "following"] as const,
  directory: (q: string) => ["v2", "social", "channels", "directory", q] as const,
  channel: (id: string) => ["v2", "social", "channels", "one", id] as const,
  posts: (id: string) => ["v2", "social", "channels", "posts", id] as const,
};

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

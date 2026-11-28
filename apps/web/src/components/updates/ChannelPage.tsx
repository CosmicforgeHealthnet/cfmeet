"use client";

import { useEffect, useMemo, useRef } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { ArrowLeft, Lock, Pin, ShieldAlert } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { followersText } from "@/lib/channels";
import { channelsService } from "@/services/v2/channels.service";
import { useChannelPostsV2, useChannelsRefresh, useChannelV2, useFollowChannel } from "@/hooks/v2/useChannels";
import ChannelPostCard from "@/components/channels/ChannelPostCard";
import ChannelComposer from "@/components/channels/ChannelComposer";
import ChannelMenu from "@/components/channels/ChannelInfo";
import { ChannelAvatar, VerifiedBadge } from "@/components/channels/bits";
import { errorText } from "./shared";

const STATE_TEXT: Record<string, string> = {
  RESTRICTED: "CF Meet restricted this channel: it isn't shown in search and can't take new followers.",
  SUSPENDED: "CF Meet suspended this channel. Only its admins can see it for now.",
  BANNED: "CF Meet banned this channel.",
};

/**
 * One channel (WhatsApp): posts newest at the bottom with every kind of post, pinned posts,
 * polls, reactions, the menu (info, notifications, share, report, unfollow, admin tools)
 * and the composer for the owner and admins who can post.
 */
export default function ChannelPage({ channelId }: { channelId: string }) {
  const params = useSearchParams();
  const focusPost = params.get("post");
  const channel = useChannelV2(channelId);
  const c = channel.data;
  const readable = !!c && (c.moderationState === "ACTIVE" || c.moderationState === "RESTRICTED" || c.myRole === "OWNER" || (c.moderationState === "SUSPENDED" && c.myRole === "ADMIN"));
  const posts = useChannelPostsV2(channelId, readable);
  const follow = useFollowChannel();
  const refresh = useChannelsRefresh();
  const bottom = useRef<HTMLDivElement>(null);
  const seen = useRef(new Set<string>());
  const list = useMemo(() => [...(posts.data?.pages.flat() ?? [])].reverse(), [posts.data]);
  const latest = list.at(-1)?.postId;

  useEffect(() => {
    if (focusPost) document.getElementById(`post-${focusPost}`)?.scrollIntoView({ block: "center" });
    else bottom.current?.scrollIntoView({ block: "end" });
  }, [latest, focusPost]);

  // Views: one per person and post (the server dedupes and ignores the channel's admins).
  useEffect(() => {
    if (!c || c.myRole === "OWNER" || c.myRole === "ADMIN") return;
    const fresh = list.map((p) => p.postId).filter((id) => !seen.current.has(id));
    if (fresh.length === 0) return;
    fresh.forEach((id) => seen.current.add(id));
    void channelsService.views(channelId, fresh).catch(() => fresh.forEach((id) => seen.current.delete(id)));
  }, [list, c, channelId]);

  if (channel.isError) {
    return (
      <div className="mx-auto max-w-md px-6 py-16 text-center">
        <p className="text-lg font-semibold">This channel isn&apos;t available</p>
        <p className="mt-1 text-sm text-muted-foreground">It may be private, or it no longer exists.</p>
        <Link href="/dashboard/updates" className="mt-4 inline-block font-semibold text-primary">Back to Updates</Link>
      </div>
    );
  }

  const reload = () => { void channel.refetch(); void posts.refetch(); void refresh(channelId); };
  const pinned = c?.pinnedPosts ?? [];
  return (
    <div className="flex h-[calc(100dvh-4rem)] flex-col md:h-dvh" data-testid="channel-page">
      <header className="flex items-center gap-2 border-b border-border bg-background px-2 py-2">
        <Link href="/dashboard/updates" aria-label="Back" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        {c ? <ChannelAvatar name={c.name} avatar={c.avatar} /> : <span className="size-10 animate-pulse rounded-full bg-muted" />}
        <div className="min-w-0 flex-1">
          <p className="flex items-center gap-1 font-semibold"><span className="truncate">{c?.name ?? "Channel"}</span>{c?.verified ? <VerifiedBadge /> : null}{c?.visibility === "PRIVATE" ? <Lock aria-label="Private channel" className="size-3.5 text-muted-foreground" /> : null}</p>
          <p className="truncate text-xs text-muted-foreground">{c ? [c.handle ? `@${c.handle}` : null, followersText(c.followerCount)].filter(Boolean).join(" · ") : ""}</p>
        </div>
        {c && c.myRole === "NONE" && c.visibility === "PUBLIC" ? (
          <button type="button" disabled={follow.isPending || c.moderationState !== "ACTIVE"} onClick={() => follow.mutate({ id: c.channelId, on: true }, { onSuccess: reload, onError: (e) => toast(errorText(e), "error") })}
            className="h-9 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground disabled:opacity-60" data-testid="channel-follow">Follow</button>
        ) : null}
        {c ? <ChannelMenu channel={c} onChanged={reload} /> : null}
      </header>

      {c && c.moderationState !== "ACTIVE" ? (
        <p className="flex items-center gap-2 bg-amber-500/10 px-4 py-2 text-xs text-amber-700 dark:text-amber-300" data-testid="channel-moderation-banner"><ShieldAlert className="size-4 shrink-0" />{STATE_TEXT[c.moderationState]}</p>
      ) : null}
      {pinned.length ? (
        <button type="button" onClick={() => document.getElementById(`post-${pinned[0].postId}`)?.scrollIntoView({ block: "center" })}
          className="flex items-center gap-2 border-b border-border bg-background px-4 py-2 text-left text-sm" data-testid="channel-pinned">
          <Pin className="size-4 shrink-0 text-primary" />
          <span className="min-w-0 flex-1 truncate">{pinned[0].text ?? pinned[0].poll?.question ?? "Pinned post"}</span>
          {pinned.length > 1 ? <span className="text-xs text-muted-foreground">+{pinned.length - 1}</span> : null}
        </button>
      ) : null}

      <div className="chat-wallpaper min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <div className="mx-auto max-w-2xl space-y-3">
          {c?.description ? <p className="mx-auto max-w-md rounded-xl bg-background/90 px-4 py-2.5 text-center text-sm text-muted-foreground shadow-sm ring-1 ring-border">{c.description}</p> : null}
          {c && c.myRole === "NONE" && readable ? <p className="mx-auto max-w-md text-center text-xs text-muted-foreground">You&apos;re previewing this channel. Follow it to get its updates.</p> : null}
          {posts.hasNextPage ? (
            <button type="button" onClick={() => void posts.fetchNextPage()} className="mx-auto block h-8 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10">Load older posts</button>
          ) : null}
          {readable && posts.isPending ? [0, 1].map((i) => <div key={i} className="h-24 w-4/5 animate-pulse rounded-2xl bg-muted" />) : null}
          {readable && !posts.isPending && list.length === 0 ? <p className="py-12 text-center text-sm text-muted-foreground">{c?.permissions.post ? "Post the first update for your followers." : "No posts yet."}</p> : null}
          {c ? list.map((p) => (
            <div key={p.postId} className={cn(focusPost === p.postId && "rounded-2xl ring-2 ring-primary/50")}>
              <ChannelPostCard post={p} channel={c} onChanged={() => void posts.refetch()} onRemoved={reload} />
            </div>
          )) : null}
          <div ref={bottom} />
        </div>
      </div>

      {c?.permissions.post && (c.moderationState === "ACTIVE" || c.moderationState === "RESTRICTED") ? (
        <ChannelComposer channelId={c.channelId} onPosted={(scheduled) => { if (!scheduled) void posts.refetch(); void refresh(c.channelId); }} />
      ) : null}
    </div>
  );
}

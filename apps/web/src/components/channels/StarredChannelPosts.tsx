"use client";

import Link from "next/link";
import { ArrowLeft } from "lucide-react";
import { useStarredChannelPosts } from "@/hooks/v2/useChannels";
import { mediaView } from "@/services/v2/channels.service";
import { useMediaUrl } from "@/hooks/v2/useSocial";
import type { Starred } from "@/services/v2/channels.service";
import { shortWhen } from "@/components/updates/shared";
import { VerifiedBadge } from "./bits";

function Thumb({ item }: { item: Starred }) {
  const m = item.post.media;
  const { url } = useMediaUrl(m && m.kind === "IMAGE" ? mediaView(m) : null);
  if (!url) return null;
  // eslint-disable-next-line @next/next/no-img-element
  return <img src={url} alt="" className="size-14 shrink-0 rounded-lg object-cover" />;
}

const label: Record<string, string> = { IMAGE: "Photo", VIDEO: "Video", AUDIO: "Audio", VOICE: "Voice message", DOCUMENT: "Document", GIF: "GIF", STICKER: "Sticker", POLL: "Poll" };

/** The caller's starred channel posts (their own list), newest star first; tap to open the post. */
export default function StarredChannelPosts() {
  const starred = useStarredChannelPosts();
  const items = starred.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <div className="min-h-dvh" data-testid="starred-channel-posts">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border bg-background px-2 py-2">
        <Link href="/dashboard/updates" aria-label="Back" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        <h1 className="text-lg font-semibold">Starred channel posts</h1>
      </header>
      <div className="mx-auto max-w-2xl divide-y divide-border">
        {starred.isPending ? [0, 1].map((i) => <div key={i} className="mx-4 my-3 h-16 animate-pulse rounded-2xl bg-muted" />) : null}
        {!starred.isPending && items.length === 0 ? <p className="px-6 py-12 text-center text-sm text-muted-foreground">Star a channel post to find it here.</p> : null}
        {items.map((i) => (
          <Link key={i.post.postId} href={`/dashboard/updates/channels/${i.channel.channelId}?post=${i.post.postId}`} className="flex items-center gap-3 px-4 py-3 hover:bg-muted/50">
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-1 text-xs font-semibold text-primary">{i.channel.name}{i.channel.verified ? <VerifiedBadge className="size-3.5" /> : null}</span>
              <span className="line-clamp-2 block text-sm">{i.post.text ?? i.post.poll?.question ?? label[i.post.kind] ?? "Post"}</span>
              <span className="block text-xs text-muted-foreground">{shortWhen(i.post.createdAt)}</span>
            </span>
            <Thumb item={i} />
          </Link>
        ))}
      </div>
      {starred.hasNextPage ? <div className="py-4 text-center"><button type="button" onClick={() => void starred.fetchNextPage()} className="h-9 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10">Show more</button></div> : null}
    </div>
  );
}

"use client";

import StarredContent, { type ChannelStars } from "./StarredContent";
import { channelsService } from "@/services/v2/channels.service";
import type { ChannelStar } from "@/lib/starred";

/** At most this many pages of channel stars are read (the store pages them newest first). */
const MAX_CHANNEL_PAGES = 20;

/**
 * Channel posts as a Starred source (ADR 0072's per-person channel star store): only posts the
 * caller may still read are returned by the server; unstarring goes back to the same store.
 */
export const channelStars: ChannelStars = {
  load: async () => {
    const out: ChannelStar[] = [];
    let cursor: string | undefined;
    for (let page = 0; page < MAX_CHANNEL_PAGES; page++) {
      const r = await channelsService.starred(cursor);
      for (const s of r.items) {
        out.push({ starredAt: s.starredAt, channel: { channelId: s.channel.channelId, name: s.channel.name }, post: { postId: s.post.postId, text: s.post.text ?? null, kind: s.post.kind } });
      }
      if (!r.nextCursor) break;
      cursor = r.nextCursor;
    }
    return out;
  },
  unstar: (channelId, postId) => channelsService.star(channelId, postId, false),
};

/** Starred with every source: chat and group messages and channel posts. */
export default function StarredWithChannels() {
  return <StarredContent channelStars={channelStars} />;
}

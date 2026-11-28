"use client";

/**
 * Status rings round avatars (WhatsApp): segmented by status count, the brand colour while
 * something is unseen, neutral once everything is seen, the Close friends colour when a
 * status was shared with the viewer as a Close friend, and no ring at all when nothing is
 * visible to the viewer (the rings API returns only what the server's gate allows).
 *
 * `StatusViewerHost` (mounted once in the dashboard) opens a person's statuses from any
 * screen: `useOpenStatus()(principalId)`.
 */
import { createContext, useCallback, useContext, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { socialKeys, useSocialRefresh } from "@/hooks/v2/useSocial";
import { useAccountProfile } from "@/hooks/useAccount";
import { socialService, type PersonView, type StatusFeed, type StatusPost, type StatusRing } from "@/services/v2/social.service";
import { ringArcs, ringLook, type RingLike } from "@/lib/status-privacy";
import StatusViewer from "./StatusViewer";

function arcPath(cx: number, cy: number, r: number, from: number, to: number): string {
  const pt = (deg: number) => {
    const rad = ((deg - 90) * Math.PI) / 180;
    return [cx + r * Math.cos(rad), cy + r * Math.sin(rad)];
  };
  if (to - from >= 359.9) return `M ${cx} ${cy - r} A ${r} ${r} 0 1 1 ${cx - 0.01} ${cy - r}`;
  const [x1, y1] = pt(from);
  const [x2, y2] = pt(to);
  return `M ${x1} ${y1} A ${r} ${r} 0 ${to - from > 180 ? 1 : 0} 1 ${x2} ${y2}`;
}

/** The avatar `children` inside the person's status ring (or untouched when there is none). */
export function StatusRingFrame({ ring, children, className }: { ring: RingLike | null | undefined; children: React.ReactNode; className?: string }) {
  const look = ringLook(ring);
  if (look.kind === "none") return <span className={cn("inline-flex shrink-0", className)}>{children}</span>;
  const arcs = ringArcs(look.segments, look.seenSegments);
  const unseenStroke = look.closeFriends ? "stroke-emerald-500" : "stroke-primary";
  return (
    <span className={cn("relative inline-flex shrink-0 rounded-full p-[3px]", className)} data-testid="status-ring"
      data-ring={look.unseen ? "unseen" : "seen"} data-close-friends={look.closeFriends || undefined} data-segments={look.segments}>
      <svg viewBox="0 0 100 100" aria-hidden className="pointer-events-none absolute inset-0 size-full">
        {arcs.map((a, i) => (
          <path key={i} d={arcPath(50, 50, 47, a.from, a.to)} fill="none" strokeWidth={5} strokeLinecap="round"
            className={a.seen ? "stroke-muted-foreground/35" : unseenStroke} />
        ))}
      </svg>
      <span className="relative flex rounded-full bg-background p-[2px]">{children}</span>
    </span>
  );
}

/** A ring from a feed author's posts (Updates page: the same look as everywhere else). */
export function ringOfPosts(principalId: string, posts: StatusPost[], closeFriends = false): StatusRing {
  return { principalId, hasActiveStatus: posts.length > 0, count: posts.length, unseenCount: posts.filter((p) => !p.viewed).length, closeFriends };
}

type Open = (principalId: string, fallbackName?: string) => void;
const OpenContext = createContext<Open | null>(null);

/** Opens the status viewer for a person (null outside the dashboard). */
export function useOpenStatus(): Open | null {
  return useContext(OpenContext);
}

/** Mounted once in the dashboard: hosts the status viewer opened from chat list, contacts and profiles. */
export function StatusViewerHost({ children }: { children: React.ReactNode }) {
  const client = useQueryClient();
  const profile = useAccountProfile();
  const me: PersonView = { userId: "me", name: profile.data?.effectiveDisplayName || "You", avatarUrl: profile.data?.avatarUrl ?? null, principalId: null };
  const refresh = useSocialRefresh();
  const [viewing, setViewing] = useState<{ person: PersonView; posts: StatusPost[]; mine: boolean; muted: boolean; startAt: number } | null>(null);
  const open = useCallback<Open>((principalId, fallbackName) => {
    void (async () => {
      try {
        const feed = await client.fetchQuery<StatusFeed>({ queryKey: socialKeys.feed, queryFn: socialService.feed, staleTime: 15_000 });
        const author = feed.contacts.find((a) => a.person.principalId === principalId);
        const quiet = feed.muted.find((a) => a.person.principalId === principalId);
        const hit = author ?? quiet;
        if (hit && hit.posts.length) {
          setViewing({ person: hit.person.name === "Contact" && fallbackName ? { ...hit.person, name: fallbackName } : hit.person, posts: hit.posts, mine: false, muted: !!quiet && !author, startAt: Math.max(0, hit.posts.findIndex((p) => !p.viewed)) });
        } else if (feed.mine.length && feed.mine[0].authorPrincipalId === principalId) {
          setViewing({ person: me, posts: feed.mine, mine: true, muted: false, startAt: 0 });
        } else {
          toast("No status update to show", "error");
          void refresh();
        }
      } catch {
        toast("Status updates couldn't load.", "error");
      }
    })();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, refresh, me.name, me.avatarUrl]);
  return (
    <OpenContext.Provider value={open}>
      {children}
      {viewing ? <StatusViewer {...viewing} onClose={() => { setViewing(null); void refresh(); }} onChanged={() => void client.invalidateQueries({ queryKey: socialKeys.feed })} /> : null}
    </OpenContext.Provider>
  );
}

"use client";

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Bell, CheckCheck, X } from "lucide-react";
import { Popover } from "radix-ui";
import { notificationsService } from "@/services/v2/notifications.service";
import type { NotificationItem } from "@/types/generated/notifications";
import { inboxHref, mergeInboxPages } from "@/lib/notifications/inbox";
import { cn } from "@/lib/utils";
import { getErrorMessage } from "@/utils/error";

const KEY = ["notifications"] as const;
const COUNT_KEY = ["notifications", "unread-count"] as const;

const ago = (iso: string) => {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return new Date(iso).toLocaleDateString([], { day: "numeric", month: "short" });
};

/** The bell (Gmail / Teams): unread count, every notification of the account (all
 *  categories, newest first, "Load more" pages through the whole inbox), mark read. */
export default function NotificationBell() {
  const router = useRouter();
  const client = useQueryClient();
  const [isOpen, setOpen] = useState(false);
  const count = useQuery({ queryKey: COUNT_KEY, queryFn: notificationsService.unreadCount, refetchInterval: 30_000, retry: false });
  const list = useInfiniteQuery({
    queryKey: KEY,
    queryFn: ({ pageParam }) => notificationsService.list(pageParam),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    enabled: isOpen,
    retry: false,
  });
  const refresh = () => { void client.invalidateQueries({ queryKey: KEY }); void client.invalidateQueries({ queryKey: COUNT_KEY }); };
  const readAll = useMutation({ mutationFn: () => notificationsService.markRead(), onSuccess: refresh });
  const dismiss = useMutation({ mutationFn: (id: string) => notificationsService.dismiss([id]), onSuccess: refresh });
  const unread = count.data?.unreadCount ?? 0;
  const items = mergeInboxPages(list.data?.pages);
  const { refetch } = list;
  // A new notification while the list is open: show it (the count is polled).
  const seenUnread = useRef(unread);
  useEffect(() => {
    if (isOpen && unread > seenUnread.current) void refetch();
    seenUnread.current = unread;
  }, [isOpen, unread, refetch]);

  const open = (n: NotificationItem) => {
    if (!n.readAt) void notificationsService.update(n, { read: true }).then(refresh).catch(() => undefined);
    const href = inboxHref(n);
    if (href) router.push(href);
  };

  return (
    <Popover.Root onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button type="button" aria-label={unread ? `Notifications, ${unread} unread` : "Notifications"} title="Notifications"
          className="relative flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground">
          <Bell className="size-5" />
          {unread ? <span className="absolute right-1 top-1 flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-destructive px-1 text-[10px] font-bold text-white">{unread > 99 ? "99+" : unread}</span> : null}
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content align="end" sideOffset={8} collisionPadding={12} className="z-50 flex max-h-[70vh] w-[min(24rem,calc(100vw-1.5rem))] flex-col overflow-hidden rounded-2xl border border-border bg-popover shadow-2xl">
          <div className="flex items-center justify-between border-b border-border px-4 py-3">
            <p className="font-semibold">Notifications</p>
            {unread ? <button type="button" onClick={() => readAll.mutate()} disabled={readAll.isPending} className="inline-flex items-center gap-1.5 rounded-lg px-2 py-1 text-xs font-semibold text-primary hover:bg-primary/10"><CheckCheck className="size-4" />Mark all read</button> : null}
          </div>
          <ul className="no-scrollbar min-h-0 flex-1 overflow-y-auto">
            {list.isFetching && !list.data ? [0, 1, 2].map((i) => <li key={i} className="mx-4 my-3 h-12 animate-pulse rounded-xl bg-muted" />) : null}
            {list.isError ? <li role="alert" className="px-6 py-10 text-center text-sm text-muted-foreground">Notifications couldn&apos;t load ({getErrorMessage(list.error)}). <button type="button" className="font-semibold text-primary underline" onClick={() => void list.refetch()}>Try again</button></li> : null}
            {list.data && items.length === 0 ? <li className="px-6 py-10 text-center text-sm text-muted-foreground">You&apos;re all caught up.</li> : null}
            {items.map((n) => (
              <li key={n.id} className={cn("group relative border-b border-border last:border-b-0", !n.readAt && "bg-primary/5")}>
                <button type="button" onClick={() => open(n)} className="flex w-full items-start gap-3 px-4 py-3 pr-10 text-left hover:bg-muted/60">
                  <span className={cn("mt-1.5 size-2 shrink-0 rounded-full", n.readAt ? "bg-transparent" : "bg-primary")} />
                  <span className="min-w-0 flex-1">
                    <span className="flex items-baseline justify-between gap-2">
                      <span className="truncate text-sm font-semibold">{n.title}{n.groupCount > 1 ? ` (${n.groupCount})` : ""}</span>
                      <span className="shrink-0 text-xs text-muted-foreground">{ago(n.createdAt)}</span>
                    </span>
                    <span className="mt-0.5 line-clamp-2 block text-sm text-muted-foreground">{n.body}</span>
                  </span>
                </button>
                <button type="button" aria-label="Dismiss" title="Dismiss" onClick={() => dismiss.mutate(n.id)} className="absolute right-2 top-2.5 flex size-7 items-center justify-center rounded-full text-muted-foreground opacity-0 hover:bg-muted group-hover:opacity-100 focus:opacity-100 max-md:opacity-100"><X className="size-4" /></button>
              </li>
            ))}
            {list.hasNextPage ? (
              <li className="px-4 py-3 text-center">
                <button type="button" onClick={() => void list.fetchNextPage()} disabled={list.isFetchingNextPage} className="text-sm font-semibold text-primary hover:underline disabled:opacity-60">
                  {list.isFetchingNextPage ? "Loading…" : "Load more"}
                </button>
              </li>
            ) : null}
          </ul>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

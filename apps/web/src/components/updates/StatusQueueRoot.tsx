"use client";

import { useEffect, useSyncExternalStore } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "@/components/ui/toast";
import { socialKeys } from "@/hooks/v2/useSocial";
import { cancelStatus, enqueueStatus, getStatusQueue, retryStatus, startStatusQueue } from "@/lib/status-publisher/client";
import type { JobView, QueueSnapshot } from "@/lib/status-publisher/types";

const SERVER_SNAPSHOT: QueueSnapshot = { jobs: [], online: true, persistent: false };
const noopSubscribe = () => () => {};

/** The page's status queue, live: pending / failed / just-published jobs and actions. */
export function useStatusQueue() {
  const q = typeof window === "undefined" ? null : getStatusQueue();
  const snapshot = useSyncExternalStore(q ? q.subscribe : noopSubscribe, q ? q.getSnapshot : () => SERVER_SNAPSHOT, () => SERVER_SNAPSHOT);
  return { snapshot, enqueue: enqueueStatus, retry: retryStatus, cancel: cancelStatus };
}

const onUpdates = () => typeof window !== "undefined" && window.location.pathname.startsWith("/dashboard/updates");

/**
 * Mounted once in the dashboard layout: starts the background status queue for the signed-in
 * account (it resumes whatever a previous visit left pending) and, as statuses are published,
 * refreshes the status feed so "My status" shows the real posts without a reload. Renders nothing.
 * The queue itself outlives this component (routes outside the dashboard keep it running).
 */
export function StatusQueueRoot({ ownerId }: { ownerId: string }) {
  const client = useQueryClient();

  useEffect(() => { startStatusQueue(ownerId); }, [ownerId]);

  useEffect(() => {
    const q = getStatusQueue();
    const settle = (job: JobView) => {
      // The card keeps showing the job until the feed has its statuses: no flicker.
      void client.invalidateQueries({ queryKey: socialKeys.feed }).finally(() => q.acknowledge(job.id));
    };
    // Published while no dashboard page was mounted.
    for (const job of q.getSnapshot().jobs) if (job.state === "done") settle(job);
    return q.onEvent((e) => {
      if (e.type === "published") {
        settle(e.job);
        if (!onUpdates()) toast(e.job.postCount > 1 ? `${e.job.postCount} status updates added` : "Status added");
      } else if (!onUpdates()) {
        // On Updates the card itself says "Failed to upload · Tap to retry".
        toast("Your status couldn't be added. Open Updates to retry.", "error");
      }
    });
  }, [client]);

  return null;
}

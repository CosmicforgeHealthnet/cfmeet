"use client";

import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { UsersRound } from "lucide-react";
import { MessagingEngine } from "@/lib/messaging/engine";
import { useSharedMessagingEngine } from "@/components/messaging/MessagingEngineProvider";
import { useGroupPhoto } from "@/components/messaging/ChatListRow";
import { messagingService } from "@/services/v2/messaging.service";
import { cn } from "@/lib/utils";

/** The app-wide messaging engine (a page-local one only where no provider exists). */
export function useCommunityEngine(): MessagingEngine {
  const shared = useSharedMessagingEngine();
  const [own] = useState(() => (shared ? null : new MessagingEngine()));
  useEffect(() => {
    if (!own) return;
    void own.start();
    return () => own.stop();
  }, [own]);
  return shared ?? own!;
}

export function useEngineSnapshot(engine: MessagingEngine) {
  return useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
}

/** Display names of principals: what the chat engine knows, the rest asked once. */
export function useNames(engine: MessagingEngine, principalIds: readonly string[]): (id: string) => string {
  const s = useEngineSnapshot(engine);
  const missing = useMemo(() => [...new Set(principalIds)].filter((p) => !s.names[p]).sort().slice(0, 256), [principalIds, s.names]);
  const asked = useQuery({
    queryKey: ["v2", "communities", "names", missing.join(",")],
    queryFn: () => messagingService.names(missing),
    enabled: missing.length > 0,
    retry: false,
    staleTime: 10 * 60_000,
  });
  const me = s.identity?.principalId;
  return (id: string) => {
    if (id === me) return "You";
    if (s.names[id]) return s.names[id];
    const found = asked.data?.principals.find((p) => p.principalId === id);
    return found ? found.alias?.trim() || found.displayName : "CF Meet user";
  };
}

/** The community picture (the Announcements group's photo), or its initial. */
export function CommunityAvatar({ engine, conversationId, pictureRef, name, size = "md" }: {
  engine: MessagingEngine; conversationId: string | null; pictureRef: string | null; name: string; size?: "sm" | "md" | "lg";
}) {
  const photo = useGroupPhoto(engine, conversationId ?? "", conversationId ? pictureRef : null);
  const box = size === "lg" ? "size-20 rounded-3xl text-2xl" : size === "sm" ? "size-10 rounded-xl text-sm" : "size-12 rounded-2xl text-base";
  return (
    <span className={cn("flex shrink-0 items-center justify-center overflow-hidden bg-primary/10 font-semibold text-primary", box)} aria-hidden>
      {/* eslint-disable-next-line @next/next/no-img-element -- a blob: URL of a group photo */}
      {photo ? <img src={photo} alt="" className="size-full object-cover" /> : name.trim() ? name.trim()[0]!.toLocaleUpperCase() : <UsersRound className="size-5" />}
    </span>
  );
}

export function Section({ title, action, children }: { title: string; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="space-y-2">
      <div className="flex items-center justify-between gap-2 px-1">
        <h2 className="text-sm font-semibold text-muted-foreground">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

export function ErrorNote({ error }: { error: string | null }) {
  return error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null;
}

export function Empty({ children }: { children: React.ReactNode }) {
  return <p className="rounded-2xl border border-dashed border-border px-4 py-6 text-center text-sm text-muted-foreground">{children}</p>;
}

export const field = "w-full rounded-xl border border-border bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring";
export const chip = "inline-flex h-8 items-center gap-1.5 rounded-full px-3 text-sm font-semibold ring-1 ring-border hover:bg-muted disabled:opacity-50";

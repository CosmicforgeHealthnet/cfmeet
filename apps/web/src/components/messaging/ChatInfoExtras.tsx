"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { BellOff, FileText, Film, Image as ImageIcon, Mic, Users } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { communicationsService as hub, type ChatPreferences } from "@/services/v2/communications.service";

const MUTE_OPTIONS = [
  { value: "ALL", label: "All messages" },
  { value: "MENTIONS", label: "Only @mentions" },
  { value: "MUTED_8H", label: "Mute for 8 hours" },
  { value: "MUTED_1W", label: "Mute for 1 week" },
  { value: "MUTED", label: "Mute always" },
];
const CLASS_ICON: Record<string, typeof FileText> = { IMAGE: ImageIcon, VIDEO: Film, VOICE_NOTE: Mic };
const CLASS_LABEL: Record<string, [string, string]> = { IMAGE: ["photo", "photos"], VIDEO: ["video", "videos"], VOICE_NOTE: ["voice note", "voice notes"], DOCUMENT: ["document", "documents"], FILE: ["file", "files"] };
const size = (n: number | null) => n == null ? "" : n < 1024 * 1024 ? `${Math.max(1, Math.round(n / 1024))} KB` : `${(n / 1024 / 1024).toFixed(1)} MB`;

/**
 * Chat info extras (WhatsApp): notifications for this chat, its media and files, and groups
 * in common for a one-to-one chat. Each part hides itself when the server doesn't offer it.
 */
export default function ChatInfoExtras({ conversationId, direct, groupTitle }: { conversationId: string; direct: boolean; groupTitle: (id: string) => string | null }) {
  const client = useQueryClient();
  const prefsKey = ["chat-info", conversationId, "prefs"];
  const prefs = useQuery({ queryKey: prefsKey, queryFn: () => hub.conversationPreferences(conversationId), retry: false });
  const media = useQuery({ queryKey: ["chat-info", conversationId, "media"], queryFn: () => hub.conversationMedia(conversationId), retry: false });
  const shared = useQuery({ queryKey: ["chat-info", conversationId, "groups"], queryFn: () => hub.sharedGroups(conversationId), enabled: direct, retry: false });
  const save = useMutation({
    mutationFn: (choice: string) => {
      const p = prefs.data as ChatPreferences;
      const hours = choice === "MUTED_8H" ? 8 : choice === "MUTED_1W" ? 24 * 7 : 0;
      return hub.setConversationPreferences(conversationId, {
        expectedVersion: p.version,
        notifications: choice.startsWith("MUTED") ? "MUTED" : (choice as "ALL" | "MENTIONS"),
        mutedUntil: hours ? new Date(Date.now() + hours * 3600_000).toISOString() : null,
        mediaAutoDownload: p.mediaAutoDownload,
      });
    },
    onSuccess: (next) => client.setQueryData(prefsKey, next),
  });

  const p = prefs.data;
  const current = !p ? "ALL" : p.effectiveNotifications === "MUTED" && !p.mutedUntil ? "MUTED" : p.effectiveNotifications;
  const items = media.data?.data ?? [];
  const counts = items.reduce<Record<string, number>>((acc, m) => ({ ...acc, [m.mediaClass]: (acc[m.mediaClass] ?? 0) + 1 }), {});
  const groups = (shared.data?.groups ?? []).map((g) => groupTitle(g.conversationId)).filter((t): t is string => !!t);

  return (
    <>
      {p ? (
        <section>
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Notifications</p>
          <NiceSelect value={current === "MUTED" && p.mutedUntil ? "MUTED" : current} onChange={(v) => save.mutate(v)} options={MUTE_OPTIONS} />
          {p.effectiveNotifications === "MUTED" && p.mutedUntil ? (
            <p className="mt-1.5 flex items-center gap-1.5 text-xs text-muted-foreground"><BellOff className="size-3.5" />Muted until {new Date(p.mutedUntil).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" })}</p>
          ) : null}
          {save.isError ? <p className="mt-1.5 text-xs text-destructive">That didn&apos;t save. Try again.</p> : null}
        </section>
      ) : null}

      {media.data ? (
        <section>
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Media and files</p>
          {items.length === 0 ? <p className="text-sm text-muted-foreground">Photos, videos and files shared here show up in this list.</p> : (
            <>
              <p className="mb-2 text-sm text-muted-foreground">{Object.entries(counts).map(([k, n]) => `${n} ${(CLASS_LABEL[k] ?? ["file", "files"])[n === 1 ? 0 : 1]}`).join(" · ")}</p>
              <ul className="space-y-1">
                {items.slice(0, 12).map((m) => {
                  const Icon = CLASS_ICON[m.mediaClass] ?? FileText;
                  return (
                    <li key={m.attachmentId} className="flex items-center gap-3 rounded-xl px-1 py-1.5">
                      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><Icon className="size-4" /></span>
                      <span className="min-w-0 flex-1 text-sm">
                        <span className="block truncate">{(CLASS_LABEL[m.mediaClass] ?? ["File"])[0].replace(/^./, (c) => c.toUpperCase())}</span>
                        <span className="block text-xs text-muted-foreground">{[size(m.sizeBytes), new Date(m.createdAt).toLocaleDateString([], { day: "numeric", month: "short" })].filter(Boolean).join(" · ")}</span>
                      </span>
                    </li>
                  );
                })}
              </ul>
              {items.length > 12 ? <p className="mt-1 text-xs text-muted-foreground">and {items.length - 12} more</p> : null}
            </>
          )}
        </section>
      ) : null}

      {direct && shared.data && shared.data.available !== false ? (
        <section>
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Groups in common</p>
          {groups.length === 0 ? <p className="text-sm text-muted-foreground">No groups in common.</p> : (
            <ul className="space-y-1">
              {groups.map((t, i) => (
                <li key={i} className="flex items-center gap-3 rounded-xl px-1 py-1.5">
                  <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"><Users className="size-4" /></span>
                  <span className="truncate text-sm font-medium">{t}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      ) : null}
    </>
  );
}

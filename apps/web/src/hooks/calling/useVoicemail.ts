"use client";

/**
 * React Query hooks for the signed-in user's own voicemail (CAL-006, owner view).
 */

import { useQuery, useMutation, useQueryClient, useInfiniteQuery } from "@tanstack/react-query";
import { voicemailService } from "@/services/calling";
import type { ReadStateRequest, VoicemailMessageFilters } from "@/types/calling";

export const voicemailKeys = {
  all: ["calling", "voicemail"] as const,
  mailboxes: () => [...voicemailKeys.all, "mailboxes"] as const,
  messages: (mailboxId: string, readState?: string) => [...voicemailKeys.all, "messages", mailboxId, readState ?? "ALL"] as const,
};

export function useVoicemailMailboxes() {
  return useQuery({ queryKey: voicemailKeys.mailboxes(), queryFn: () => voicemailService.listMailboxes(), staleTime: 60_000, retry: false });
}

export function useVoicemailMessages(filters: VoicemailMessageFilters) {
  return useInfiniteQuery({
    queryKey: voicemailKeys.messages(filters.mailboxId, filters.readState),
    queryFn: ({ pageParam }) => voicemailService.listMessages({ ...filters, cursor: pageParam }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    enabled: !!filters.mailboxId,
    refetchInterval: 30_000,
    retry: false,
  });
}

function useMessageMutation<T>(mutationFn: (input: T) => Promise<unknown>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    retry: false,
    onSettled: () => void queryClient.invalidateQueries({ queryKey: [...voicemailKeys.all, "messages"] }),
  });
}

export function useDeleteVoicemailMessage() {
  return useMessageMutation(({ messageId, expectedVersion }: { messageId: string; expectedVersion: number }) =>
    voicemailService.deleteMessage(messageId, expectedVersion));
}

export function useSetReadState() {
  return useMessageMutation(({ messageId, body }: { messageId: string; body: ReadStateRequest }) =>
    voicemailService.setReadState(messageId, body));
}

/** A fresh short-lived URL per playback; never cached. */
export function useVoicemailMediaAccess() {
  return useMutation({ mutationFn: (messageId: string) => voicemailService.getMediaAccess(messageId), retry: false });
}

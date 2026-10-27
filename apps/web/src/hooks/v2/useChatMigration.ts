"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { chatMigrationService } from "@/services/v2/chat-migration.service";
import type { ChatMigrationTransitionRequest } from "@/types/messaging";

export const chatMigrationKey = ["v2", "chat-migration"] as const;
/** Opt-in only: callers must be on a platform-admin surface. Backend checks role. */
export function useChatMigration(enabled = false) {
  return useQuery({ queryKey: chatMigrationKey, queryFn: chatMigrationService.status, enabled, retry: false });
}

export function useChatMigrationTransition() {
  const client = useQueryClient();
  return useMutation({ mutationFn: (body: ChatMigrationTransitionRequest) => chatMigrationService.transition(body), retry: false,
    onSettled: () => client.invalidateQueries({ queryKey: chatMigrationKey }) });
}

export function useChatMigrationParity() {
  const client = useQueryClient();
  return useMutation({ mutationFn: chatMigrationService.runParity, retry: false,
    onSettled: () => client.invalidateQueries({ queryKey: chatMigrationKey }) });
}

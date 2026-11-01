"use client";

/**
 * React Query hooks for voicemail management.
 */

import {
  useQuery,
  useMutation,
  useQueryClient,
  useInfiniteQuery,
} from "@tanstack/react-query";
import { voicemailService } from "@/services/calling";
import type {
  CreateMailboxRequest,
  UpdateMailboxRequest,
  VoicemailMessageFilters,
  ReadStateRequest,
} from "@/types/calling";

export const voicemailKeys = {
  all: ["calling", "voicemail"] as const,
  mailboxes: () => [...voicemailKeys.all, "mailboxes"] as const,
  mailbox: (id: string) => [...voicemailKeys.all, "mailbox", id] as const,
  messages: (mailboxId: string) =>
    [...voicemailKeys.all, "messages", mailboxId] as const,
  message: (id: string) => [...voicemailKeys.all, "message", id] as const,
};

// -------------------------------------------------------------------------
// Mailbox hooks
// -------------------------------------------------------------------------

/**
 * List all voicemail mailboxes
 */
export function useVoicemailMailboxes() {
  return useQuery({
    queryKey: voicemailKeys.mailboxes(),
    queryFn: () => voicemailService.listMailboxes(),
    staleTime: 1000 * 60,
  });
}

/**
 * Get a specific mailbox
 */
export function useVoicemailMailbox(mailboxId: string) {
  return useQuery({
    queryKey: voicemailKeys.mailbox(mailboxId),
    queryFn: () => voicemailService.getMailbox(mailboxId),
    enabled: !!mailboxId,
  });
}

/**
 * Create a new voicemail mailbox
 */
export function useCreateMailbox() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: CreateMailboxRequest) =>
      voicemailService.createMailbox(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: voicemailKeys.mailboxes() });
    },
  });
}

/**
 * Update a mailbox's settings
 */
export function useUpdateMailbox(mailboxId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: UpdateMailboxRequest) =>
      voicemailService.updateMailbox(mailboxId, data),
    onSuccess: (data) => {
      queryClient.setQueryData(voicemailKeys.mailbox(mailboxId), data);
      queryClient.invalidateQueries({ queryKey: voicemailKeys.mailboxes() });
    },
  });
}

/**
 * Delete (disable) a mailbox
 */
export function useDeleteMailbox() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (mailboxId: string) =>
      voicemailService.deleteMailbox(mailboxId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: voicemailKeys.all });
    },
  });
}

// -------------------------------------------------------------------------
// Message hooks
// -------------------------------------------------------------------------

/**
 * List messages in a mailbox with pagination
 */
export function useVoicemailMessages(filters: VoicemailMessageFilters) {
  return useInfiniteQuery({
    queryKey: voicemailKeys.messages(filters.mailboxId),
    queryFn: ({ pageParam }) =>
      voicemailService.listMessages({ ...filters, cursor: pageParam }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    enabled: !!filters.mailboxId,
    refetchInterval: 30000,
  });
}

/**
 * Get a specific voicemail message
 */
export function useVoicemailMessage(messageId: string) {
  return useQuery({
    queryKey: voicemailKeys.message(messageId),
    queryFn: () => voicemailService.getMessage(messageId),
    enabled: !!messageId,
  });
}

/**
 * Delete a voicemail message
 */
export function useDeleteVoicemailMessage(mailboxId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (messageId: string) =>
      voicemailService.deleteMessage(messageId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: voicemailKeys.messages(mailboxId),
      });
    },
  });
}

/**
 * Get short-lived access URL for message audio
 */
export function useVoicemailMediaAccess() {
  return useMutation({
    mutationFn: (messageId: string) =>
      voicemailService.getMediaAccess(messageId),
  });
}

/**
 * Mark a message as read or unread
 */
export function useSetReadState(mailboxId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      messageId,
      body,
    }: {
      messageId: string;
      body: ReadStateRequest;
    }) => voicemailService.setReadState(messageId, body),
    onSuccess: (data) => {
      queryClient.setQueryData(voicemailKeys.message(data.messageId), data);
      queryClient.invalidateQueries({
        queryKey: voicemailKeys.messages(mailboxId),
      });
    },
  });
}

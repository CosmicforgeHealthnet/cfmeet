/** Messaging hooks (MANAGED conversations; polling — no push, see Settings → Notifications). */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { messagingApi } from "@/features/messaging/api";
import { summarizeConversation } from "@/lib/messaging/relay-paths";
import { collectConversationPages, readConversationHistory, type ConversationHistory } from "@/lib/messaging/history";

export const messagingKeys = {
  me: ["messaging", "me"] as const,
  conversations: ["messaging", "conversations"] as const,
  conversation: (id: string) => ["messaging", "conversation", id] as const,
  lines: (id: string) => ["messaging", "lines", id] as const,
  names: (ids: string[]) => ["messaging", "names", ...ids] as const,
  directory: (q: string) => ["messaging", "directory", q] as const,
};

export function useMessagingIdentity() {
  return useQuery({ queryKey: messagingKeys.me, queryFn: messagingApi.me, staleTime: 5 * 60_000, retry: 1 });
}

export function useConversations(enabled: boolean) {
  return useQuery({
    queryKey: messagingKeys.conversations,
    enabled,
    refetchInterval: 15_000,
    queryFn: async ({ signal }) => {
      const out = await collectConversationPages((cursor) => messagingApi.conversations(cursor, signal));
      // The list carries at most two members; read group members in full.
      for (const c of out) {
        if (c.contextClass !== "DIRECT") {
          const m = await messagingApi.members(c.conversationId).catch(() => null);
          if (m) c.members = m.members;
        }
      }
      return out.map(summarizeConversation);
    },
  });
}

export function useNames(principalIds: string[]) {
  const ids = [...new Set(principalIds)].sort();
  return useQuery({
    queryKey: messagingKeys.names(ids),
    enabled: ids.length > 0,
    staleTime: 10 * 60_000,
    queryFn: async () => {
      const res = await messagingApi.names(ids);
      const map: Record<string, string> = {};
      for (const p of res.principals) map[p.principalId] = p.displayName;
      return map;
    },
  });
}

export function useConversation(id: string) {
  return useQuery({
    queryKey: messagingKeys.conversation(id),
    queryFn: async () => {
      const c = await messagingApi.conversation(id);
      if (c.contextClass !== "DIRECT" || !c.members) {
        const m = await messagingApi.members(id).catch(() => null);
        if (m) c.members = m.members;
      }
      return summarizeConversation(c);
    },
  });
}

/** Incremental history of a MANAGED conversation (afterSequence cursor, merged by sequence). */
export function useConversationLines(id: string, me: string | null, enabled: boolean) {
  const qc = useQueryClient();
  return useQuery({
    queryKey: messagingKeys.lines(id),
    enabled,
    refetchInterval: 5000,
    select: (history: ConversationHistory) => history.lines,
    queryFn: async ({ signal }) => {
      const previous = qc.getQueryData<ConversationHistory>(messagingKeys.lines(id));
      const history = await readConversationHistory(previous, me, (after) => messagingApi.messagesAfter(id, after, signal));
      if (!signal.aborted && history.afterSequence > (previous?.afterSequence ?? 0)) {
        void messagingApi.markRead(id, history.afterSequence).catch(() => undefined);
      }
      return history;
    },
  });
}

export function useSendMessage(id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (text: string) => messagingApi.send(id, text),
    onSuccess: () => void qc.invalidateQueries({ queryKey: messagingKeys.lines(id) }),
  });
}

export function useDirectory(q: string) {
  return useQuery({
    queryKey: messagingKeys.directory(q),
    queryFn: async () => (await messagingApi.directory(q)).people,
  });
}

export function useCreateManagedConversation() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (userIds: string[]) => {
      const resolved = await messagingApi.resolve(userIds);
      return messagingApi.createManaged(resolved.people.map((p) => p.principalId));
    },
    onSuccess: () => void qc.invalidateQueries({ queryKey: messagingKeys.conversations }),
  });
}

/** Messaging hooks (MANAGED conversations; polling — no push, see Settings → Notifications). */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { messagingApi } from "@/features/messaging/api";
import { mergeChatLines, summarizeConversation, toChatLines, type ChatLine } from "@/lib/messaging/relay-paths";

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
    queryFn: async () => {
      const out = [];
      let cursor: string | null = null;
      for (let i = 0; i < 5; i++) {
        const page: Awaited<ReturnType<typeof messagingApi.conversations>> = await messagingApi.conversations(cursor);
        out.push(...page.conversations);
        cursor = page.nextCursor;
        if (!cursor) break;
      }
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

/** Highest sequence read per conversation (commits/edits advance it without producing a line). */
const cursors = new Map<string, number>();

/** Incremental history of a MANAGED conversation (afterSequence cursor, merged by sequence). */
export function useConversationLines(id: string, me: string | null, enabled: boolean) {
  const qc = useQueryClient();
  return useQuery({
    queryKey: messagingKeys.lines(id),
    enabled,
    refetchInterval: 5000,
    queryFn: async () => {
      let lines: ChatLine[] = qc.getQueryData<ChatLine[]>(messagingKeys.lines(id)) ?? [];
      const previous = cursors.get(id) ?? 0;
      let after = Math.max(cursors.get(id) ?? 0, lines.length ? lines[lines.length - 1].sequence : 0);
      for (let i = 0; i < 20; i++) {
        const page = await messagingApi.messagesAfter(id, after);
        lines = mergeChatLines(lines, toChatLines(page.messages, me));
        after = page.messages.reduce((m, x) => Math.max(m, x.sequence), after);
        if (page.messages.length < 50) break;
      }
      cursors.set(id, after);
      if (after > previous) void messagingApi.markRead(id, after).catch(() => undefined);
      return lines;
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

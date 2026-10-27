"use client";

import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { callsService, endpointsService } from "@/services/calling";
import type { CallHistoryFilters, CommandRequest, CreateCallRequest, InvitationCommandRequest, IvrInputRequest, TransferRequest } from "@/types/calling";

export const callingKeys = {
  all: ["calling"] as const,
  identity: ["calling", "identity"] as const,
  endpoints: ["calling", "endpoints"] as const,
  history: (filters: CallHistoryFilters) => ["calling", "history", filters] as const,
  detail: (id: string) => ["calling", "call", id] as const,
  actions: (id: string) => ["calling", "actions", id] as const,
};

export function useCallingIdentity() {
  return useQuery({ queryKey: callingKeys.identity, queryFn: endpointsService.identity, retry: false });
}

export function useCallingEndpoints(enabled = true) {
  return useQuery({ queryKey: callingKeys.endpoints, queryFn: endpointsService.list, enabled, retry: false });
}

export function useCallHistory(filters: CallHistoryFilters = {}, enabled = true) {
  return useInfiniteQuery({
    queryKey: callingKeys.history(filters),
    queryFn: ({ pageParam }) => callsService.history({ ...filters, cursor: pageParam }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    enabled, refetchInterval: 10_000, retry: false,
  });
}

export function useCall(id: string) {
  return useQuery({ queryKey: callingKeys.detail(id), queryFn: () => callsService.get(id), enabled: !!id, refetchInterval: 3000, retry: false });
}

export function useCallActions(id: string) {
  return useQuery({ queryKey: callingKeys.actions(id), queryFn: () => callsService.actions(id), enabled: !!id, refetchInterval: 3000, retry: false });
}

export type CallingMutation =
  | { operation: "create"; body: CreateCallRequest; key: string }
  | { operation: "cancel" | "end" | "hold" | "resume"; callId: string; body: CommandRequest; key: string }
  | { operation: "accept" | "decline"; callId: string; body: InvitationCommandRequest; key: string }
  | { operation: "transfer"; callId: string; body: TransferRequest; key: string }
  | { operation: "removeSession"; callId: string; sessionId: string; body: CommandRequest; key: string }
  | { operation: "ivrInput"; callId: string; body: IvrInputRequest };

export function useCallingMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    retry: false,
    mutationFn: async (input: CallingMutation) => {
      switch (input.operation) {
        case "create": return callsService.create(input.body, input.key);
        case "transfer": return callsService.transfer(input.callId, input.body, input.key);
        case "accept": return callsService.accept(input.callId, input.body, input.key);
        case "decline": return callsService.decline(input.callId, input.body, input.key);
        case "removeSession": return callsService.removeSession(input.callId, input.sessionId, input.body, input.key);
        case "ivrInput": return callsService.ivrInput(input.callId, input.body);
        default: return callsService[input.operation](input.callId, input.body, input.key);
      }
    },
    // Stale-version refusals also refresh the authoritative state. Never retry a
    // command automatically with a different version or idempotency identity.
    onSettled: () => queryClient.invalidateQueries({ queryKey: callingKeys.all }),
  });
}

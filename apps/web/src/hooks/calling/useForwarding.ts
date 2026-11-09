"use client";

/**
 * React Query hooks for the signed-in user's own call forwarding rules (CAL-006).
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { forwardingService } from "@/services/calling";
import type { CreateForwardingRuleRequest, UpdateForwardingRuleRequest } from "@/types/calling";

export const forwardingKeys = {
  all: ["calling", "forwarding"] as const,
  lists: () => [...forwardingKeys.all, "list"] as const,
};

export function useForwardingRules() {
  return useQuery({ queryKey: forwardingKeys.lists(), queryFn: () => forwardingService.list(), staleTime: 60_000, retry: false });
}

function useForwardingMutation<T>(mutationFn: (input: T) => Promise<unknown>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    retry: false,
    onSettled: () => void queryClient.invalidateQueries({ queryKey: forwardingKeys.all }),
  });
}

export function useCreateForwardingRule() {
  return useForwardingMutation((data: CreateForwardingRuleRequest) => forwardingService.create(data));
}

export function useUpdateForwardingRule() {
  return useForwardingMutation(({ ruleId, body }: { ruleId: string; body: UpdateForwardingRuleRequest }) => forwardingService.update(ruleId, body));
}

export function useDeleteForwardingRule() {
  return useForwardingMutation(({ ruleId, expectedVersion }: { ruleId: string; expectedVersion: number }) => forwardingService.remove(ruleId, expectedVersion));
}

"use client";

/**
 * React Query hooks for call forwarding rules management.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { forwardingService } from "@/services/calling";
import type { CreateForwardingRuleRequest, UpdateForwardingRuleRequest } from "@/types/calling";

export const forwardingKeys = {
  all: ["calling", "forwarding"] as const,
  lists: () => [...forwardingKeys.all, "list"] as const,
  detail: (id: string) => [...forwardingKeys.all, "detail", id] as const,
};

/**
 * List all forwarding rules
 */
export function useForwardingRules() {
  return useQuery({
    queryKey: forwardingKeys.lists(),
    queryFn: () => forwardingService.list(),
    staleTime: 1000 * 60,
  });
}

/**
 * Get a specific forwarding rule
 */
export function useForwardingRule(ruleId: string) {
  return useQuery({
    queryKey: forwardingKeys.detail(ruleId),
    queryFn: () => forwardingService.get(ruleId),
    enabled: !!ruleId,
  });
}

/**
 * Create a new forwarding rule
 */
export function useCreateForwardingRule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: CreateForwardingRuleRequest) => forwardingService.create(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: forwardingKeys.all });
    },
  });
}

/**
 * Update a forwarding rule
 */
export function useUpdateForwardingRule(ruleId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: UpdateForwardingRuleRequest) =>
      forwardingService.update(ruleId, data),
    onSuccess: (data) => {
      queryClient.setQueryData(forwardingKeys.detail(ruleId), data);
      queryClient.invalidateQueries({ queryKey: forwardingKeys.lists() });
    },
  });
}

/**
 * Delete (disable) a forwarding rule
 */
export function useDeleteForwardingRule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (ruleId: string) => forwardingService.delete(ruleId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: forwardingKeys.all });
    },
  });
}

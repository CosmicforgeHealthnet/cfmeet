"use client";

/**
 * React Query hooks for call blocking management.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { blockingService } from "@/services/calling";
import type { CreateBlockRequest } from "@/types/calling";

export const blockingKeys = {
  all: ["calling", "blocks"] as const,
  lists: () => [...blockingKeys.all, "list"] as const,
  list: () => [...blockingKeys.lists()] as const,
};

/**
 * List the signed-in principal's blocking rules.
 */
export function useBlocks() {
  return useQuery({
    queryKey: blockingKeys.list(),
    queryFn: () => blockingService.list(),
    staleTime: 1000 * 60,
    retry: false,
  });
}

/**
 * Create a new block rule
 */
export function useCreateBlock() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: CreateBlockRequest) => blockingService.create(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: blockingKeys.all });
    },
  });
}

/**
 * Delete (release) a block rule
 */
export function useDeleteBlock() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (blockId: string) => blockingService.delete(blockId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: blockingKeys.all });
    },
  });
}

"use client";

/**
 * React Query hooks for call blocking management.
 */

import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { blockingService } from "@/services/calling";
import type { CreateBlockRequest, BlockFilters } from "@/types/calling";

export const blockingKeys = {
  all: ["calling", "blocks"] as const,
  lists: () => [...blockingKeys.all, "list"] as const,
  list: (filters?: BlockFilters) => [...blockingKeys.lists(), filters] as const,
};

/**
 * List blocked numbers/contacts with pagination
 */
export function useBlocks(filters: BlockFilters = {}) {
  return useInfiniteQuery({
    queryKey: blockingKeys.list(filters),
    queryFn: ({ pageParam }) =>
      blockingService.list({ ...filters, cursor: pageParam }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    staleTime: 1000 * 60,
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

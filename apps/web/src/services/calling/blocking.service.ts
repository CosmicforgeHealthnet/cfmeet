/**
 * Call Blocking API service.
 * Manages blocked numbers and contacts.
 */

import callingApi from "./api";
import type {
  Block,
  BlockList,
  CreateBlockRequest,
  BlockFilters,
} from "@/types/calling";

export const blockingService = {
  /**
   * List block rules with optional filters
   */
  list: async (filters: BlockFilters = {}): Promise<BlockList> => {
    const response = await callingApi.get<BlockList>("/v1/blocks", {
      params: filters,
    });
    return response.data;
  },

  /**
   * Create a new block rule
   */
  create: async (body: CreateBlockRequest): Promise<Block> => {
    const response = await callingApi.post<Block>("/v1/blocks", body);
    return response.data;
  },

  /**
   * Delete (release) a block rule
   */
  delete: async (blockId: string): Promise<void> => {
    await callingApi.delete(`/v1/blocks/${encodeURIComponent(blockId)}`);
  },
};

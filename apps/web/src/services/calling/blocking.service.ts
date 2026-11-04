/**
 * Call Blocking API service.
 * Manages blocked numbers and contacts.
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type {
  Block,
  BlockList,
  CreateBlockRequest,
} from "@/types/calling";

export const blockingService = {
  /**
   * List block rules with optional filters
   */
  list: async (): Promise<BlockList> => {
    const response = await callingApi.get<BlockList>("/v1/blocks");
    return response.data;
  },

  /**
   * Create a new block rule
   */
  create: async (body: CreateBlockRequest): Promise<Block> => {
    const response = await callingApi.post<Block>("/v1/blocks", body, {
      headers: { "Idempotency-Key": newIdempotencyKey() },
    });
    return response.data;
  },

  /**
   * Delete (release) a block rule
   */
  delete: async (blockId: string): Promise<void> => {
    await callingApi.delete(`/v1/blocks/${encodeURIComponent(blockId)}`, {
      headers: { "Idempotency-Key": newIdempotencyKey() },
    });
  },
};

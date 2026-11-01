/**
 * Call Blocking types for the Calling API.
 * Manages blocked numbers and contacts.
 */

import type { Uuid } from "./common";

/** Scope of the block - which call types are blocked */
export type BlockScope = "ALL" | "VOICE" | "VIDEO";

/** Reason for blocking */
export type BlockReason = "UNWANTED" | "SPAM" | "ABUSE" | "OTHER";

/** A single block rule */
export interface Block {
  blockId: Uuid;
  principalId: Uuid;
  blockedNumber?: string;
  blockedPrincipalId?: Uuid;
  scope: BlockScope;
  reason?: BlockReason;
  note?: string;
  createdAt: string;
  expiresAt?: string;
}

/** Paginated list of block rules */
export interface BlockList {
  data: Block[];
  nextCursor?: string | null;
}

/** Request body for creating a new block rule */
export interface CreateBlockRequest {
  blockedNumber?: string;
  blockedPrincipalId?: Uuid;
  scope?: BlockScope;
  reason?: BlockReason;
  note?: string;
  expiresAt?: string;
}

/** Filters for listing block rules */
export interface BlockFilters {
  limit?: number;
  cursor?: string;
  scope?: BlockScope;
}

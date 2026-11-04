import type { Uuid } from "./common";

export type BlockReason = "UNWANTED" | "HARASSMENT" | "SPAM" | "OTHER";

export interface Block {
  blockId: Uuid;
  principalId: Uuid | null;
  blockedKind: "PRINCIPAL" | "CONTACT_REFERENCE";
  blockedPrincipalId?: Uuid | null;
  blockedContactReferenceId?: Uuid | null;
  reasonCode: BlockReason;
  status: "ACTIVE" | "RELEASED";
  createdAt: string;
  releasedAt?: string | null;
  version: number;
}

export interface BlockList {
  items: Block[];
}

export interface CreateBlockRequest {
  blockedPrincipalId?: Uuid;
  blockedContactReferenceId?: Uuid;
  reasonCode: BlockReason;
}

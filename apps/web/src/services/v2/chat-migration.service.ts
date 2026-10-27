import { messagingRequest } from "./messaging-http";
import type { ChatMigrationStatus, ChatMigrationTransitionRequest, ChatParityRun } from "@/types/messaging";

/** Platform-admin endpoints: never sent through the end-user Messaging relay. */
export const chatMigrationService = {
  status: () => messagingRequest<ChatMigrationStatus>("GET", "/admin/chat-migration"),
  transition: (body: ChatMigrationTransitionRequest) => messagingRequest<ChatMigrationStatus>("POST", "/admin/chat-migration/transitions", body),
  runParity: () => messagingRequest<ChatParityRun>("POST", "/admin/chat-migration/parity-runs"),
};

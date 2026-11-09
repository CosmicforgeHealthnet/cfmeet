/**
 * Unified Messaging through the Meeting Service bridge (MANAGED conversations only).
 * Relay paths come from lib/messaging/relay-paths.ts and are checked against the mobile
 * allowlist before any request is sent.
 */
import { send } from "@/lib/api/client";
import { ROUTES } from "@/lib/api/routes";
import { idempotencyKey } from "@/features/meetings/api";
import {
  isMobileRelayPath,
  managedConversationBody,
  relayPaths,
  RelayPathError,
  type ServerConversation,
  type ServerMessage,
} from "@/lib/messaging/relay-paths";

export interface MessagingIdentity {
  principalId: string;
}

export interface Colleague {
  userId: string;
  displayName: string;
  principalId: string | null;
}

function checked(method: string, path: string): string {
  if (!isMobileRelayPath(method, path)) throw new RelayPathError("This messaging operation is not available.");
  return path;
}

export const messagingApi = {
  me: () => send<MessagingIdentity>(ROUTES.messagingMe, []),
  directory: (q?: string) => send<{ people: Colleague[] }>(ROUTES.messagingDirectory, [], { params: { q: q || undefined } }),
  resolve: (userIds: string[]) =>
    send<{ people: { userId: string; principalId: string }[] }>(ROUTES.messagingResolve, [], { data: { userIds } }),
  names: (principalIds: string[]) =>
    send<{ principals: { principalId: string; displayName: string }[] }>(ROUTES.messagingNames, [], {
      data: { principalIds },
    }),

  conversations: (cursor?: string | null) =>
    send<{ conversations: ServerConversation[]; nextCursor: string | null }>(ROUTES.relayGet, [
      checked("GET", relayPaths.conversations(cursor)),
    ]),
  conversation: (id: string) => send<ServerConversation>(ROUTES.relayGet, [checked("GET", relayPaths.conversation(id))]),
  members: (id: string) =>
    send<{ members: { principalId: string }[] }>(ROUTES.relayGet, [checked("GET", relayPaths.members(id))]),
  messagesAfter: (id: string, afterSequence: number) =>
    send<{ messages: ServerMessage[] }>(ROUTES.relayGet, [checked("GET", relayPaths.messagesAfter(id, afterSequence))]),
  send: (id: string, text: string) =>
    send<ServerMessage>(ROUTES.relayPost, [checked("POST", relayPaths.sendMessage(id))], {
      data: { clientMessageId: idempotencyKey(), text },
    }),
  markRead: (id: string, sequence: number) =>
    send<unknown>(ROUTES.relayPut, [checked("PUT", relayPaths.readCursor(id))], { data: { sequence } }),
  /** Creates a MANAGED conversation; the caller must have shown the managed disclosure first. */
  async createManaged(principalIds: string[]): Promise<ServerConversation> {
    const conv = await send<ServerConversation>(ROUTES.relayPost, [checked("POST", relayPaths.createConversation())], {
      data: managedConversationBody(principalIds),
      headers: { "Idempotency-Key": idempotencyKey() },
    });
    if (conv.securityProfile !== "MANAGED") {
      // Never silently accept a different profile from the one the user chose.
      throw new RelayPathError("The conversation was not created as a managed conversation.");
    }
    if (principalIds.length > 1) {
      for (const principalId of principalIds) {
        await send(ROUTES.relayPost, [checked("POST", relayPaths.addMember(conv.conversationId))], {
          data: { principalId },
        });
      }
    }
    return conv;
  },
};

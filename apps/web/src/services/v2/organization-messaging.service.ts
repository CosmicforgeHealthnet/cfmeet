import { messagingRequest as call } from "./messaging-http";
import { MessagingApiError } from "@/lib/messaging/errors";
import { isMessagingRelayRoute, type MessagingMethod } from "@/lib/messaging/routes";
import { organizationMessagingBase, organizationRelayPath } from "@/lib/messaging/organization";

export type OrganizationConversation = {
  conversationId: string;
  contextClass?: string;
  securityProfile?: string;
  title?: string | null;
  lastMessageAt?: string | null;
};

export type OrganizationMessage = {
  messageId: string;
  sequence: number;
  senderPrincipalId: string;
  createdAt: string;
  managed?: { text?: string | null } | null;
  lifecycle?: string;
};

/** One organization's Messaging, in that organization's own scope (J-10). */
export function organizationMessaging(organizationId: string) {
  const base = organizationMessagingBase(organizationId);
  async function relay<T>(method: MessagingMethod, path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
    if (!isMessagingRelayRoute(method, path)) throw new MessagingApiError(0, "BAD_PATH", null, "This Messaging operation is not available.");
    return call<T>(method, organizationRelayPath(organizationId, path), body, idempotencyKey ? { "Idempotency-Key": idempotencyKey } : undefined);
  }
  return {
    relay,
    conversations: () =>
      relay<{ conversations: OrganizationConversation[]; nextCursor: string | null }>("GET", "v1/conversations?limit=50"),
    messages: (conversationId: string) =>
      relay<{ messages: OrganizationMessage[] }>("GET", `v1/conversations/${conversationId}/messages?limit=50`),
    send: (conversationId: string, text: string) =>
      relay<OrganizationMessage>("POST", `v1/conversations/${conversationId}/messages`, { clientMessageId: crypto.randomUUID(), text }),
    createGroup: (title: string) =>
      relay<OrganizationConversation>(
        "POST",
        "v1/conversations",
        { contextClass: "GROUP", securityProfile: "MANAGED", title },
        `org-web-${crypto.randomUUID()}`,
      ),
    addMember: (conversationId: string, principalId: string) =>
      relay("POST", `v1/conversations/${conversationId}/members`, { principalId }),
    /** Active members of this organization only; anyone else is omitted by the server. */
    resolveMembers: (userIds: string[]) =>
      call<{ people: { userId: string; principalId: string }[] }>("POST", `${base}/members/resolve`, { userIds }),
    sessionToken: (sessionDeviceId?: string | null) =>
      call<unknown>("POST", `${base}/session-tokens`, sessionDeviceId ? { sessionDeviceId } : {}),
  };
}

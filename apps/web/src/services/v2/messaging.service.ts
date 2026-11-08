import { messagingRequest as call } from "./messaging-http";
import { MessagingApiError } from "@/lib/messaging/errors";
import { isMessagingRelayRoute, type MessagingMethod } from "@/lib/messaging/routes";
import type { Colleague, DeviceRevocationReason, MessagingClientToken, MessagingDevice, MessagingIdentity, PublishedKeyPackages, RegistrationChallenge } from "@/types/messaging";

export async function relay<T>(method: MessagingMethod, path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
  if (!isMessagingRelayRoute(method, path)) throw new MessagingApiError(0, "BAD_PATH", null, "This Messaging operation is not available.");
  return call<T>(method, `/messaging/relay/${path}`, body, idempotencyKey ? { "Idempotency-Key": idempotencyKey } : undefined);
}

export const messagingService = {
  me: () => call<MessagingIdentity>("GET", "/messaging/me"),
  directory: (q?: string) => call<{ people: Colleague[] }>("GET", `/messaging/directory${q ? `?q=${encodeURIComponent(q)}` : ""}`),
  resolve: (userIds: string[]) => call<{ people: { userId: string; principalId: string }[] }>("POST", "/messaging/directory/resolve", { userIds }),
  names: (principalIds: string[]) => call<{ principals: { principalId: string; displayName: string }[] }>("POST", "/messaging/principals/names", { principalIds }),
  registrationChallenge: () => call<RegistrationChallenge>("POST", "/messaging/devices/registration-challenges", {}),
  registerDevice: (challengeId: string, keyPackage: string, displayLabel: string) =>
    call<MessagingDevice>("POST", "/messaging/devices", { challengeId, keyPackage, displayLabel }),
  publishKeyPackages: (deviceId: string, keyPackages: string[]) =>
    call<PublishedKeyPackages>("POST", `/messaging/devices/${encodeURIComponent(deviceId)}/key-packages`, { keyPackages }),
  revokeDevice: (deviceId: string, reason: DeviceRevocationReason = "USER_REQUESTED") =>
    call<MessagingDevice>("POST", `/messaging/devices/${encodeURIComponent(deviceId)}/revoke`, { reason }),
  clientToken: (deviceId: string) => call<MessagingClientToken>("POST", "/messaging/client-tokens", { deviceId }),
  /** MANAGED realtime without an E2EE device (ADR 0049 A1): renew with the returned session device id. */
  sessionToken: (sessionDeviceId?: string | null) =>
    call<unknown>("POST", "/messaging/session-tokens", sessionDeviceId ? { sessionDeviceId } : {}),
  revokeAllSessions: () => call<{ revokedSessions: number }>("POST", "/messaging/client-sessions/revoke-all"),
  devices: (principalId: string) => relay<{ devices: MessagingDevice[] }>("GET", `v1/messaging/principals/${principalId}/devices`),
  relay,
};

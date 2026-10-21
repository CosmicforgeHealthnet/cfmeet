/**
 * Logout from CF Meet: this browser's Messaging device is revoked and every key and
 * message of the account stored here is erased (threat model §4). Best effort and
 * bounded: logout itself never waits more than a few seconds, and the Meeting Service
 * revokes every Messaging client session of the account server-side anyway.
 */
import { identityApi } from "./api";
import { CryptoClient } from "./crypto-client";
import { currentMessagingEngine } from "./engine";

async function erase(): Promise<void> {
  // The Messages page of this tab holds the device (and its lock): it signs out itself.
  const engine = currentMessagingEngine();
  if (engine) {
    await engine.signOut();
    return;
  }
  const identity = await identityApi.me().catch(() => null);
  if (!identity) return;
  const client = new CryptoClient();
  try {
    const opened = await client.call<{ status: string; deviceId?: string }>("open", {
      account: identity.principalId,
      scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId },
    });
    if (opened.status === "READY" && opened.deviceId) {
      await identityApi.revokeDevice(opened.deviceId).catch(() => {});
    }
    await client.call("wipe", { account: identity.principalId });
  } finally {
    client.terminate();
  }
}

export async function signOutMessaging(timeoutMs = 5000): Promise<void> {
  if (typeof window === "undefined" || typeof Worker === "undefined" || !("indexedDB" in window)) return;
  await Promise.race([erase().catch(() => {}), new Promise<void>((r) => setTimeout(r, timeoutMs))]);
}

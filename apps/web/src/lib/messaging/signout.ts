/**
 * Logout from CF Meet: authenticator-protected installations are locked; legacy or
 * explicitly revoked installations are erased. Best effort and
 * bounded: logout itself never waits more than a few seconds, and the Meeting Service
 * revokes every Messaging client session of the account server-side anyway.
 */
import { identityApi } from "./api";
import { CryptoClient } from "./crypto-client";
import { currentMessagingEngine, lockOtherMessagingTabs } from "./engine";
import { clearAllOutboxes } from "./outbox";
import { accountDevice } from "@/lib/auth/account-device";
import { validateLocalProtection, type LocalProtectionDescriptor } from "./local-authenticator";

async function endLocalDevice(terminal: boolean): Promise<void> {
  // The Messages page of this tab holds the device (and its lock): it signs out itself.
  const engine = currentMessagingEngine();
  if (engine) {
    await engine.signOut(terminal);
    return;
  }
  const identity = await identityApi.me().catch(() => null);
  if (!identity) return;
  const client = new CryptoClient();
  try {
    lockOtherMessagingTabs();
    const args = {
      account: identity.principalId,
      scope: { tenantId: identity.tenantId, applicationId: identity.applicationId, environmentId: identity.environmentId },
    };
    type Opened = { status: string; deviceId?: string; descriptor?: LocalProtectionDescriptor };
    let opened = await client.call<Opened>("open", args);
    for (let attempt = 0; opened.status === "LOCKED" && attempt < 6; attempt++) {
      await new Promise((done) => setTimeout(done, 100 * (attempt + 1)));
      opened = await client.call<Opened>("open", args);
    }
    if (!terminal && opened.status === "AUTHENTICATOR_REQUIRED" && opened.descriptor) {
      const descriptor = opened.descriptor;
      const current = await accountDevice.binding();
      if (!current) return;
      validateLocalProtection(descriptor, { ...args, accountDeviceId: current.accountDeviceId, installationId: current.installationId, messagingDeviceId: descriptor.messagingDeviceId });
      // No authenticator prompt is needed to close a locked profile. A server outage or
      // older server must not turn ordinary sign-out into deletion of sealed ciphertext.
      // Fresh authorization and active-device proof remain mandatory before next unlock.
      return;
    }
    const deviceId = opened.deviceId ?? opened.descriptor?.messagingDeviceId;
    if (deviceId) await identityApi.revokeDevice(deviceId).catch(() => {});
    await client.call("wipe", { account: identity.principalId });
  } finally {
    client.terminate();
  }
}

export async function signOutMessaging(timeoutMs = 5000, terminal = false): Promise<void> {
  if (typeof window === "undefined") return;
  // Queued MANAGED sends and the session-device id never outlive the sign-in.
  clearAllOutboxes();
  try {
    for (const k of Object.keys(window.sessionStorage ?? {})) if (k.startsWith("cfmeet.messaging.session-device.")) window.sessionStorage.removeItem(k);
  } catch {}
  if (typeof Worker === "undefined" || !("indexedDB" in window)) return;
  await Promise.race([endLocalDevice(terminal).catch(() => {}), new Promise<void>((r) => setTimeout(r, timeoutMs))]);
}

import { relay } from "@/services/v2/messaging.service";

/**
 * Browser notifications for chats (Web Push). The server sends metadata only (which chat);
 * the notification never contains message text. Needs this browser's messaging device.
 */
const SW_URL = "/cfmeet-push-sw.js";

type PushConfig = { platforms: { WEB_PUSH: boolean }; webPush: { applicationServerKey: string } | null };
type TrustedTypesLike = { createPolicy(name: string, rules: { createScriptURL(u: string): string }): { createScriptURL(u: string): unknown } };

export const webPushSupported = () =>
  typeof window !== "undefined" && "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

/** The service worker URL as a TrustedScriptURL where Trusted Types are enforced. */
function swUrl(): string {
  const tt = (globalThis as { trustedTypes?: TrustedTypesLike }).trustedTypes;
  if (!tt) return SW_URL;
  const policy = tt.createPolicy("cfmeet", {
    createScriptURL(u: string) {
      if (u !== SW_URL) throw new TypeError("refused script URL");
      return u;
    },
  });
  return policy.createScriptURL(SW_URL) as string;
}

function keyBytes(b64url: string): Uint8Array<ArrayBuffer> {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (b64url.length % 4)) % 4);
  const raw = atob(b64);
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

/** Whether the server can send browser notifications at all. */
export async function webPushAvailable(): Promise<boolean> {
  const c = await relay<PushConfig>("GET", "v1/messaging/push/configuration").catch(() => null);
  return !!c?.platforms.WEB_PUSH && !!c.webPush;
}

/** Whether this browser already has a push subscription. */
export async function webPushEnabled(): Promise<boolean> {
  if (!webPushSupported()) return false;
  const reg = await navigator.serviceWorker.getRegistration(SW_URL).catch(() => undefined);
  return !!(await reg?.pushManager.getSubscription().catch(() => null)) && Notification.permission === "granted";
}

/** Ask permission, subscribe and register this device's push handle with Messaging. */
export async function enableWebPush(deviceId: string): Promise<void> {
  if (!webPushSupported()) throw new Error("This browser can't show notifications.");
  const config = await relay<PushConfig>("GET", "v1/messaging/push/configuration");
  if (!config.platforms.WEB_PUSH || !config.webPush) throw new Error("Browser notifications aren't switched on for CF Meet yet.");
  const permission = await Notification.requestPermission();
  if (permission !== "granted") throw new Error("Notifications are blocked for this site. Allow them in your browser's site settings.");
  const reg = await navigator.serviceWorker.register(swUrl(), { scope: "/" });
  await navigator.serviceWorker.ready;
  const sub = (await reg.pushManager.getSubscription())
    ?? (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(config.webPush.applicationServerKey) }));
  await relay("PUT", `v1/messaging/devices/${deviceId}/push-registration`, { platform: "WEB_PUSH", handle: JSON.stringify(sub.toJSON()), enabled: true });
}

/** Stop notifications on this browser. */
export async function disableWebPush(deviceId: string | null): Promise<void> {
  const reg = webPushSupported() ? await navigator.serviceWorker.getRegistration(SW_URL).catch(() => undefined) : undefined;
  await (await reg?.pushManager.getSubscription().catch(() => null))?.unsubscribe().catch(() => false);
  if (deviceId) await relay("DELETE", `v1/messaging/devices/${deviceId}/push-registration`).catch(() => undefined);
}

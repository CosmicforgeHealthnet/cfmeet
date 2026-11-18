/**
 * Notification Service (relayed by the Meeting Service, /api/v2/notifications/*): the in-app
 * inbox (bell), read/dismiss, and the push endpoint of this browser's Account device.
 */
import apiV2 from "./api";
import type {
  DismissMyNotificationsResponse,
  GetMyNotificationUnreadCountResponse,
  ListMyNotificationsResponse,
  MarkMyNotificationsReadResponse,
  NotificationItem,
} from "@/types/generated/notifications";

const key = () => ({ headers: { "Idempotency-Key": crypto.randomUUID() } });

export const notificationsService = {
  list: async (cursor?: string | null) =>
    (await apiV2.get<ListMyNotificationsResponse>("/notifications", { params: { limit: 30, ...(cursor ? { cursor } : {}) } })).data,
  unreadCount: async () => (await apiV2.get<GetMyNotificationUnreadCountResponse>("/notifications/unread-count")).data,
  markRead: async (ids?: string[]) =>
    (await apiV2.post<MarkMyNotificationsReadResponse>("/notifications/read", ids ? { ids } : { upTo: new Date().toISOString() }, key())).data,
  dismiss: async (ids: string[]) =>
    (await apiV2.post<DismissMyNotificationsResponse>("/notifications/dismiss", { ids }, key())).data,
  update: async (n: NotificationItem, patch: { read?: boolean; dismissed?: boolean }) =>
    (await apiV2.patch<NotificationItem>(`/notifications/${encodeURIComponent(n.id)}`, { ...patch, expectedVersion: n.version }, key())).data,

  /** This browser's Account device id (the push target belongs to it). */
  currentDeviceId: async () => {
    const r = (await apiV2.get<{ devices: { deviceId: string; current: boolean }[] }>("/me/devices")).data;
    return r.devices.find((d) => d.current)?.deviceId ?? null;
  },
  registerPush: async (accountDeviceId: string, handle: string) =>
    (await apiV2.put(`/notifications/devices/${encodeURIComponent(accountDeviceId)}/push`, { platform: "WEB_PUSH", handle }, key())).data,
  unregisterPush: async (accountDeviceId: string) =>
    (await apiV2.delete(`/notifications/devices/${encodeURIComponent(accountDeviceId)}/push`, key())).data,
};

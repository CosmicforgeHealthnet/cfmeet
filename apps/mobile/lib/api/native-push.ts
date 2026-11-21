import { send } from "./client";
import { ROUTES } from "./routes";
import type { NativePushApi } from "../../features/notifications/native-push-registration";

/** Native APNs/FCM handles only; account and device ownership are verified by the relay. */
export const nativePushApi: NativePushApi = {
  register: (device, token) => send(ROUTES.registerNativePush, [device], { data: token }),
  remove: (device, platform) => send(ROUTES.removeNativePush, [device], { params: { platform } }),
};

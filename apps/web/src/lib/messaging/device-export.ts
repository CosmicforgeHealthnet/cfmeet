import { CryptoClient } from "./crypto-client";
import { messagingService } from "@/services/v2/messaging.service";

import { collectDeviceHistory, type LocalHistoryPage } from "./device-export-data";

export async function privateHistoryFromThisDevice() {
  const identity = await messagingService.me();
  const worker = new CryptoClient();
  try {
    const result = await collectDeviceHistory((after) => worker.call<LocalHistoryPage>("exportLocalHistory", { account: identity.principalId, after }));
    return {
      schemaVersion: 1, source: "THIS_DEVICE_ONLY", exportedAt: new Date().toISOString(),
      attachmentBytesIncluded: false, otherDevicesIncluded: false,
      ...result,
    };
  } finally { worker.terminate(); }
}

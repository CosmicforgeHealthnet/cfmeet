"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { messagingService } from "@/services/v2/messaging.service";
import type { DeviceRevocationReason } from "@/types/messaging";
import type { MessagingEngine } from "@/lib/messaging/engine";

export const messagingKeys = {
  all: ["v2", "messaging"] as const,
  identity: ["v2", "messaging", "identity"] as const,
  directory: (search: string) => ["v2", "messaging", "directory", search] as const,
  devices: (principalId: string) => ["v2", "messaging", "devices", principalId] as const,
};

export function useMessagingIdentity() {
  return useQuery({ queryKey: messagingKeys.identity, queryFn: messagingService.me, retry: false });
}

export function useMessagingDirectory(search = "", enabled = true) {
  return useQuery({ queryKey: messagingKeys.directory(search), queryFn: () => messagingService.directory(search), enabled, retry: false });
}

export function useMessagingDevices(principalId: string) {
  return useQuery({ queryKey: messagingKeys.devices(principalId), queryFn: () => messagingService.devices(principalId), enabled: !!principalId, retry: false, refetchInterval: 30_000 });
}

export function useRevokeMessagingDevice(principalId: string) {
  const client = useQueryClient();
  return useMutation({ mutationFn: ({ deviceId, reason }: { deviceId: string; reason: DeviceRevocationReason }) => messagingService.revokeDevice(deviceId, reason),
    retry: false, onSettled: () => client.invalidateQueries({ queryKey: messagingKeys.devices(principalId) }) });
}

/** Registration must keep proof generation and key adoption inside the crypto engine. */
export function useRegisterMessagingDevice(engine: MessagingEngine) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async () => {
      await engine.registerDevice();
      const snapshot = engine.getSnapshot();
      if (snapshot.phase !== "ready") throw new Error(snapshot.detail || "Could not register this messaging device.");
    },
    retry: false,
    onSettled: () => client.invalidateQueries({ queryKey: messagingKeys.all }),
  });
}

/** Revokes through v2, then lets the crypto engine erase this browser's local keys. */
export function useRemoveCurrentMessagingDevice(engine: MessagingEngine) {
  const client = useQueryClient();
  return useMutation({ mutationFn: () => engine.removeThisDevice(), retry: false,
    onSettled: () => client.invalidateQueries({ queryKey: messagingKeys.all }) });
}

/** Keeps reconnects paused after v2 session revocation, without erasing device keys. */
export function useEndMessagingSessions(engine: MessagingEngine) {
  return useMutation({ mutationFn: () => engine.revokeAllSessions(), retry: false });
}

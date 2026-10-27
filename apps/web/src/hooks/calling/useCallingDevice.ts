"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { createSignalingService, endpointsService } from "@/services/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import { callingKeys } from "./useCalls";
import type { Endpoint, RecoveredInvitation } from "@/types/calling";

export function useCallingDevice(principalId?: string) {
  const queryClient = useQueryClient();
  const [endpoint, setEndpoint] = useState<Endpoint | null>(null);
  const [status, setStatus] = useState("offline");
  const [error, setError] = useState<string | null>(null);
  const [invitations, setInvitations] = useState<RecoveredInvitation[]>([]);
  const service = useRef<ReturnType<typeof createSignalingService> | null>(null);
  const [generation, setGeneration] = useState(0);

  const register = useCallback(async () => {
    if (!principalId) return;
    setError(null);
    setStatus("connecting");
    try {
      const storageKey = `cfmeet.calling.endpoint.${principalId}`;
      let key = localStorage.getItem(storageKey);
      if (!key) { key = crypto.randomUUID(); localStorage.setItem(storageKey, key); }
      const registered = await endpointsService.register({ principalId, endpointKey: key,
        endpointKind: "WEB", capabilities: { audio: true, video: true } });
      setEndpoint(registered);
      setGeneration((value) => value + 1);
      await queryClient.invalidateQueries({ queryKey: callingKeys.endpoints });
    } catch (e) { setError(callingErrorMessage(e)); setStatus("offline"); }
  }, [principalId, queryClient]);

  useEffect(() => {
    if (!endpoint || !principalId) return;
    let stopped = false;
    let socket: WebSocket | undefined;
    let reconnect: ReturnType<typeof setTimeout> | undefined;
    let renewal: ReturnType<typeof setTimeout> | undefined;
    let heartbeat: ReturnType<typeof setInterval> | undefined;
    let attempts = 0;
    const recover = async () => {
      try {
        const result = await service.current?.invitations();
        if (!stopped && result) setInvitations(result.data);
      } catch (e) { if (!stopped) setError(callingErrorMessage(e)); }
    };
    const connect = async () => {
      try {
        const credential = await endpointsService.mintSignalingCredential({ principalId, endpointId: endpoint.endpointId });
        if (stopped) return;
        service.current = createSignalingService(credential);
        socket = service.current.connect();
        socket.onopen = () => {
          attempts = 0; setStatus("online"); setError(null);
          void recover();
          void queryClient.invalidateQueries({ queryKey: callingKeys.all });
          heartbeat = setInterval(() => { if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "ping" })); }, 25_000);
        };
        socket.onmessage = (event) => {
          try {
            const message = JSON.parse(event.data);
            if (message.type !== "signaling.pong") {
              void recover();
              void queryClient.invalidateQueries({ queryKey: callingKeys.all });
            }
          } catch { /* A malformed delivery never replaces authoritative HTTP state. */ }
        };
        socket.onclose = () => {
          clearInterval(heartbeat); clearTimeout(renewal);
          if (!stopped) { setStatus("reconnecting"); reconnect = setTimeout(() => void connect(), Math.min(30_000, 1000 * 2 ** attempts++)); }
        };
        renewal = setTimeout(() => socket?.close(), Math.max(1000, Date.parse(credential.expiresAt) - Date.now() - 15_000));
      } catch (e) {
        if (!stopped) {
          setError(callingErrorMessage(e)); setStatus("offline");
          reconnect = setTimeout(() => void connect(), Math.min(30_000, 1000 * 2 ** attempts++));
        }
      }
    };
    void connect();
    const recovery = setInterval(() => void recover(), 10_000);
    return () => {
      stopped = true; clearTimeout(reconnect); clearTimeout(renewal);
      clearInterval(heartbeat); clearInterval(recovery);
      socket?.close(); service.current = null;
    };
  }, [endpoint, principalId, queryClient, generation]);

  const answer = async (invitation: RecoveredInvitation, accept: boolean) => {
    if (!service.current) throw new Error("Connect this browser before answering.");
    const result = accept
      ? await service.current.accept(invitation.invitationId, { expectedVersion: invitation.callVersion })
      : await service.current.decline(invitation.invitationId, { expectedVersion: invitation.callVersion });
    setInvitations((current) => current.filter((i) => i.invitationId !== invitation.invitationId));
    await queryClient.invalidateQueries({ queryKey: callingKeys.all });
    return result;
  };

  return { endpoint, status, error, invitations, register, answer,
    mediaCredential: (id: string) => {
      if (!service.current) throw new Error("Connect this browser before joining.");
      return service.current.mediaCredential(id);
    } };
}

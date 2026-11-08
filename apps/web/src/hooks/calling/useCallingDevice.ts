"use client";

/**
 * This browser as a Calling endpoint (CAL-004).
 *
 * - Registration is automatic once the user is signed in (POST /v1/calling/endpoints with the
 *   browser's stable endpointKey — idempotent on the backend), unless the user turned
 *   "receive calls in this browser" off. The preference is per account and per browser.
 * - The short-lived signaling credential (POST /v1/calling/signaling-credentials, ≤ 3600 s) is
 *   renewed before expiry; one that is malformed, expired or claims a longer life is not
 *   trusted and is replaced at once (`renewalDelay`).
 * - The signaling WebSocket reconnects with bounded backoff; offers are recovered over HTTP
 *   (GET /v1/calling/invitations) on open, on every signal and every 10 s, so a missed push
 *   never hides a ringing call. A 4401 close (endpoint revoked) re-registers once.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { createSignalingService, endpointsService } from "@/services/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import { renewalDelay } from "@/lib/calling/runtime-guard";
import { callingKeys } from "./useCalls";
import type { Endpoint, RecoveredInvitation } from "@/types/calling";

export type DeviceStatus = "disabled" | "offline" | "connecting" | "online" | "reconnecting";

const preferenceKey = (principalId: string) => `cfmeet.calling.receive.${principalId}`;
const endpointKeyStorage = (principalId: string) => `cfmeet.calling.endpoint.${principalId}`;

function readPreference(principalId: string): boolean {
  try { return localStorage.getItem(preferenceKey(principalId)) !== "off"; } catch { return true; }
}

export function useCallingDevice(principalId?: string) {
  const queryClient = useQueryClient();
  const [endpoint, setEndpoint] = useState<Endpoint | null>(null);
  const [status, setStatus] = useState<DeviceStatus>("offline");
  const [error, setError] = useState<string | null>(null);
  const [invitations, setInvitations] = useState<RecoveredInvitation[]>([]);
  const [receiveCalls, setReceiveCalls] = useState(() => (principalId ? readPreference(principalId) : false));
  const service = useRef<ReturnType<typeof createSignalingService> | null>(null);
  const [generation, setGeneration] = useState(0);
  const registering = useRef(false);

  const register = useCallback(async () => {
    if (!principalId || registering.current) return;
    registering.current = true;
    setError(null);
    setStatus("connecting");
    try {
      let key: string | null = null;
      try { key = localStorage.getItem(endpointKeyStorage(principalId)); } catch { /* storage disabled */ }
      if (!key) {
        key = crypto.randomUUID();
        try { localStorage.setItem(endpointKeyStorage(principalId), key); } catch { /* one-page endpoint */ }
      }
      const registered = await endpointsService.register({ principalId, endpointKey: key,
        endpointKind: "WEB", capabilities: { audio: true, video: true } });
      setEndpoint(registered);
      setGeneration((value) => value + 1);
      await queryClient.invalidateQueries({ queryKey: callingKeys.endpoints });
    } catch (e) {
      setError(callingErrorMessage(e));
      setStatus("offline");
    } finally {
      registering.current = false;
    }
  }, [principalId, queryClient]);

  /** Turns receiving on (and registers) or off (revokes this browser's endpoint). */
  const setReceiving = useCallback(async (on: boolean) => {
    if (!principalId) return;
    try { localStorage.setItem(preferenceKey(principalId), on ? "on" : "off"); } catch { /* page-only choice */ }
    setReceiveCalls(on);
    if (on) { await register(); return; }
    const current = endpoint;
    setEndpoint(null);
    setInvitations([]);
    setStatus("offline");
    if (current) {
      try { await endpointsService.revoke(current.endpointId); } catch (e) { setError(callingErrorMessage(e)); }
      await queryClient.invalidateQueries({ queryKey: callingKeys.endpoints });
    }
  }, [principalId, endpoint, register, queryClient]);

  // Automatic registration once signed in (CAL-004): no button press, survives reloads.
  useEffect(() => {
    if (!principalId || !receiveCalls || endpoint) return;
    const timer = setTimeout(() => void register(), 0);
    return () => clearTimeout(timer);
  }, [principalId, receiveCalls, endpoint, register]);

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
    const retry = () => { reconnect = setTimeout(() => void connect(), Math.min(30_000, 1000 * 2 ** attempts++)); };
    const connect = async () => {
      try {
        const credential = await endpointsService.mintSignalingCredential({ principalId, endpointId: endpoint.endpointId });
        if (stopped) return;
        const delay = renewalDelay(credential.expiresAt, Date.now());
        if (delay === null) throw new Error("The calling connection credential was not valid. Retrying…");
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
        socket.onclose = (event) => {
          clearInterval(heartbeat); clearTimeout(renewal);
          if (stopped) return;
          if (event.code === 4401) {
            // The endpoint was revoked (another tab / device settings): register again.
            stopped = true;
            setEndpoint(null);
            setStatus("offline");
            return;
          }
          setStatus("reconnecting");
          retry();
        };
        // Renew the short-lived credential before it expires (a fresh socket per credential).
        renewal = setTimeout(() => socket?.close(), delay);
      } catch (e) {
        if (!stopped) {
          setError(callingErrorMessage(e)); setStatus("reconnecting");
          retry();
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

  const answer = useCallback(async (invitation: RecoveredInvitation, accept: boolean) => {
    if (!service.current) throw new Error("This browser is still connecting to calling. Try again in a moment.");
    const result = accept
      ? await service.current.accept(invitation.invitationId, { expectedVersion: invitation.callVersion })
      : await service.current.decline(invitation.invitationId, { expectedVersion: invitation.callVersion });
    setInvitations((current) => current.filter((i) => i.invitationId !== invitation.invitationId));
    await queryClient.invalidateQueries({ queryKey: callingKeys.all });
    return result;
  }, [queryClient]);

  /** A media credential for the endpoint's own session (signaling path), or null when offline. */
  const mediaCredential = useCallback((sessionId: string) => service.current?.mediaCredential(sessionId) ?? null, []);

  // Receiving turned off is a derived status (nothing to connect).
  return { endpoint, status: receiveCalls ? status : "disabled" as DeviceStatus, error, invitations, receiveCalls, register, setReceiving, answer, mediaCredential };
}

export type CallingDevice = ReturnType<typeof useCallingDevice>;

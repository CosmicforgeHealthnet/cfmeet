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
 * - Every new PENDING offer is acknowledged once (POST …/acknowledge, C1): the server marks
 *   it RINGING, so the caller sees "Ringing" only when this device really rings.
 * - A 401 on the signaling credential is never retried with the same token (C4):
 *   SIGNALING_CREDENTIAL_EXPIRED mints a fresh credential; AUTHENTICATION_FAILED stops
 *   polling and re-registers the endpoint at most once per window (then stays offline).
 *   A 401 while minting (account session gone after the shared refresh) stops retrying.
 * - Logout revokes this browser's endpoint while still signed in (S-10,
 *   `lib/calling/signout.ts`); no tab registers it again until the next sign-in.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { createSignalingService, endpointsService } from "@/services/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import { renewalDelay } from "@/lib/calling/runtime-guard";
import { mayReregister, offersToAcknowledge, signalingFailure } from "@/lib/calling/signaling-auth";
import { forgetEndpointId, isSigningOut, rememberEndpointId, type KeyValueStorage } from "@/lib/calling/endpoint-signout";
import { callingKeys } from "./useCalls";
import type { Endpoint, RecoveredInvitation } from "@/types/calling";

export type DeviceStatus = "disabled" | "offline" | "connecting" | "online" | "reconnecting";

const preferenceKey = (principalId: string) => `cfmeet.calling.receive.${principalId}`;
const endpointKeyStorage = (principalId: string) => `cfmeet.calling.endpoint.${principalId}`;

function browserStorage(): KeyValueStorage | null {
  try { return localStorage; } catch { return null; }
}

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
  /** When this browser last re-registered after a rejected credential (C4). */
  const lastReregister = useRef<number | null>(null);
  /** Offers already acknowledged by this browser (C1). */
  const acknowledged = useRef<Set<string>>(new Set());

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
      // A logout in progress (this or another tab) revokes this browser's endpoint: never
      // register it again before the next sign-in (S-10).
      if (isSigningOut(browserStorage())) { setStatus("offline"); return; }
      const registered = await endpointsService.register({ principalId, endpointKey: key,
        endpointKind: "WEB", capabilities: { audio: true, video: true } });
      if (isSigningOut(browserStorage())) {
        // Logout started while registering: undo it (best effort) instead of keeping it.
        void endpointsService.revoke(registered.endpointId).catch(() => {});
        setStatus("offline");
        return;
      }
      // Remembered so logout can revoke this browser's endpoint while still signed in.
      rememberEndpointId(browserStorage(), principalId, registered.endpointId);
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
    forgetEndpointId(browserStorage(), principalId);
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
    /** The credential in use was refused (C4): never send it again. */
    const credentialRefused = (e: unknown): boolean => {
      const failure = signalingFailure(e);
      if (failure === "transient" || stopped) return false;
      service.current = null;
      if (failure === "expired") {
        // Lifetime ended (e.g. the device slept through renewal): a fresh credential now.
        clearTimeout(renewal);
        if (socket && socket.readyState !== WebSocket.CLOSED) socket.close();
        else { setStatus("reconnecting"); retry(); }
        return true;
      }
      // Authority gone (endpoint revoked elsewhere, principal or minting chain invalid).
      stopped = true;
      clearTimeout(reconnect); clearTimeout(renewal);
      socket?.close();
      setStatus("offline");
      if (mayReregister(lastReregister.current, Date.now())) {
        lastReregister.current = Date.now();
        setEndpoint(null);
      } else {
        setError("Calling could not verify this browser. Sign in again or turn receiving calls back on.");
      }
      return true;
    };
    const acknowledge = (offers: RecoveredInvitation[]) => {
      for (const id of offersToAcknowledge(offers, acknowledged.current)) {
        acknowledged.current.add(id);
        void service.current?.acknowledge(id).catch((e) => {
          // A refused credential is handled once; an offer that just ended needs nothing.
          if (!credentialRefused(e) && signalingFailure(e) === "transient") acknowledged.current.delete(id);
        });
      }
    };
    const recover = async () => {
      // No credential (refused, or being re-minted): nothing to poll with.
      const current = service.current;
      if (!current) return;
      try {
        const result = await current.invitations();
        if (!stopped && result) {
          setInvitations(result.data);
          acknowledge(result.data);
        }
      } catch (e) { if (!stopped && !credentialRefused(e)) setError(callingErrorMessage(e)); }
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
          setError(callingErrorMessage(e));
          // Minting refused after the shared session refresh: the account is signed out.
          // Stop here; the normal sign-in flow takes over (never a retry loop).
          if ((e as { response?: { status?: number } })?.response?.status === 401) {
            stopped = true; setStatus("offline"); return;
          }
          setStatus("reconnecting");
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

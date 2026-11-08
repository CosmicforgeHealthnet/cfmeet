"use client";

/**
 * App-wide calling session (CAL-004, CAL-014, CAL-015).
 *
 * Mounted once at the root (inside QueryProvider) so the browser stays reachable and an
 * active call keeps its media while the user moves between app pages:
 *  - automatic endpoint registration + signaling for the signed-in user (useCallingDevice);
 *  - the global incoming-call layer (caller identity, ringtone, notification, accept/decline);
 *  - the active-call dock that owns the media connection and its automatic reconnect;
 *  - call launch: direct / group / extension (Calling via the adapter) and contact /
 *    conversation (Communications Hub launch, the production-verified first-party path).
 *
 * It is inert for guests and signed-out pages (no request is made).
 */

import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState, type ReactNode } from "react";
import axios from "axios";
import { useProfile } from "@/hooks";
import { useCall, useCallingIdentity } from "@/hooks/calling/useCalls";
import { useCallingDevice, type CallingDevice } from "@/hooks/calling/useCallingDevice";
import { callsService } from "@/services/calling";
import { hubCallsService } from "@/services/calling/hub-calls.service";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { isWindingDown, ownSession } from "@/lib/calling/call-permissions";
import { INITIAL_MEDIA_STATE, isRetryableCredentialFailure, mediaReducer, type MediaEvent, type MediaState } from "@/lib/calling/media-reconnect";
import IncomingCallLayer from "@/components/calling/IncomingCallLayer";
import ActiveCallDock from "@/components/calling/ActiveCallDock";
import type { CallKind, CreateCallRequest, MediaCredential, RecoveredInvitation } from "@/types/calling";

export interface ActiveMedia {
  callId: string;
  sessionId: string;
  kind: CallKind;
  /** null while a (fresh) credential is being requested. */
  credential: MediaCredential | null;
}

export type CallTarget =
  | { targetPrincipalIds: string[] }
  | { targetExtension: string };

export interface CallingContextValue {
  principalId: string;
  device: CallingDevice;
  media: ActiveMedia | null;
  mediaState: MediaState;
  joinMedia: (callId: string, sessionId: string, kind?: CallKind) => Promise<void>;
  /** Drops this device's media (the session is left by the caller of `leaveSession`). */
  leaveMedia: () => void;
  retryMedia: () => void;
  answer: (invitation: RecoveredInvitation, accept: boolean) => Promise<string>;
  /** Direct (1:1), group (several principals) or extension call through the adapter; returns the callId. */
  startCall: (kind: CallKind, target: CallTarget) => Promise<string>;
  /** Communications Hub launch from a contact / a conversation; returns the callId. */
  callContact: (contactId: string, kind: CallKind) => Promise<string>;
  callConversation: (conversationId: string, kind: CallKind) => Promise<string>;
  /** Media callbacks for the dock. */
  mediaEvents: {
    connected: () => void;
    disconnected: (userInitiated: boolean) => void;
    providerReconnecting: () => void;
    providerReconnected: () => void;
  };
}

const CallingContext = createContext<CallingContextValue | null>(null);

/** The app-wide calling session, or null for guests / while calling is unavailable. */
export function useCalling(): CallingContextValue | null {
  return useContext(CallingContext);
}

export function CallingRoot({ children }: { children: ReactNode }) {
  const profile = useProfile();
  const user = profile.data as { id?: unknown } | undefined;
  if (!user) return <>{children}</>;
  return <CallingIdentityGate key={String(user.id ?? "account")}>{children}</CallingIdentityGate>;
}

function CallingIdentityGate({ children }: { children: ReactNode }) {
  const identity = useCallingIdentity();
  const principalId = identity.data?.principalId;
  // Calling not configured / unavailable: the app works without it; the Calls page explains why.
  if (!principalId) return <>{children}</>;
  return <CallingSession key={principalId} principalId={principalId}>{children}</CallingSession>;
}

function status(error: unknown): number | undefined {
  return axios.isAxiosError(error) ? error.response?.status : undefined;
}

const LEFT: MediaState = { status: "left", attempt: 0, retryInMs: null, reason: null };

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function CallingSession({ principalId, children }: { principalId: string; children: ReactNode }) {
  const device = useCallingDevice(principalId);
  const [media, setMedia] = useState<ActiveMedia | null>(null);
  const [mediaState, dispatch] = useReducer((state: MediaState, event: MediaEvent) => mediaReducer(state, event), INITIAL_MEDIA_STATE);
  const mediaCall = useCall(media?.callId ?? "");
  const mediaRef = useRef(media);
  useEffect(() => { mediaRef.current = media; }, [media]);

  const { mediaCredential, answer: answerOffer } = device;
  const credentialFor = useCallback(async (callId: string, sessionId: string) => {
    // The endpoint's signaling credential when connected, else the user's own session via the adapter.
    return (await mediaCredential(sessionId)) ?? callsService.issueSessionMediaCredential(callId, sessionId);
  }, [mediaCredential]);

  const joinMedia = useCallback(async (callId: string, sessionId: string, kind?: CallKind) => {
    const callKind = kind ?? (await callsService.get(callId)).kind;
    dispatch({ type: "JOIN" });
    setMedia({ callId, sessionId, kind: callKind, credential: null });
    try {
      const credential = await credentialFor(callId, sessionId);
      setMedia((current) => (current?.sessionId === sessionId ? { ...current, credential } : current));
    } catch (error) {
      dispatch({ type: "REJOIN_FAILED", retryable: false, reason: "Could not connect the call audio. Try again." });
      throw error;
    }
  }, [credentialFor]);

  const leaveMedia = useCallback(() => {
    dispatch({ type: "LEAVE" });
    setMedia(null);
  }, []);

  // The call ended (by anyone, or automatically): the media is dropped (derived, not stored).
  const mediaCallState = mediaCall.data?.state;
  const callGone = (!!mediaCallState && isWindingDown(mediaCallState)) || status(mediaCall.error) === 404;
  const activeMedia = callGone ? null : media;
  const activeMediaState: MediaState = callGone ? LEFT : mediaState;

  // A live call whose connection was lost: fresh credential + bounded backoff (CAL-015).
  useEffect(() => {
    if (mediaState.status !== "reconnecting" || !media || callGone) return;
    const { callId, sessionId } = media;
    const timer = setTimeout(async () => {
      dispatch({ type: "REJOIN_STARTED" });
      try {
        const credential = await credentialFor(callId, sessionId);
        setMedia((current) => (current?.sessionId === sessionId ? { ...current, credential } : current));
      } catch (error) {
        dispatch({ type: "REJOIN_FAILED", retryable: isRetryableCredentialFailure(status(error)) });
      }
    }, mediaState.retryInMs ?? 0);
    return () => clearTimeout(timer);
    // `media` identity changes with the credential; the attempt counter drives retries.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mediaState.status, mediaState.attempt, mediaState.retryInMs, media?.sessionId, credentialFor, callGone]);

  const mediaEvents = useMemo(() => ({
    connected: () => dispatch({ type: "CONNECTED" }),
    providerReconnecting: () => dispatch({ type: "PROVIDER_RECONNECTING" }),
    providerReconnected: () => dispatch({ type: "PROVIDER_RECONNECTED" }),
    disconnected: (userInitiated: boolean) => {
      const current = mediaRef.current;
      // The user's own leave goes through leaveMedia (media is already null). A client-side
      // disconnect while media is still set is our own room swap (fresh credential): ignore it.
      if (!current || userInitiated) return;
      // Unmount the dead room; the reconnect effect mounts a new one with a fresh credential.
      setMedia((m) => (m ? { ...m, credential: null } : m));
      void callsService.get(current.callId)
        .then((call) => dispatch({ type: "DISCONNECTED", userInitiated: false, callLive: !isWindingDown(call.state) }))
        .catch((error) => dispatch({ type: "DISCONNECTED", userInitiated: false, callLive: status(error) !== 404 }));
    },
  }), []);

  const answer = useCallback(async (invitation: RecoveredInvitation, accept: boolean) => {
    const result = await answerOffer(invitation, accept);
    if (accept && "sessionId" in result) await joinMedia(result.call.callId, result.sessionId, invitation.callKind);
    return result.call.callId;
  }, [answerOffer, joinMedia]);

  const requireEndpoint = useCallback(() => {
    if (!device.endpoint) {
      throw new Error(device.receiveCalls
        ? "This browser is still connecting to calling. Try again in a moment."
        : "Turn on “Receive calls in this browser” (Calls › Devices) to place calls from here.");
    }
    return device.endpoint.endpointId;
  }, [device.endpoint, device.receiveCalls]);

  /** After a Hub launch: find the caller's own session on this endpoint and join its media. */
  const joinLaunched = useCallback(async (callId: string, endpointId: string, kind: CallKind) => {
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const call = await callsService.get(callId);
      const session = ownSession(call, principalId, endpointId);
      if (session) { await joinMedia(callId, session.sessionId, kind); return; }
      if (isWindingDown(call.state)) return;
      await sleep(500);
    }
    throw new Error("The call was started, but this browser could not join it yet. Open it from Calls.");
  }, [principalId, joinMedia]);

  const startCall = useCallback(async (kind: CallKind, target: CallTarget) => {
    const endpointId = requireEndpoint();
    const body: CreateCallRequest = { kind, initiatorPrincipalId: principalId, initiatorEndpointId: endpointId, ...target };
    const result = await callsService.create(body, newIdempotencyKey());
    if (result.initiatorSessionId) await joinMedia(result.call.callId, result.initiatorSessionId, kind);
    return result.call.callId;
  }, [principalId, requireEndpoint, joinMedia]);

  const callContact = useCallback(async (contactId: string, kind: CallKind) => {
    const endpointId = requireEndpoint();
    const launched = await hubCallsService.callContact(contactId, kind, endpointId);
    await joinLaunched(launched.callId, endpointId, kind);
    return launched.callId;
  }, [requireEndpoint, joinLaunched]);

  const callConversation = useCallback(async (conversationId: string, kind: CallKind) => {
    const endpointId = requireEndpoint();
    const launched = await hubCallsService.callConversation(conversationId, kind, endpointId);
    await joinLaunched(launched.callId, endpointId, kind);
    return launched.callId;
  }, [requireEndpoint, joinLaunched]);

  const value = useMemo<CallingContextValue>(() => ({
    principalId, device, media: activeMedia, mediaState: activeMediaState, joinMedia, leaveMedia,
    retryMedia: () => dispatch({ type: "RETRY_NOW" }),
    answer, startCall, callContact, callConversation, mediaEvents,
  }), [principalId, device, activeMedia, activeMediaState, joinMedia, leaveMedia, answer, startCall, callContact, callConversation, mediaEvents]);

  return (
    <CallingContext.Provider value={value}>
      {children}
      <IncomingCallLayer />
      <ActiveCallDock />
    </CallingContext.Provider>
  );
}

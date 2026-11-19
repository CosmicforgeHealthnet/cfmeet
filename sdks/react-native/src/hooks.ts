// React hooks over the device clients (`react` is a peer dependency). Each hook owns the
// session it opens and closes it on unmount or when its inputs change.

import { useCallback, useEffect, useRef, useState } from 'react';

import type {
  CallSession,
  Calling,
  CallsConnectOptions,
  CfMeetDeviceClient,
  ChatConnectOptions,
  ChatSession,
  IncomingInvitation,
  MeetingJoin,
  MeetingJoinOptions,
  Meetings,
} from '@cfmeet/platform-sdk';

export type CfMeetConnectionStatus = 'idle' | 'connecting' | 'open' | 'reconnecting' | 'closed' | 'error';

// ---------------------------------------------------------------------------------------
// Chat

export interface UseCfMeetChatOptions extends Omit<ChatConnectOptions, 'signal'> {
  /** false: do not connect (e.g. before sign-in). Default true. */
  readonly enabled?: boolean;
  /** Keep at most this many recent realtime events in `events` (default 200). */
  readonly maxEvents?: number;
}

export interface UseCfMeetChat<M> {
  readonly status: CfMeetConnectionStatus;
  readonly session: ChatSession<M> | null;
  /** Recent realtime frames, oldest first. */
  readonly events: readonly M[];
  readonly error: unknown;
  /** Sends a MANAGED text message (throws when not connected). */
  sendMessage(conversationId: string, text: string): Promise<void>;
}

/** Connects chat for the device's user: realtime events, plus REST through `session`. */
export function useCfMeetChat<M = unknown>(device: CfMeetDeviceClient | null, options: UseCfMeetChatOptions = {}): UseCfMeetChat<M> {
  const { enabled = true, maxEvents = 200, installationId, realtime } = options;
  const [status, setStatus] = useState<CfMeetConnectionStatus>('idle');
  const [session, setSession] = useState<ChatSession<M> | null>(null);
  const [events, setEvents] = useState<readonly M[]>([]);
  const [error, setError] = useState<unknown>(null);
  const sessionRef = useRef<ChatSession<M> | null>(null);

  useEffect(() => {
    if (!device || !enabled) {
      setStatus('idle');
      return undefined;
    }
    let cancelled = false;
    const subs: (() => void)[] = [];
    setStatus('connecting');
    setError(null);
    device.chat
      .connect<M>({ installationId, realtime })
      .then((s) => {
        if (cancelled) return s.close();
        sessionRef.current = s;
        setSession(s);
        setStatus(realtime === false ? 'open' : 'connecting');
        subs.push(
          s.on('open', () => setStatus('open')),
          s.on('reconnecting', () => setStatus('reconnecting')),
          s.on('message', ({ data }) => setEvents((prev) => [...prev, data].slice(-maxEvents))),
          s.on('error', ({ error: e }) => setError(e)),
          s.on('closed', (info) => {
            setStatus(info.cause === 'client' ? 'closed' : 'error');
            if (info.error) setError(info.error);
          }),
        );
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e);
        setStatus('error');
      });
    return () => {
      cancelled = true;
      for (const u of subs) u();
      sessionRef.current?.close();
      sessionRef.current = null;
      setSession(null);
    };
  }, [device, enabled, installationId, realtime, maxEvents]);

  const sendMessage = useCallback(async (conversationId: string, text: string) => {
    const s = sessionRef.current;
    if (!s) throw new Error('chat is not connected');
    await s.sendMessage(conversationId, text);
  }, []);

  return { status, session, events, error, sendMessage };
}

// ---------------------------------------------------------------------------------------
// Calls

export interface UseCfMeetCallOptions extends Omit<CallsConnectOptions, 'signal'> {
  readonly enabled?: boolean;
}

export interface UseCfMeetCall {
  readonly status: CfMeetConnectionStatus;
  readonly session: CallSession | null;
  /** The ringing incoming call, cleared when it is answered, declined, cancelled or expires. */
  readonly incoming: IncomingInvitation | null;
  readonly error: unknown;
  /** Answers `incoming`; resolves with the accept result (its `sessionId` → `session.mediaCredential`). */
  accept(): Promise<Calling.AcceptResult>;
  decline(): Promise<void>;
}

const ENDING_SIGNALS = new Set([
  'call.cancelled',
  'call.ring_cancelled',
  'call.answered_elsewhere',
  'call.declined',
  'call.invitation_expired',
  'call.no_answer',
  'call.ended',
  'call.failed',
]);

/** Registers this installation as the user's call endpoint and exposes the ringing call. */
export function useCfMeetCall(device: CfMeetDeviceClient | null, options: UseCfMeetCallOptions = {}): UseCfMeetCall {
  const { enabled = true, installationId, kind, signaling } = options;
  const [status, setStatus] = useState<CfMeetConnectionStatus>('idle');
  const [session, setSession] = useState<CallSession | null>(null);
  const [incoming, setIncoming] = useState<IncomingInvitation | null>(null);
  const [error, setError] = useState<unknown>(null);
  const sessionRef = useRef<CallSession | null>(null);
  const incomingRef = useRef<IncomingInvitation | null>(null);

  const ring = (inv: IncomingInvitation | null) => {
    incomingRef.current = inv;
    setIncoming(inv);
  };

  useEffect(() => {
    if (!device || !enabled) {
      setStatus('idle');
      return undefined;
    }
    let cancelled = false;
    const subs: (() => void)[] = [];
    setStatus('connecting');
    setError(null);
    device.calls
      .connect({ installationId, kind, signaling })
      .then((s) => {
        if (cancelled) return s.close();
        sessionRef.current = s;
        setSession(s);
        setStatus(signaling === false ? 'open' : 'connecting');
        subs.push(
          s.on('open', () => setStatus('open')),
          s.on('reconnecting', () => setStatus('reconnecting')),
          s.on('invitation', ({ invitation }) => {
            ring(invitation);
            s.acknowledge(invitation.invitationId).catch(setError);
          }),
          s.on('signal', ({ signal }) => {
            if (ENDING_SIGNALS.has(signal.type) && incomingRef.current?.callId === signal.callId) ring(null);
          }),
          s.on('error', ({ error: e }) => setError(e)),
          s.on('closed', (info) => {
            setStatus(info.cause === 'client' ? 'closed' : 'error');
            if (info.error) setError(info.error);
          }),
        );
        // Offers that arrived while the app was not connected (e.g. it was woken by a push).
        return s.recoverInvitations().then((live) => {
          if (!cancelled && live[0] && !incomingRef.current) ring(live[0]);
        });
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e);
        setStatus('error');
      });
    return () => {
      cancelled = true;
      for (const u of subs) u();
      sessionRef.current?.close();
      sessionRef.current = null;
      setSession(null);
      ring(null);
    };
  }, [device, enabled, installationId, kind, signaling]);

  const accept = useCallback(async () => {
    const s = sessionRef.current;
    const inv = incomingRef.current;
    if (!s || !inv) throw new Error('no incoming call');
    const res = await s.accept(inv);
    ring(null);
    return res.data;
  }, []);

  const decline = useCallback(async () => {
    const s = sessionRef.current;
    const inv = incomingRef.current;
    if (!s || !inv) return;
    ring(null);
    await s.decline(inv);
  }, []);

  return { status, session, incoming, error, accept, decline };
}

// ---------------------------------------------------------------------------------------
// Meetings

export interface UseCfMeetMeeting {
  readonly status: 'idle' | 'joining' | 'joined' | 'error';
  readonly joined: MeetingJoin | null;
  /** Hand to LiveKit's client SDK: `room.connect(rtc.url, rtc.credential)` (e.g. @livekit/react-native). */
  readonly rtc: Meetings.RtcCredentialResponse | null;
  readonly error: unknown;
  /** Joins again (e.g. after the RTC credential expired or the lobby admitted the user). */
  rejoin(): void;
}

/** Joins `conferenceId` with the user token (the token must grant it: `grt.mtg`). */
export function useCfMeetMeeting(
  device: CfMeetDeviceClient | null,
  conferenceId: string | null,
  options: Omit<MeetingJoinOptions, 'signal'> = {},
): UseCfMeetMeeting {
  const [status, setStatus] = useState<UseCfMeetMeeting['status']>('idle');
  const [joined, setJoined] = useState<MeetingJoin | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  const { installationId, previousSessionId } = options;

  useEffect(() => {
    if (!device || !conferenceId) {
      setStatus('idle');
      setJoined(null);
      return undefined;
    }
    const controller = new AbortController();
    setStatus('joining');
    setError(null);
    device.meetings
      .join(conferenceId, { installationId, previousSessionId, signal: controller.signal })
      .then((j) => {
        if (controller.signal.aborted) return;
        setJoined(j);
        setStatus('joined');
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setError(e);
        setStatus('error');
      });
    return () => controller.abort();
  }, [device, conferenceId, installationId, previousSessionId, attempt]);

  const rejoin = useCallback(() => setAttempt((n) => n + 1), []);
  return { status, joined, rtc: joined?.rtc ?? null, error, rejoin };
}

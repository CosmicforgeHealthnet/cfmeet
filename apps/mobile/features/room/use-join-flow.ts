/**
 * Pre-join → join → waiting room → live (MTG-002, FE-004, FE-005).
 *
 * - `GET /conferences/{id}/pre-join` describes access (passcode, consents, display name,
 *   expected waiting room) and the conference status. While the conference is not LIVE the
 *   screen re-reads pre-join (a host may start it with `POST /start`); join is never attempted
 *   on a conference that is not live.
 * - `POST /conferences/{id}/join` exactly once per user action. A guest's `participantToken`
 *   is stored (SecureStore, bound to this conference) before anything else, so every later call
 *   identifies the same participant (`X-Participant-Token`).
 * - WAITING: poll the read-only `GET /participants/me` with backoff; after ADMITTED, one more
 *   `POST /join` returns the RTC credential for the same participant.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { meetingsApi } from "@/features/meetings/api";
import { useLiveSession } from "@/features/room/live-session";
import { decodeApiError } from "@/lib/api/errors";
import { participantCredentialStore } from "@/lib/storage/participant-credential";
import { tokenStorage } from "@/lib/storage/secure-store";
import { classifyJoin, pollAdmission, type LiveMedia } from "@/lib/meeting/admission";
import type { ConsentGrant, JoinConferenceRequest, PreJoinResponse } from "@/lib/api/generated/meeting-service";

export type JoinPhase =
  | { kind: "form" }
  | { kind: "joining" }
  | { kind: "waiting"; attempt: number; retrying: boolean }
  | { kind: "admitted_connecting" }
  | { kind: "live"; media: LiveMedia }
  | { kind: "refused"; reason: string }
  | { kind: "error"; message: string };

export interface JoinInput {
  displayName: string;
  passcode?: string;
  invitationToken?: string;
  consents: ConsentGrant[];
}

export const PRE_JOIN_KEY = (id: string) => ["conference", id, "pre-join"] as const;

export function useJoinFlow(conferenceId: string, meta: { title: string | null; meetingCode: string | null }) {
  const queryClient = useQueryClient();
  const setLive = useLiveSession((s) => s.setLive);
  const [phase, setPhase] = useState<JoinPhase>({ kind: "form" });
  const aborted = useRef(false);
  const lastRequest = useRef<JoinConferenceRequest | null>(null);
  const sleeper = useRef<{ timer: ReturnType<typeof setTimeout> | null; wake: (() => void) | null }>({ timer: null, wake: null });

  const preJoin = useQuery({
    queryKey: PRE_JOIN_KEY(conferenceId),
    queryFn: () => meetingsApi.preJoin(conferenceId),
    enabled: !!conferenceId,
    // Re-read while the meeting has not started yet (host start, schedule).
    refetchInterval: (q) => {
      const data = q.state.data as PreJoinResponse | undefined;
      return data && data.conferenceStatus !== "LIVE" && !["ENDED", "CANCELLED", "FAILED"].includes(data.conferenceStatus)
        ? 5000
        : false;
    },
  });

  useEffect(() => {
    aborted.current = false;
    return () => {
      aborted.current = true;
      const s = sleeper.current;
      if (s.timer) clearTimeout(s.timer);
      s.wake?.();
    };
  }, []);

  const sleep = useCallback(
    (ms: number) =>
      new Promise<void>((resolve) => {
        const s = sleeper.current;
        s.wake = resolve;
        s.timer = setTimeout(() => {
          s.timer = null;
          s.wake = null;
          resolve();
        }, ms);
      }),
    []
  );

  const goLive = useCallback(
    async (media: LiveMedia) => {
      const isGuest = !(await tokenStorage.getAccessToken());
      setLive({
        media,
        spaceRole: preJoin.data?.media.role ?? null,
        title: meta.title ?? preJoin.data?.title ?? null,
        meetingCode: meta.meetingCode,
        isGuest,
      });
      setPhase({ kind: "live", media });
    },
    [setLive, preJoin.data, meta.title, meta.meetingCode]
  );

  /** One `POST /join`; keeps a guest credential first. */
  const postJoin = useCallback(
    async (body: JoinConferenceRequest) => {
      const response = await meetingsApi.join(conferenceId, body);
      if (response.participantToken && !(await tokenStorage.getAccessToken())) {
        await participantCredentialStore.set(conferenceId, response.participantToken);
      }
      return response;
    },
    [conferenceId]
  );

  const waitForAdmission = useCallback(async () => {
    setPhase({ kind: "waiting", attempt: 0, retrying: false });
    const result = await pollAdmission({
      fetchMe: () => meetingsApi.me(conferenceId),
      sleep,
      statusOf: (e) => decodeApiError(e).status,
      isAborted: () => aborted.current,
      onPoll: ({ attempt, consecutiveFailures }) =>
        !aborted.current && setPhase({ kind: "waiting", attempt, retrying: consecutiveFailures > 0 }),
    });
    if (aborted.current || result.kind === "aborted") return;
    if (result.kind === "refused") {
      await participantCredentialStore.clear();
      setPhase({ kind: "refused", reason: result.reason });
      return;
    }
    if (result.kind === "failed") {
      setPhase({
        kind: "error",
        message:
          result.status === 404
            ? "Your place in the waiting room was not found. Join again."
            : "You can no longer wait for this meeting. Check the meeting status and join again.",
      });
      return;
    }
    // Admitted: exactly one join for the media credential of the same participant.
    setPhase({ kind: "admitted_connecting" });
    try {
      const outcome = classifyJoin(await postJoin(lastRequest.current ?? {}));
      if (aborted.current) return;
      if (outcome.kind === "live") await goLive(outcome.media);
      else if (outcome.kind === "refused") setPhase({ kind: "refused", reason: outcome.reason });
      else if (outcome.kind === "waiting") setPhase({ kind: "error", message: "The host has not admitted you yet. Join again." });
      else setPhase({ kind: "error", message: outcome.reason });
    } catch (error) {
      setPhase({ kind: "error", message: decodeApiError(error).message });
    }
  }, [conferenceId, sleep, postJoin, goLive]);

  const join = useCallback(
    async (input: JoinInput) => {
      setPhase({ kind: "joining" });
      const body: JoinConferenceRequest = {
        displayName: input.displayName.trim() || undefined,
        passcode: input.passcode?.trim() || undefined,
        invitationToken: input.invitationToken || undefined,
        consents: input.consents.length ? input.consents : undefined,
      };
      lastRequest.current = body;
      try {
        const outcome = classifyJoin(await postJoin(body));
        if (outcome.kind === "live") await goLive(outcome.media);
        else if (outcome.kind === "waiting") await waitForAdmission();
        else if (outcome.kind === "refused") {
          await participantCredentialStore.clear();
          setPhase({ kind: "refused", reason: outcome.reason });
        } else setPhase({ kind: "error", message: outcome.reason });
      } catch (error) {
        const decoded = decodeApiError(error);
        if (decoded.code === "CONFERENCE_NOT_LIVE") {
          void queryClient.invalidateQueries({ queryKey: PRE_JOIN_KEY(conferenceId) });
        }
        setPhase(
          decoded.code === "CONFERENCE_NOT_LIVE"
            ? { kind: "error", message: "This meeting has not started yet. Wait for the host." }
            : { kind: "error", message: decoded.message }
        );
      }
    },
    [postJoin, goLive, waitForAdmission, queryClient, conferenceId]
  );

  /** Host/owner: start a conference that is not live yet, then refresh pre-join. */
  const start = useCallback(async () => {
    try {
      await meetingsApi.startConference(conferenceId);
      await queryClient.invalidateQueries({ queryKey: PRE_JOIN_KEY(conferenceId) });
      setPhase({ kind: "form" });
    } catch (error) {
      setPhase({ kind: "error", message: decodeApiError(error).message });
    }
  }, [conferenceId, queryClient]);

  /** Leaves the waiting room (server-side too) and forgets the guest credential. */
  const cancelWaiting = useCallback(async () => {
    aborted.current = true;
    const s = sleeper.current;
    if (s.timer) clearTimeout(s.timer);
    s.wake?.();
    try {
      await meetingsApi.leave(conferenceId, {});
    } catch {
      // The waiting entry expires server-side; the local credential is dropped regardless.
    }
    await participantCredentialStore.clear();
    aborted.current = false;
    setPhase({ kind: "form" });
  }, [conferenceId]);

  const reset = useCallback(() => setPhase({ kind: "form" }), []);

  return { preJoin, phase, join, start, cancelWaiting, reset };
}

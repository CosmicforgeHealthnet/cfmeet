"use client";

/**
 * CallRecordingControls — recording notice, the participant's own consent, start / stop (CAL-005).
 *
 * Every request goes through the adapter, bound to the signed-in participant: consent is taken
 * through the user's own endpoint with a session in this Call; start / stop act for the
 * participant (`principalId`), never as a recording administrator. Calling decides whether a
 * recording may run (policy, notice, all-party consent) and records that decision; this UI
 * shows the decision and its reason, and never claims a recording that Calling did not report.
 */

import { useState } from "react";
import { Circle, Square, ShieldCheck, ShieldX } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useCallRecordings, useRecordConsent, useRecordingAuthorizations, useStartRecording, useStopRecording } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type { ConsentDecision, RecordingAuthorizationReason } from "@/types/calling";

const ACTIVE_STATES = new Set(["STARTING", "RECORDING", "STOPPING"]);

const reasonText: Record<RecordingAuthorizationReason, string> = {
  AUTHORIZED: "Recording was authorized.",
  RECORDING_DISABLED: "Recording is turned off for this service.",
  NOT_AN_ENTERPRISE_CALL: "Only organization (enterprise) calls can be recorded.",
  NO_RECORDING_POLICY: "No recording policy applies to this call.",
  POLICY_DISABLED: "The recording policy for this call is disabled.",
  AUTO_RECORDING_NOT_CONFIGURED: "Automatic recording is not configured.",
  CALL_STATE_NOT_ELIGIBLE: "The call cannot be recorded in its current state.",
  ACTOR_NOT_PARTICIPANT: "Only participants can start a recording.",
  NOTICE_UNDELIVERABLE: "Not every participant could be notified, so recording did not start.",
  CONSENT_MISSING: "Every required participant must allow recording first.",
  CONSENT_DECLINED: "A participant did not allow recording.",
  ARTIFACT_STORAGE_UNAVAILABLE: "Recording storage is unavailable right now.",
  CAPTURE_UNAVAILABLE: "Recording capture is unavailable right now.",
};

export default function CallRecordingControls({ callId, participantId, endpointId, live }: {
  callId: string;
  /** The signed-in user's active participant in this Call. */
  participantId: string;
  /** This browser's endpoint when it has a session in the Call (consent is per endpoint). */
  endpointId: string | null;
  live: boolean;
}) {
  const [error, setError] = useState<string | null>(null);
  const authorizations = useRecordingAuthorizations(callId);
  const recordings = useCallRecordings(callId);
  const consent = useRecordConsent(callId);
  const start = useStartRecording(callId);
  const stop = useStopRecording(callId);

  const latest = authorizations.data?.authorizations.at(-1);
  const active = recordings.data?.recordings.filter((r) => ACTIVE_STATES.has(r.state)) ?? [];
  const myDecision: ConsentDecision | null = latest?.consent.participants?.find((p) => p.participantId === participantId)?.decision ?? null;
  const consentRequired = !!latest?.consent.required && latest.requiredParticipantIds.includes(participantId);
  const run = (action: () => Promise<unknown>) => { setError(null); action().catch((e) => setError(callingErrorMessage(e))); };
  const decide = (decision: ConsentDecision) => {
    if (!endpointId) { setError("Join the call on this device to answer the recording request."); return; }
    run(() => consent.mutateAsync({ endpointId, decision }));
  };

  return (
    <section aria-labelledby={`recording-${callId}`} className="space-y-3 rounded-xl border border-border bg-background p-4">
      <h3 id={`recording-${callId}`} className="text-sm font-semibold">Recording</h3>
      {active.length > 0 ? (
        <p role="status" aria-live="assertive" className="flex items-center gap-2 rounded-lg bg-destructive/10 p-2 text-sm font-medium text-destructive">
          <Circle className="size-3 animate-pulse fill-current" /> This call is being recorded.
        </p>
      ) : null}
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {authorizations.error && recordings.error ? <p className="text-sm text-muted-foreground">{callingErrorMessage(authorizations.error)}</p> : null}
      {latest && latest.decision === "DENIED" ? <p className="text-sm text-muted-foreground">{reasonText[latest.reason] ?? "Recording is not available for this call."}</p> : null}
      {live ? (
        <div className="space-y-2">
          <p className="text-sm text-muted-foreground">
            {myDecision === "GRANTED" ? "You allowed recording of this call." : myDecision === "DECLINED" ? "You did not allow recording of this call." : consentRequired ? "Your consent is required before this call can be recorded." : "You can decide whether this call may be recorded."}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant={myDecision === "GRANTED" ? "default" : "outline"} disabled={consent.isPending} onClick={() => decide("GRANTED")}><ShieldCheck className="size-4" /> Allow recording</Button>
            <Button size="sm" variant={myDecision === "DECLINED" ? "default" : "outline"} disabled={consent.isPending} onClick={() => decide("DECLINED")}><ShieldX className="size-4" /> Don&apos;t allow{active.length > 0 ? " (stops it)" : ""}</Button>
            {active.length === 0 ? (
              <Button size="sm" variant="outline" disabled={start.isPending} onClick={() => run(() => start.mutateAsync(newIdempotencyKey()))}><Circle className="size-4" /> Start recording</Button>
            ) : active.map((recording) => (
              <Button key={recording.recordingId} size="sm" variant="destructive" disabled={stop.isPending || recording.state === "STOPPING"} onClick={() => run(() => stop.mutateAsync(recording.recordingId))}><Square className="size-4" /> {recording.state === "STOPPING" ? "Stopping…" : "Stop recording"}</Button>
            ))}
          </div>
        </div>
      ) : null}
      {recordings.data && recordings.data.recordings.length > 0 ? (
        <ul className="space-y-1 text-xs text-muted-foreground">
          {recordings.data.recordings.map((r) => <li key={r.recordingId}>Recording {new Date(r.startedAt).toLocaleTimeString()} · {r.state.toLowerCase()}{r.stopReason ? ` (${r.stopReason.replaceAll("_", " ").toLowerCase()})` : ""}</li>)}
        </ul>
      ) : null}
    </section>
  );
}

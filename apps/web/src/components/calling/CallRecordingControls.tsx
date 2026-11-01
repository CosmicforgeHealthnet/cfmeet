"use client";

/**
 * CallRecordingControls - Recording consent and controls during calls.
 */

import { useState } from "react";
import { Circle, Square, CheckCircle, XCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import {
  useRecordingAuthorizations,
  useCallRecordings,
  useRecordConsent,
  useStartRecording,
  useStopRecording,
} from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { RecordingConsentDecision } from "@/types/calling";

interface CallRecordingControlsProps {
  callId: string;
  isHost: boolean;
  participantId?: string;
  className?: string;
}

export default function CallRecordingControls({
  callId,
  isHost,
  participantId,
  className,
}: CallRecordingControlsProps) {
  const [error, setError] = useState<string | null>(null);

  const authorizations = useRecordingAuthorizations(callId);
  const recordings = useCallRecordings(callId);
  const recordConsent = useRecordConsent(callId);
  const startRecording = useStartRecording(callId);
  const stopRecording = useStopRecording(callId);

  const activeRecording = recordings.data?.data.find(
    (r) => r.state === "RECORDING" || r.state === "PENDING"
  );
  const isRecording = !!activeRecording;

  const myAuthorization = authorizations.data?.data.find(
    (a) => a.participantId === participantId
  );
  const needsConsent = isRecording && !myAuthorization;

  const handleConsent = async (decision: RecordingConsentDecision) => {
    setError(null);
    try {
      await recordConsent.mutateAsync({ decision, participantId });
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const handleStartRecording = async () => {
    setError(null);
    try {
      await startRecording.mutateAsync({});
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const handleStopRecording = async () => {
    if (!activeRecording) return;
    setError(null);
    try {
      await stopRecording.mutateAsync(activeRecording.recordingId);
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  return (
    <div className={className}>
      {error && (
        <p className="mb-3 rounded-lg border border-destructive/30 bg-destructive/5 p-2 text-sm text-destructive">
          {error}
        </p>
      )}

      {/* Recording indicator */}
      {isRecording && (
        <div className="mb-3 flex items-center gap-2 rounded-lg bg-destructive/10 px-3 py-2">
          <span className="relative flex size-3">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-destructive opacity-75" />
            <span className="relative inline-flex size-3 rounded-full bg-destructive" />
          </span>
          <span className="text-sm font-medium text-destructive">Recording</span>
          {activeRecording?.startedAt && (
            <span className="text-xs text-destructive/70">
              Started {new Date(activeRecording.startedAt).toLocaleTimeString()}
            </span>
          )}
        </div>
      )}

      {/* Consent prompt */}
      {needsConsent && (
        <div className="mb-3 space-y-2 rounded-xl border border-primary bg-primary/5 p-4">
          <p className="font-medium">This call is being recorded</p>
          <p className="text-sm text-muted-foreground">
            Please confirm your consent to continue with the recording.
          </p>
          <div className="flex gap-2">
            <Button
              size="sm"
              onClick={() => handleConsent("ACCEPTED")}
              disabled={recordConsent.isPending}
            >
              {recordConsent.isPending ? <Spinner className="mr-2 size-4" /> : <CheckCircle className="mr-1 size-4" />}
              Accept
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => handleConsent("DECLINED")}
              disabled={recordConsent.isPending}
            >
              <XCircle className="mr-1 size-4" />
              Decline
            </Button>
          </div>
        </div>
      )}

      {/* Host recording controls */}
      {isHost && (
        <div className="flex items-center gap-2">
          {isRecording ? (
            <Button
              size="sm"
              variant="destructive"
              onClick={handleStopRecording}
              disabled={stopRecording.isPending}
            >
              {stopRecording.isPending ? <Spinner className="mr-2 size-4" /> : <Square className="mr-1 size-4" />}
              Stop Recording
            </Button>
          ) : (
            <Button
              size="sm"
              variant="outline"
              onClick={handleStartRecording}
              disabled={startRecording.isPending}
            >
              {startRecording.isPending ? <Spinner className="mr-2 size-4" /> : <Circle className="mr-1 size-4 text-destructive" />}
              Start Recording
            </Button>
          )}
        </div>
      )}

      {/* Consent status */}
      {isRecording && authorizations.data && authorizations.data.data.length > 0 && (
        <div className="mt-3">
          <p className="mb-2 text-xs font-medium text-muted-foreground">Consent Status</p>
          <div className="space-y-1">
            {authorizations.data.data.map((auth) => (
              <div
                key={auth.participantId}
                className="flex items-center justify-between text-sm"
              >
                <span>{auth.displayName || "Participant"}</span>
                <span
                  className={`flex items-center gap-1 ${
                    auth.decision === "ACCEPTED"
                      ? "text-green-600"
                      : auth.decision === "DECLINED"
                      ? "text-destructive"
                      : "text-muted-foreground"
                  }`}
                >
                  {auth.decision === "ACCEPTED" && <CheckCircle className="size-4" />}
                  {auth.decision === "DECLINED" && <XCircle className="size-4" />}
                  {auth.decision}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

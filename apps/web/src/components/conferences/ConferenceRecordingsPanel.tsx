"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useRecordings, useStartRecording, useStopRecording } from "@/hooks/v2/useArtifacts";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import type { ArtifactStatus, RecordingResponse, Role, RolePolicy } from "@/types/v2";

const activeRecordingStatuses: ArtifactStatus[] = ["REQUESTED", "RECORDING", "STOPPING", "PROCESSING"];

function recordingStatusMessage(result: RecordingResponse) {
  if (!result.created) return "This recording request was already processed.";
  if (result.providerDispatch === "PENDING_RETRY") {
    return "Recording request saved. The provider start command is queued for retry.";
  }
  if (result.providerDispatch === "NOT_REQUIRED") {
    return "No provider action was needed.";
  }
  return "Recording requested. It will show as recording after provider confirmation.";
}

export default function ConferenceRecordingsPanel({
  conferenceId,
  spaceRole,
  rolePolicy,
  recordingPolicy,
  live,
}: {
  conferenceId: string;
  spaceRole?: Role;
  rolePolicy?: RolePolicy;
  recordingPolicy?: string;
  live: boolean;
}) {
  const [layout, setLayout] = useState("GRID");
  const [feedback, setFeedback] = useState("");
  const me = useCurrentParticipant(conferenceId);
  const role = effectiveConferenceRole(spaceRole, me.data?.role, me.data?.admissionState);
  const recordingEnabled = recordingPolicy !== "DISABLED";
  const canStart = recordingEnabled && meetingAllows(role, "recording.start", rolePolicy);
  const canStop = recordingEnabled && meetingAllows(role, "recording.stop", rolePolicy);
  const canList = meetingAllows(role, "artifact.list", rolePolicy) || canStart || canStop;
  const recordings = useRecordings(conferenceId, canList);
  const start = useStartRecording(conferenceId);
  const stop = useStopRecording(conferenceId);
  const activeRecording = recordings.data?.recordings.find(item =>
    activeRecordingStatuses.includes(item.status),
  );
  const canStopActive = !!activeRecording &&
    ["REQUESTED", "RECORDING"].includes(activeRecording.status) &&
    canStop;

  const startRecording = async () => {
    setFeedback("");
    try {
      const result = await start.mutateAsync({ layout, idempotencyKey: crypto.randomUUID() });
      setFeedback(recordingStatusMessage(result));
    } catch {
      setFeedback("");
    }
  };

  const stopRecording = async () => {
    if (!activeRecording) return;
    setFeedback("");
    try {
      const result = await stop.mutateAsync(activeRecording.id);
      setFeedback(
        result.recording.status === "READY"
          ? "Recording is ready."
          : "Recording stop requested. Finalization and storage verification are still in progress.",
      );
    } catch {
      setFeedback("");
    }
  };

  if (!canList && !recordings.isLoading && !recordings.isError) return null;

  return (
    <section className="space-y-3 rounded-xl border border-border bg-card p-4">
      <header>
        <h2 className="font-semibold">Recordings</h2>
        <p className="text-sm text-muted-foreground">
          Recording status changes only after the provider confirms capture and finalization.
        </p>
      </header>

      {me.isError && (me.error as { response?: { status?: number } })?.response?.status !== 404 && (
        <RequestError error={me.error} retry={() => void me.refetch()} />
      )}
      {recordings.isLoading && <p role="status" className="text-sm">Loading recordings…</p>}
      {recordings.isError && <RequestError error={recordings.error} retry={() => void recordings.refetch()} />}

      {feedback && <p role="status" className="rounded-md bg-muted p-3 text-sm">{feedback}</p>}
      {start.isError && <RequestError error={start.error} />}
      {stop.isError && <RequestError error={stop.error} />}

      {(canStart || canStop) && (
        <div className="flex flex-wrap items-end gap-2">
          {canStart && !activeRecording && (
            <>
              <label className="grid gap-1 text-sm font-medium">
                Layout
                <select
                  className="h-9 rounded-md border border-input bg-background px-2"
                  value={layout}
                  onChange={event => setLayout(event.target.value)}
                >
                  <option value="GRID">Grid</option>
                  <option value="SPEAKER">Speaker</option>
                </select>
              </label>
              <Button
                type="button"
                disabled={!live || start.isPending || recordings.isLoading}
                onClick={() => void startRecording()}
              >
                {start.isPending ? "Requesting…" : "Start recording"}
              </Button>
              {!live && <p className="text-xs text-muted-foreground">The conference must be live to start recording.</p>}
            </>
          )}
          {canStopActive && (
            <Button
              type="button"
              variant="destructive"
              disabled={stop.isPending}
              onClick={() => void stopRecording()}
            >
              {stop.isPending ? "Stopping…" : "Stop recording"}
            </Button>
          )}
        </div>
      )}

      {canList && recordings.data && (
        <div className="space-y-2">
          {recordings.data.recordings.length === 0 && (
            <p className="text-sm text-muted-foreground">No recordings yet.</p>
          )}
          {recordings.data.recordings.map(recording => (
            <article key={recording.id} className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border p-3">
              <div>
                <p className="text-sm font-medium">{recording.filename || "Conference recording"}</p>
                <p className="text-xs text-muted-foreground">
                  {recording.status.replaceAll("_", " ")} · {new Date(recording.createdAt).toLocaleString()}
                  {recording.sizeBytes != null ? ` · ${Math.round(recording.sizeBytes / 1024)} KB` : ""}
                </p>
              </div>
            </article>
          ))}
        </div>
      )}
    </section>
  );
}

"use client";

import { NiceSelect } from "@/components/ui/nice-select";
import { Circle, Film, LayoutGrid, Square, User } from "lucide-react";
import { artifactDisplayName, artifactStateLabel, formatBytes, formatDurationMs } from "@/lib/artifact-display";
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
    <section className="space-y-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-base font-semibold text-foreground">Recordings</h2>
        {live && activeRecording && (
          <span className="inline-flex items-center gap-1.5 rounded-full bg-destructive/10 px-2.5 py-1 text-xs font-semibold text-destructive">
            <span className="size-1.5 animate-pulse rounded-full bg-destructive" /> Recording
          </span>
        )}
      </div>

      {me.isError && (me.error as { response?: { status?: number } })?.response?.status !== 404 && (
        <RequestError error={me.error} retry={() => void me.refetch()} />
      )}
      {recordings.isLoading && <div aria-busy="true" className="h-16 animate-pulse rounded-xl bg-muted" />}
      {recordings.isError && <RequestError error={recordings.error} retry={() => void recordings.refetch()} />}

      {feedback && <p role="status" className="rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground">{feedback}</p>}
      {start.isError && <RequestError error={start.error} />}
      {stop.isError && <RequestError error={stop.error} />}

      {/* Recording controls only while the session is live. */}
      {live && (canStart || canStop) && (
        <div className="flex flex-wrap items-end gap-2 rounded-xl border border-border p-4">
          {canStart && !activeRecording && (
            <>
              <NiceSelect
                label="Layout"
                className="w-40"
                value={layout}
                onChange={setLayout}
                options={[
                  { value: "GRID", label: "Grid", icon: LayoutGrid },
                  { value: "SPEAKER", label: "Speaker", icon: User },
                ]}
              />
              <Button type="button" className="h-11 rounded-xl shadow-none" disabled={start.isPending || recordings.isLoading} onClick={() => void startRecording()}>
                <Circle className="size-3.5 fill-current" /> {start.isPending ? "Starting…" : "Start recording"}
              </Button>
            </>
          )}
          {canStopActive && (
            <Button type="button" variant="destructive" className="h-11 rounded-xl shadow-none" disabled={stop.isPending} onClick={() => void stopRecording()}>
              <Square className="size-3.5 fill-current" /> {stop.isPending ? "Stopping…" : "Stop recording"}
            </Button>
          )}
        </div>
      )}

      {canList && recordings.data && (
        recordings.data.recordings.length === 0 ? (
          <div className="flex flex-col items-center py-8 text-center">
            <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Film className="size-5" /></span>
            <p className="mt-3 text-sm font-medium text-foreground">No recordings</p>
            <p className="text-sm text-muted-foreground">{live ? "Start a recording and it will appear here." : "This session wasn't recorded."}</p>
          </div>
        ) : (
          <ul className="space-y-2">
            {recordings.data.recordings.map(recording => (
              <li key={recording.id} className="flex items-center gap-3 rounded-xl border border-border p-3">
                <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary"><Film className="size-4" /></span>
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium text-foreground">{artifactDisplayName(recording)}</p>
                  <p className="text-xs text-muted-foreground">
                    {artifactStateLabel(recording)}
                    {formatDurationMs(recording.durationMs) ? ` · ${formatDurationMs(recording.durationMs)}` : ""}
                    {formatBytes(recording.sizeBytes) ? ` · ${formatBytes(recording.sizeBytes)}` : ""}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        )
      )}
    </section>
  );
}

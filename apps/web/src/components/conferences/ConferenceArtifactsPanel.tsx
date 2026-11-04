"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import {
  useArtifact,
  useArtifactAccess,
  useArtifacts,
  useAttendance,
  useConsentRequirements,
  useDeleteArtifact,
  useGenerateReport,
  useRecordConsents,
  useRequestIntelligence,
  useStartTranscription,
  useStopTranscription,
  useTranscripts,
  useTranscript,
  useTranslateTranscript,
  useWithdrawConsent,
} from "@/hooks/v2/useArtifacts";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import type {
  ArtifactType,
  GenerateReportRequest,
  ConsentKind,
  ProfilePolicy,
  Role,
  TranscriptQuery,
  WithdrawalResponse,
} from "@/types/v2";

type ReportType = GenerateReportRequest["artifactType"];

const reportOptions: { value: ReportType; label: string }[] = [
  { value: "ATTENDANCE_REPORT", label: "Attendance report" },
  { value: "POLL_REPORT", label: "Poll report" },
  { value: "CHAT_EXPORT", label: "Chat export" },
  { value: "INTELLIGENCE_OUTPUT", label: "Intelligence output" },
];

const consentLabels: Record<ConsentKind, string> = {
  RECORDING: "Recording",
  TRANSCRIPTION: "Transcription",
  AI_PROCESSING: "AI processing",
  DATA_SHARING: "Data sharing",
  HEALTHCARE_TERMS: "Healthcare terms",
  PRIVACY_NOTICE: "Privacy notice",
};

function withdrawalDescription(result: WithdrawalResponse) {
  if (result.effect === "PROCESSING_STOPPED") {
    return "Consent withdrawn. Processing that depended on it was stopped; existing artifacts are not deleted.";
  }
  if (result.effect === "PARTICIPANT_EXITED") {
    return "Consent withdrawn. The meeting policy returned you to the waiting room; existing artifacts are not deleted.";
  }
  return "Consent withdrawal recorded. Existing artifacts are not deleted.";
}

export default function ConferenceArtifactsPanel({
  conferenceId,
  spaceRole,
  rolePolicy,
  recordingPolicy,
  live,
}: {
  conferenceId: string;
  spaceRole?: Role;
  rolePolicy?: ProfilePolicy extends never ? never : import("@/types/v2").RolePolicy;
  recordingPolicy?: string;
  live: boolean;
}) {
  const [reportType, setReportType] = useState<ReportType>("ATTENDANCE_REPORT");
  const [feedback, setFeedback] = useState("");
  const [attendanceVisible, setAttendanceVisible] = useState(false);
  const [selectedArtifactId, setSelectedArtifactId] = useState("");
  const me = useCurrentParticipant(conferenceId);
  const role = effectiveConferenceRole(spaceRole, me.data?.role, me.data?.admissionState);
  const canList = meetingAllows(role, "artifact.list", rolePolicy) ||
    meetingAllows(role, "artifact.read", rolePolicy) ||
    meetingAllows(role, "recording.start", rolePolicy);
  const canRead = meetingAllows(role, "artifact.read", rolePolicy);
  const canDelete = meetingAllows(role, "artifact.delete", rolePolicy);
  const canExport = meetingAllows(role, "artifact.export", rolePolicy);
  const canReadTranscripts = meetingAllows(role, "transcript.read", rolePolicy);
  const canRequestIntelligence = meetingAllows(role, "intelligence.request", rolePolicy);
  const canUseOwnConsent = me.data?.admissionState === "ADMITTED";
  const artifacts = useArtifacts(conferenceId, undefined, canList);
  const attendance = useAttendance(conferenceId, attendanceVisible && canExport);
  const selectedArtifact = useArtifact(conferenceId, selectedArtifactId);
  const generate = useGenerateReport(conferenceId);
  const access = useArtifactAccess(conferenceId);
  const remove = useDeleteArtifact(conferenceId);
  const items = artifacts.data?.artifacts ?? [];

  const generateReport = async () => {
    setFeedback("");
    try {
      const artifact = await generate.mutateAsync({ artifactType: reportType });
      setFeedback(`${reportType.replaceAll("_", " ")} generated (${artifact.status}).`);
    } catch {
      setFeedback("");
    }
  };

  const mintAccess = async (artifactId: string) => {
    setFeedback("");
    try {
      const grant = await access.mutateAsync(artifactId);
      setFeedback(`Access link expires ${new Date(grant.expiresAt).toLocaleString()}.`);
      setSelectedArtifactId("");
      setAccessUrl(grant.url);
    } catch {
      setFeedback("");
      setAccessUrl("");
    }
  };

  const [accessUrl, setAccessUrl] = useState("");

  return (
    <div className="space-y-4">
      <section className="space-y-3 rounded-xl border border-border bg-card p-4">
        <header>
          <h2 className="font-semibold">Artifacts and reports</h2>
          <p className="text-sm text-muted-foreground">
            Downloads use short-lived, read-only links. Artifact status reflects confirmed storage state.
          </p>
        </header>
        {artifacts.isLoading && <p role="status" className="text-sm">Loading artifacts…</p>}
        {artifacts.isError && <RequestError error={artifacts.error} retry={() => void artifacts.refetch()} />}
        {feedback && <p role="status" className="rounded-md bg-muted p-3 text-sm">{feedback}</p>}
        {generate.isError && <RequestError error={generate.error} />}
        {access.isError && <RequestError error={access.error} />}
        {remove.isError && <RequestError error={remove.error} />}
        {accessUrl && (
          <a className="inline-flex text-sm text-primary underline" href={accessUrl} target="_blank" rel="noopener noreferrer">
            Open secure artifact link
          </a>
        )}

        {canExport && (
          <div className="flex flex-wrap items-end gap-2">
            <label className="grid gap-1 text-sm font-medium">
              Generate report
              <select
                className="h-9 rounded-md border border-input bg-background px-2"
                value={reportType}
                onChange={event => setReportType(event.target.value as ReportType)}
              >
                {reportOptions.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}
              </select>
            </label>
            <Button type="button" disabled={generate.isPending} onClick={() => void generateReport()}>
              {generate.isPending ? "Generating…" : "Generate"}
            </Button>
          </div>
        )}

        {canExport && (
          <div className="space-y-2 border-t border-border pt-3">
            <Button type="button" variant="outline" size="sm" onClick={() => setAttendanceVisible(value => !value)}>
              {attendanceVisible ? "Hide attendance" : "View attendance"}
            </Button>
            {attendanceVisible && (
              <>
                {attendance.isLoading && <p role="status" className="text-sm">Loading attendance…</p>}
                {attendance.isError && <RequestError error={attendance.error} retry={() => void attendance.refetch()} />}
                {attendance.data && (
                  <div className="space-y-2">
                    <p className="text-xs text-muted-foreground">
                      {attendance.data.participants.length} participants · {attendance.data.sessionCount} sessions · generated {new Date(attendance.data.generatedAt).toLocaleString()}
                    </p>
                    {attendance.data.participants.map(person => (
                      <div key={person.participantId} className="flex flex-wrap justify-between gap-2 rounded-md border border-border p-2 text-sm">
                        <span>{person.displayName} · {person.role.replaceAll("_", " ")}</span>
                        <span className="text-muted-foreground">{person.admissionState} · {Math.floor(person.attendanceSeconds / 60)} min</span>
                      </div>
                    ))}
                    {attendance.data.participants.length === 0 && <p className="text-sm text-muted-foreground">No participants recorded.</p>}
                  </div>
                )}
              </>
            )}
          </div>
        )}

        {canList && (
          <div className="space-y-2 border-t border-border pt-3">
            {items.length === 0 && !artifacts.isLoading && <p className="text-sm text-muted-foreground">No artifacts yet.</p>}
            {items.map(item => (
              <article key={item.id} className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border p-3">
                <div className="min-w-0">
                  <p className="break-all text-sm font-medium">{item.filename || item.artifactType.replaceAll("_", " ")}</p>
                  <p className="text-xs text-muted-foreground">
                    {item.status} · {new Date(item.createdAt).toLocaleString()}
                    {item.sizeBytes != null ? ` · ${Math.round(item.sizeBytes / 1024)} KB` : ""}
                  </p>
                </div>
                <div className="flex flex-wrap gap-2">
                  <Button type="button" size="sm" variant="outline" onClick={() => setSelectedArtifactId(item.id)}>
                    Details
                  </Button>
                  {canRead && item.status === "READY" && (
                    <Button type="button" size="sm" variant="outline" disabled={access.isPending} onClick={() => void mintAccess(item.id)}>
                      Get secure link
                    </Button>
                  )}
                  {canDelete && item.status !== "DELETED" && (
                    <ActionDialog
                      label="Delete"
                      destructive
                      description="Deletion is complete only after storage confirms the object is absent. A pending deletion will be retried."
                      submit={async () => {
                        const deleted = await remove.mutateAsync(item.id);
                        setFeedback(deleted.status === "DELETED" ? "Artifact deleted and storage absence confirmed." : "Deletion is pending storage confirmation and will be retried.");
                      }}
                    />
                  )}
                </div>
              </article>
            ))}
          </div>
        )}

        {selectedArtifactId && (
          <section className="space-y-2 rounded-md border border-border p-3">
            {selectedArtifact.isLoading && <p role="status" className="text-sm">Loading artifact details…</p>}
            {selectedArtifact.isError && <RequestError error={selectedArtifact.error} retry={() => void selectedArtifact.refetch()} />}
            {selectedArtifact.data && (
              <>
                <div className="flex items-center justify-between gap-2">
                  <h3 className="text-sm font-semibold">Artifact details</h3>
                  <Button type="button" size="sm" variant="ghost" onClick={() => setSelectedArtifactId("")}>Close</Button>
                </div>
                <dl className="grid gap-x-4 gap-y-1 text-xs sm:grid-cols-2">
                  <dt className="text-muted-foreground">ID</dt><dd className="break-all font-mono">{selectedArtifact.data.id}</dd>
                  <dt className="text-muted-foreground">Type / status</dt><dd>{selectedArtifact.data.artifactType} · {selectedArtifact.data.status}</dd>
                  <dt className="text-muted-foreground">MIME type</dt><dd>{selectedArtifact.data.mimeType || "Not available"}</dd>
                  <dt className="text-muted-foreground">Ready at</dt><dd>{selectedArtifact.data.readyAt ? new Date(selectedArtifact.data.readyAt).toLocaleString() : "Not ready"}</dd>
                </dl>
              </>
            )}
          </section>
        )}
      </section>

      <ConsentControls conferenceId={conferenceId} enabled={canUseOwnConsent} />
      <TranscriptionControls
        conferenceId={conferenceId}
        enabled={canReadTranscripts}
        canStart={meetingAllows(role, "transcription.start", rolePolicy)}
        canStop={meetingAllows(role, "transcription.stop", rolePolicy)}
        canTranslate={meetingAllows(role, "transcription.start", rolePolicy)}
        canRequestIntelligence={canRequestIntelligence}
        live={live}
        artifacts={items}
      />
    </div>
  );
}

function ConsentControls({ conferenceId, enabled }: { conferenceId: string; enabled: boolean }) {
  const [feedback, setFeedback] = useState("");
  const requirements = useConsentRequirements(conferenceId, enabled);
  const record = useRecordConsents(conferenceId);
  const withdraw = useWithdrawConsent(conferenceId);

  const decide = async (consentType: ConsentKind, decision: "ACCEPTED" | "DECLINED", version: string) => {
    setFeedback("");
    try {
      await record.mutateAsync({ consentVersion: version, decisions: [{ consentType, decision }] });
      setFeedback(`${consentLabels[consentType]} consent ${decision.toLowerCase()}.`);
    } catch {
      setFeedback("");
    }
  };

  if (!enabled) return null;

  return (
    <section className="space-y-3 rounded-xl border border-border bg-card p-4">
      <header>
        <h2 className="font-semibold">Your consent</h2>
        <p className="text-sm text-muted-foreground">Each processing type has an independent decision. Only your own decisions can be recorded.</p>
      </header>
      {requirements.isLoading && <p role="status" className="text-sm">Loading consent requirements…</p>}
      {requirements.isError && <RequestError error={requirements.error} retry={() => void requirements.refetch()} />}
      {record.isError && <RequestError error={record.error} />}
      {withdraw.isError && <RequestError error={withdraw.error} />}
      {feedback && <p role="status" className="rounded-md bg-muted p-3 text-sm">{feedback}</p>}
      {requirements.data?.recordingNotice && <p className="rounded-md bg-muted p-3 text-sm">Recording is active. This notice does not imply consent to transcription or other processing.</p>}
      {requirements.data?.requirements.map(item => (
        <article key={item.consentType} className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border p-3">
          <div>
            <p className="text-sm font-medium">{consentLabels[item.consentType]}</p>
            <p className="text-xs text-muted-foreground">
              {item.mode.replaceAll("_", " ")} · {item.active ? "active" : "inactive"} · {item.decision || "no decision"}
              {item.required ? " · required" : ""}
            </p>
          </div>
          <div className="flex flex-wrap gap-2">
            {item.decision !== "ACCEPTED" && (
              <Button type="button" size="sm" disabled={record.isPending} onClick={() => void decide(item.consentType, "ACCEPTED", item.consentVersion)}>Accept</Button>
            )}
            {item.decision !== "DECLINED" && (
              <Button type="button" size="sm" variant="outline" disabled={record.isPending} onClick={() => void decide(item.consentType, "DECLINED", item.consentVersion)}>Decline</Button>
            )}
            {item.decision === "ACCEPTED" && (
              <ActionDialog
                label="Withdraw"
                destructive
                description="The meeting profile determines whether withdrawal only records the choice, stops processing, or returns you to the waiting room. Retained artifacts are not deleted."
                submit={async () => {
                  const result = await withdraw.mutateAsync(item.consentType);
                  setFeedback(withdrawalDescription(result));
                }}
              />
            )}
          </div>
        </article>
      ))}
    </section>
  );
}

function TranscriptionControls({
  conferenceId,
  enabled,
  canStart,
  canStop,
  canTranslate,
  canRequestIntelligence,
  live,
  artifacts,
}: {
  conferenceId: string;
  enabled: boolean;
  canStart: boolean;
  canStop: boolean;
  canTranslate: boolean;
  canRequestIntelligence: boolean;
  live: boolean;
  artifacts: { id: string; status: string; artifactType: ArtifactType }[];
}) {
  const [mode, setMode] = useState<"LIVE" | "POST_MEETING">("LIVE");
  const [language, setLanguage] = useState("en");
  const [sourceRecordingId, setSourceRecordingId] = useState("");
  const [selectedArtifactId, setSelectedArtifactId] = useState("");
  const [after, setAfter] = useState<number>();
  const [targetLanguage, setTargetLanguage] = useState("");
  const [feedback, setFeedback] = useState("");
  const transcripts = useTranscripts(conferenceId, enabled);
  const start = useStartTranscription(conferenceId);
  const stop = useStopTranscription(conferenceId);
  const transcript = useTranscript(conferenceId, selectedArtifactId, { after, limit: 200 }, enabled);
  const translate = useTranslateTranscript(conferenceId);
  const intelligence = useRequestIntelligence(conferenceId);
  const recordings = artifacts.filter(item => item.artifactType === "RECORDING" && item.status === "READY");
  const selectedJob = transcripts.data?.jobs.find(job => job.artifactId === selectedArtifactId);
  const transcriptReady = transcript.data?.artifact.status === "READY";

  const begin = async () => {
    setFeedback("");
    try {
      const job = await start.mutateAsync({
        mode,
        language: language.trim() || "en",
        ...(mode === "POST_MEETING" ? { sourceRecordingId } : {}),
      });
      setFeedback(`Transcription job created (${job.status}).`);
    } catch {
      setFeedback("");
    }
  };

  if (!enabled && !canStart) return null;

  return (
    <section className="space-y-3 rounded-xl border border-border bg-card p-4">
      <header>
        <h2 className="font-semibold">Transcription and intelligence</h2>
        <p className="text-sm text-muted-foreground">Transcription requires an enabled provider and profile authorization. Generated intelligence is currently disabled by default.</p>
      </header>
      {feedback && <p role="status" className="rounded-md bg-muted p-3 text-sm">{feedback}</p>}
      {start.isError && <RequestError error={start.error} />}
      {stop.isError && <RequestError error={stop.error} />}
      {translate.isError && <RequestError error={translate.error} />}
      {intelligence.isError && <RequestError error={intelligence.error} />}
      {transcripts.isError && <RequestError error={transcripts.error} retry={() => void transcripts.refetch()} />}

      {canStart && (
        <div className="flex flex-wrap items-end gap-2">
          <label className="grid gap-1 text-sm font-medium">
            Mode
            <select className="h-9 rounded-md border border-input bg-background px-2" value={mode} onChange={event => setMode(event.target.value as typeof mode)}>
              <option value="LIVE">Live</option>
              <option value="POST_MEETING">Post-meeting</option>
            </select>
          </label>
          <Field label="Language tag" value={language} maxLength={16} onChange={event => setLanguage(event.target.value)} />
          {mode === "POST_MEETING" && (
            <label className="grid gap-1 text-sm font-medium">
              Ready recording
              <select className="h-9 rounded-md border border-input bg-background px-2" value={sourceRecordingId} onChange={event => setSourceRecordingId(event.target.value)}>
                <option value="">Select recording</option>
                {recordings.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
              </select>
            </label>
          )}
          <Button type="button" disabled={start.isPending || (mode === "LIVE" && !live) || (mode === "POST_MEETING" && !sourceRecordingId)} onClick={() => void begin()}>
            {start.isPending ? "Starting…" : "Start transcription"}
          </Button>
        </div>
      )}
      {canStart && mode === "LIVE" && !live && <p className="text-xs text-muted-foreground">Live transcription requires a live conference.</p>}

      {transcripts.isLoading && enabled && <p role="status" className="text-sm">Loading transcription jobs…</p>}
      {enabled && transcripts.data && (
        <div className="space-y-2 border-t border-border pt-3">
          {transcripts.data.jobs.length === 0 && <p className="text-sm text-muted-foreground">No transcription jobs yet.</p>}
          {transcripts.data.jobs.map(job => (
            <article key={job.id} className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border p-3">
              <div>
                <p className="text-sm font-medium">{job.mode.replaceAll("_", " ")} · {job.language}</p>
                <p className="text-xs text-muted-foreground">{job.status} · {job.segmentCount} segments{job.failureCode ? ` · ${job.failureCode}` : ""}</p>
              </div>
              <div className="flex gap-2">
                {!!job.artifactId && <Button type="button" size="sm" variant="outline" onClick={() => { setSelectedArtifactId(job.artifactId); setAfter(undefined); }}>View transcript</Button>}
                {canStop && ["REQUESTED", "ACTIVE"].includes(job.status) && (
                  <Button type="button" size="sm" variant="destructive" disabled={stop.isPending} onClick={() => void stop.mutateAsync(job.id).then(() => setFeedback("Transcription stop requested.")).catch(() => undefined)}>Stop</Button>
                )}
              </div>
            </article>
          ))}
        </div>
      )}

      {selectedArtifactId && (
        <TranscriptDetail
          conferenceId={conferenceId}
          artifactId={selectedArtifactId}
          after={after}
          transcript={transcript.data}
          loading={transcript.isLoading}
          error={transcript.error}
          hasError={transcript.isError}
          retry={() => void transcript.refetch()}
          next={() => setAfter(transcript.data?.nextAfter)}
          close={() => { setSelectedArtifactId(""); setAfter(undefined); }}
          canTranslate={canTranslate && !!transcriptReady}
          canRequestIntelligence={canRequestIntelligence && !!transcriptReady}
          targetLanguage={targetLanguage}
          setTargetLanguage={setTargetLanguage}
          onTranslate={async () => {
            const result = await translate.mutateAsync({
              artifactId: selectedArtifactId,
              data: { targetLanguage: targetLanguage.trim() },
            });
            setFeedback(`Translated ${result.translatedSegments} segments into ${result.targetLanguage}. Original transcript remains unchanged.`);
          }}
          onIntelligence={async (kind: "SUMMARY" | "ACTION_ITEMS") => {
            const artifact = await intelligence.mutateAsync({ sourceArtifactId: selectedArtifactId, kind });
            setFeedback(`Intelligence artifact created (${artifact.status}).`);
          }}
          translatePending={translate.isPending}
          intelligencePending={intelligence.isPending}
          selectedJob={selectedJob}
        />
      )}
    </section>
  );
}

function TranscriptDetail({
  conferenceId,
  artifactId,
  after,
  transcript,
  loading,
  error,
  hasError,
  retry,
  next,
  close,
  canTranslate,
  canRequestIntelligence,
  targetLanguage,
  setTargetLanguage,
  onTranslate,
  onIntelligence,
  translatePending,
  intelligencePending,
  selectedJob,
}: {
  conferenceId: string;
  artifactId: string;
  after?: number;
  transcript?: ReturnType<typeof useTranscript>["data"];
  loading: boolean;
  error: unknown;
  hasError: boolean;
  retry: () => void;
  next: () => void;
  close: () => void;
  canTranslate: boolean;
  canRequestIntelligence: boolean;
  targetLanguage: string;
  setTargetLanguage: (value: string) => void;
  onTranslate: () => Promise<void>;
  onIntelligence: (kind: "SUMMARY" | "ACTION_ITEMS") => Promise<void>;
  translatePending: boolean;
  intelligencePending: boolean;
  selectedJob?: { language: string };
}) {
  const [kind, setKind] = useState<"SUMMARY" | "ACTION_ITEMS">("SUMMARY");

  return (
    <div className="space-y-3 rounded-md border border-border p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold">Transcript</h3>
          <p className="break-all font-mono text-xs text-muted-foreground">{artifactId}</p>
        </div>
        <Button type="button" size="sm" variant="ghost" onClick={close}>Close</Button>
      </div>
      {loading && <p role="status" className="text-sm">Loading transcript…</p>}
      {hasError && <RequestError error={error} retry={retry} />}
      {transcript && (
        <>
          <p className="text-xs text-muted-foreground">{transcript.artifact.status} · {selectedJob?.language || transcript.job?.language}</p>
          {transcript.segments.map(segment => (
            <article key={segment.sequence} className="rounded-md bg-muted/50 p-2 text-sm">
              <p className="mb-1 text-xs text-muted-foreground">
                {segment.speakerParticipantId || "Speaker"} · {Math.floor(segment.startMs / 1000)}s–{Math.floor(segment.endMs / 1000)}s
              </p>
              <p>{segment.text}</p>
            </article>
          ))}
          {transcript.segments.length === 0 && <p className="text-sm text-muted-foreground">No final transcript segments are available.</p>}
          {transcript.nextAfter != null && <Button type="button" size="sm" variant="outline" disabled={loading} onClick={next}>Load next page</Button>}
        </>
      )}
      {(canTranslate || canRequestIntelligence) && (
        <div className="flex flex-wrap items-end gap-2 border-t border-border pt-3">
          {canTranslate && (
            <>
              <Field label="Translation language" value={targetLanguage} maxLength={16} onChange={event => setTargetLanguage(event.target.value)} placeholder="e.g. fr-CA" />
              <Button type="button" variant="outline" disabled={!targetLanguage.trim() || translatePending} onClick={() => void onTranslate().catch(() => undefined)}>
                {translatePending ? "Translating…" : "Translate"}
              </Button>
            </>
          )}
          {canRequestIntelligence && (
            <>
              <label className="grid gap-1 text-sm font-medium">
                Output
                <select className="h-9 rounded-md border border-input bg-background px-2" value={kind} onChange={event => setKind(event.target.value as typeof kind)}>
                  <option value="SUMMARY">Summary</option>
                  <option value="ACTION_ITEMS">Action items</option>
                </select>
              </label>
              <Button type="button" variant="outline" disabled={intelligencePending} onClick={() => void onIntelligence(kind).catch(() => undefined)}>
                {intelligencePending ? "Requesting…" : "Request intelligence"}
              </Button>
            </>
          )}
        </div>
      )}
      <span className="sr-only">Transcript page offset: {after ?? 0}</span>
      <span className="sr-only">Conference: {conferenceId}</span>
    </div>
  );
}

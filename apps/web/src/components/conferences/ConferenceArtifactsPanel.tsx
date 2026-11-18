"use client";

import { useMemo, useState } from "react";
import { BarChart3, Captions, CheckCircle2, ClipboardList, Download, FileText, Languages, ListChecks, Loader2, MessageSquare, Search, ShieldCheck, Sparkles, Square, Users } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import apiV2 from "@/services/v2/api";
import { ActionDialog, RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useParticipantsV2 } from "@/hooks/v2";
import {
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
import { artifactAccessible, artifactDisplayName, artifactStateLabel, formatBytes } from "@/lib/artifact-display";
import { cn } from "@/lib/utils";
import type { ConsentKind, GenerateReportRequest, ProfilePolicy, Role, WithdrawalResponse } from "@/types/v2";

type ReportType = GenerateReportRequest["artifactType"];
type Tab = "transcript" | "attendance" | "reports" | "consent";

const REPORTS: { value: ReportType; label: string; hint: string; icon: typeof Users }[] = [
  { value: "ATTENDANCE_REPORT", label: "Attendance", hint: "Who joined and for how long", icon: Users },
  { value: "POLL_REPORT", label: "Poll results", hint: "Every poll and its answers", icon: BarChart3 },
  { value: "CHAT_EXPORT", label: "Chat", hint: "All in-call messages", icon: MessageSquare },
];

const LANGUAGES = [
  { value: "en", label: "English" }, { value: "fr", label: "French" }, { value: "es", label: "Spanish" },
  { value: "pt", label: "Portuguese" }, { value: "de", label: "German" }, { value: "ar", label: "Arabic" },
  { value: "zh", label: "Chinese" }, { value: "hi", label: "Hindi" }, { value: "sw", label: "Swahili" },
  { value: "yo", label: "Yoruba" }, { value: "ha", label: "Hausa" }, { value: "ig", label: "Igbo" },
];
const languageName = (code?: string) => LANGUAGES.find((l) => l.value === code?.split("-")[0])?.label ?? code ?? "";

const CONSENT_LABEL: Record<ConsentKind, string> = {
  RECORDING: "Recording",
  TRANSCRIPTION: "Transcription",
  AI_PROCESSING: "AI summaries",
  DATA_SHARING: "Data sharing",
  HEALTHCARE_TERMS: "Healthcare terms",
  PRIVACY_NOTICE: "Privacy notice",
};

const JOB_STATUS: Record<string, string> = {
  REQUESTED: "Starting…", ACTIVE: "Transcribing now", STOPPING: "Stopping…", PROCESSING: "Finishing up…", READY: "Ready", FAILED: "Failed", STOPPED: "Stopped",
};

function withdrawalText(result: WithdrawalResponse) {
  if (result.effect === "PROCESSING_STOPPED") return "Withdrawn. Anything that depended on it has stopped. Existing files stay.";
  if (result.effect === "PARTICIPANT_EXITED") return "Withdrawn. This meeting's rules moved you back to the waiting room. Existing files stay.";
  return "Withdrawn. Existing files stay.";
}

const clock = (ms: number) => { const s = Math.floor(ms / 1000); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`; };
const btn = "inline-flex h-9 items-center justify-center gap-1.5 rounded-full px-4 text-sm font-semibold transition-colors disabled:opacity-50";

/** Download a report straight away as a file (no stored copy). */
async function downloadReport(conferenceId: string, type: string, label: string) {
  const data = (await apiV2.get<unknown>(`/conferences/${encodeURIComponent(conferenceId)}/exports/${type}`)).data;
  const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url; a.download = `${label.toLowerCase().replace(/\s+/g, "-")}-${conferenceId.slice(0, 8)}.json`; a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export default function ConferenceArtifactsPanel({ conferenceId, spaceRole, rolePolicy, live }: {
  conferenceId: string;
  spaceRole?: Role;
  rolePolicy?: ProfilePolicy extends never ? never : import("@/types/v2").RolePolicy;
  recordingPolicy?: string;
  live: boolean;
}) {
  const me = useCurrentParticipant(conferenceId);
  const role = effectiveConferenceRole(spaceRole, me.data?.role, me.data?.admissionState);
  const can = (permission: string) => meetingAllows(role, permission as never, rolePolicy);
  const canList = can("artifact.list") || can("artifact.read") || can("recording.start");
  const canExport = can("artifact.export");
  const canTranscripts = can("transcript.read") || can("transcription.start");
  const canConsent = me.data?.admissionState === "ADMITTED";
  const consent = useConsentRequirements(conferenceId, canConsent);
  const hasConsent = !!consent.data?.requirements.length;

  const tabs = ([
    canTranscripts && { id: "transcript", label: "Transcript", icon: Captions },
    canExport && { id: "attendance", label: "Attendance", icon: Users },
    (canList || canExport) && { id: "reports", label: "Reports", icon: ClipboardList },
    hasConsent && { id: "consent", label: "Your consent", icon: ShieldCheck },
  ].filter(Boolean)) as { id: Tab; label: string; icon: typeof Users }[];
  const [picked, setPicked] = useState<Tab>();
  const tab = tabs.find((t) => t.id === picked)?.id ?? tabs[0]?.id;

  if (!tabs.length) return null;

  return (
    <section>
      <h2 className="text-lg font-semibold text-foreground">Notes and reports</h2>
      <p className="mt-0.5 text-sm text-muted-foreground">Transcript, attendance and exports from this session.</p>
      <div role="tablist" className="no-scrollbar mt-4 flex gap-1 overflow-x-auto border-b border-border">
        {tabs.map(({ id, label, icon: Icon }) => (
          <button key={id} role="tab" type="button" aria-selected={tab === id} onClick={() => setPicked(id)}
            className={cn("-mb-px inline-flex h-10 shrink-0 items-center gap-1.5 border-b-2 px-3 text-sm font-medium transition-colors", tab === id ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>
            <Icon className="size-4" />{label}
          </button>
        ))}
      </div>
      <div className="pt-5">
        {tab === "transcript" && <TranscriptTab conferenceId={conferenceId} live={live} canRead={can("transcript.read")} canStart={can("transcription.start")} canStop={can("transcription.stop")} canAi={can("intelligence.request")} canList={canList} />}
        {tab === "attendance" && <AttendanceTab conferenceId={conferenceId} />}
        {tab === "reports" && <ReportsTab conferenceId={conferenceId} canList={canList} canExport={canExport} canRead={can("artifact.read")} canDelete={can("artifact.delete")} />}
        {tab === "consent" && <ConsentTab conferenceId={conferenceId} />}
      </div>
    </section>
  );
}

function Empty({ icon: Icon, title, text, children }: { icon: typeof Users; title: string; text: string; children?: React.ReactNode }) {
  return (
    <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-10 text-center">
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Icon className="size-5" /></span>
      <p className="mt-3 font-semibold text-foreground">{title}</p>
      <p className="mt-1 max-w-sm text-sm text-muted-foreground">{text}</p>
      {children}
    </div>
  );
}

function TranscriptTab({ conferenceId, live, canRead, canStart, canStop, canAi, canList }: { conferenceId: string; live: boolean; canRead: boolean; canStart: boolean; canStop: boolean; canAi: boolean; canList: boolean }) {
  const transcripts = useTranscripts(conferenceId, canRead || canStart);
  const artifacts = useArtifacts(conferenceId, undefined, canList && canStart);
  const start = useStartTranscription(conferenceId);
  const stop = useStopTranscription(conferenceId);
  const translate = useTranslateTranscript(conferenceId);
  const ai = useRequestIntelligence(conferenceId);
  const people = useParticipantsV2(conferenceId);
  const names = useMemo(() => new Map((people.data ?? []).map((p) => [p.id, p.displayName])), [people.data]);

  const jobs = transcripts.data?.jobs ?? [];
  const running = jobs.find((job) => ["REQUESTED", "ACTIVE", "STOPPING", "PROCESSING"].includes(job.status));
  const withText = jobs.filter((job) => job.artifactId);
  const [jobId, setJobId] = useState<string>();
  const job = withText.find((j) => j.id === jobId) ?? withText[0];
  const [after, setAfter] = useState<number>();
  const transcript = useTranscript(conferenceId, job?.artifactId ?? "", { after, limit: 200 }, canRead && !!job?.artifactId);
  const ready = transcript.data?.artifact.status === "READY";
  const [query, setQuery] = useState("");
  const [language, setLanguage] = useState("en");
  const [target, setTarget] = useState("fr");
  const [recordingId, setRecordingId] = useState<string>();
  const [notice, setNotice] = useState("");
  const recordings = (artifacts.data?.artifacts ?? []).filter((a) => a.artifactType === "RECORDING" && a.status === "READY");
  const failure = [start, stop, translate, ai].find((m) => m.isError)?.error;

  const segments = (transcript.data?.segments ?? []).filter((s) => !query.trim() || s.text.toLowerCase().includes(query.trim().toLowerCase()));

  const begin = async () => {
    setNotice("");
    const source = recordingId ?? recordings[0]?.id;
    try {
      await start.mutateAsync(live ? { mode: "LIVE", language } : { mode: "POST_MEETING", language, sourceRecordingId: source });
      setNotice(live ? "Live transcription started. Captions are available in the call." : "Transcribing the recording. This can take a few minutes.");
    } catch { /* shown below */ }
  };

  return (
    <div className="space-y-4">
      {transcripts.isError && <RequestError error={transcripts.error} retry={() => void transcripts.refetch()} />}
      {failure && <RequestError error={failure} />}
      {notice && <p role="status" className="flex items-start gap-2 rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground"><CheckCircle2 className="mt-0.5 size-4 shrink-0 text-primary" />{notice}</p>}

      {running && (
        <div className="flex flex-wrap items-center gap-3 rounded-2xl border border-border bg-muted/40 p-4">
          <span className="relative flex size-2.5"><span className="absolute inline-flex size-full animate-ping rounded-full bg-emerald-400 opacity-60" /><span className="relative inline-flex size-2.5 rounded-full bg-emerald-500" /></span>
          <div className="min-w-0 flex-1"><p className="text-sm font-semibold text-foreground">{JOB_STATUS[running.status] ?? running.status}</p><p className="text-xs text-muted-foreground">{languageName(running.language)} · {running.segmentCount} lines so far</p></div>
          {canStop && ["REQUESTED", "ACTIVE"].includes(running.status) && <button type="button" onClick={() => stop.mutate(running.id)} disabled={stop.isPending} className={cn(btn, "border border-border bg-background text-foreground hover:bg-muted")}><Square className="size-3.5" />Stop</button>}
        </div>
      )}

      {canStart && !running && (live || recordings.length > 0) && (
        <div className="grid gap-3 rounded-2xl border border-border p-4 sm:grid-cols-[1fr_1fr_auto] sm:items-end">
          <NiceSelect label="Spoken language" value={language} onChange={setLanguage} options={LANGUAGES} />
          {!live && recordings.length > 1
            ? <NiceSelect label="Recording" value={recordingId ?? recordings[0].id} onChange={setRecordingId} options={recordings.map((r, i) => ({ value: r.id, label: `Recording ${i + 1}`, hint: artifactStateLabel(r) }))} />
            : <p className="text-sm text-muted-foreground sm:pb-3">{live ? "Transcribes the call as people speak." : "Transcribes this session's recording."}</p>}
          <button type="button" onClick={() => void begin()} disabled={start.isPending} className={cn(btn, "h-11 bg-primary text-primary-foreground hover:bg-primary/90")}>{start.isPending ? <Loader2 className="size-4 animate-spin" /> : <Captions className="size-4" />}{live ? "Start transcribing" : "Transcribe recording"}</button>
        </div>
      )}

      {transcripts.isLoading && <div className="h-40 animate-pulse rounded-2xl bg-muted" />}

      {!transcripts.isLoading && !withText.length && !running && (
        <Empty icon={Captions} title="No transcript yet" text={canStart ? (live ? "Start transcribing to get live captions now and a searchable transcript after." : recordings.length ? "Transcribe the recording to get a searchable transcript." : "Transcripts come from live transcription or a recording. This session has neither.") : "If a host transcribes this session, the transcript shows up here."} />
      )}

      {job && canRead && (
        <div className="overflow-hidden rounded-2xl border border-border">
          <div className="flex flex-wrap items-center gap-2 border-b border-border bg-muted/40 p-3">
            {withText.length > 1
              ? <NiceSelect className="w-56" value={job.id} onChange={(id) => { setJobId(id); setAfter(undefined); }} options={withText.map((j) => ({ value: j.id, label: `${languageName(j.language)} transcript`, hint: JOB_STATUS[j.status] ?? j.status }))} />
              : <p className="px-1 text-sm font-semibold text-foreground">{languageName(job.language)} transcript <span className="font-normal text-muted-foreground">· {JOB_STATUS[job.status] ?? job.status}</span></p>}
            <label className="relative ml-auto w-full sm:w-56">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search transcript" className="h-9 w-full rounded-full border border-border bg-background pl-9 pr-3 text-sm outline-none focus:border-primary" />
            </label>
          </div>
          {transcript.isError && <div className="p-3"><RequestError error={transcript.error} retry={() => void transcript.refetch()} /></div>}
          <div className="max-h-[28rem] space-y-4 overflow-y-auto p-4">
            {transcript.isLoading && <div className="space-y-3">{[0, 1, 2].map((i) => <div key={i} className="h-10 animate-pulse rounded-lg bg-muted" />)}</div>}
            {segments.map((segment) => (
              <div key={segment.sequence} className="flex gap-3">
                <span className="w-11 shrink-0 pt-0.5 text-xs tabular-nums text-muted-foreground">{clock(segment.startMs)}</span>
                <div className="min-w-0">
                  <p className="text-xs font-semibold text-primary">{(segment.speakerParticipantId && names.get(segment.speakerParticipantId)) || "Speaker"}</p>
                  <p className="text-sm leading-relaxed text-foreground">{segment.text}</p>
                </div>
              </div>
            ))}
            {transcript.data && !segments.length && <p className="text-sm text-muted-foreground">{query ? "Nothing matches your search." : "No lines yet."}</p>}
            {transcript.data?.nextAfter != null && <button type="button" onClick={() => setAfter(transcript.data?.nextAfter ?? undefined)} className={cn(btn, "w-full border border-border text-foreground hover:bg-muted")}>Show more</button>}
          </div>
          {ready && (canStart || canAi) && (
            <div className="flex flex-wrap items-end gap-2 border-t border-border bg-muted/40 p-3">
              {canStart && (
                <div className="flex items-end gap-2">
                  <NiceSelect className="w-40" value={target} onChange={setTarget} options={LANGUAGES.filter((l) => l.value !== job.language)} />
                  <button type="button" disabled={translate.isPending} onClick={() => job.artifactId && translate.mutate({ artifactId: job.artifactId, data: { targetLanguage: target } }, { onSuccess: (r) => setNotice(`Translated into ${languageName(r.targetLanguage)}. The original stays as it is.`) })} className={cn(btn, "h-11 border border-border bg-background text-foreground hover:bg-muted")}><Languages className="size-4" />{translate.isPending ? "Translating…" : "Translate"}</button>
                </div>
              )}
              {canAi && (
                <div className="flex gap-2 sm:ml-auto">
                  <button type="button" disabled={ai.isPending} onClick={() => job.artifactId && ai.mutate({ sourceArtifactId: job.artifactId, kind: "SUMMARY" }, { onSuccess: () => setNotice("Summary requested. It shows under Reports when it's ready.") })} className={cn(btn, "h-11 bg-primary text-primary-foreground hover:bg-primary/90")}><Sparkles className="size-4" />Summary</button>
                  <button type="button" disabled={ai.isPending} onClick={() => job.artifactId && ai.mutate({ sourceArtifactId: job.artifactId, kind: "ACTION_ITEMS" }, { onSuccess: () => setNotice("Action items requested. They show under Reports when ready.") })} className={cn(btn, "h-11 border border-border bg-background text-foreground hover:bg-muted")}><ListChecks className="size-4" />Action items</button>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function AttendanceTab({ conferenceId }: { conferenceId: string }) {
  const attendance = useAttendance(conferenceId, true);
  const people = attendance.data?.participants ?? [];
  const total = people.reduce((sum, p) => sum + p.attendanceSeconds, 0);
  const minutes = (s: number) => (s < 60 ? "<1 min" : `${Math.round(s / 60)} min`);
  return (
    <div className="space-y-4">
      {attendance.isError && <RequestError error={attendance.error} retry={() => void attendance.refetch()} />}
      {attendance.isLoading && <div className="h-40 animate-pulse rounded-2xl bg-muted" />}
      {attendance.data && (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
            <Stat label="People" value={String(people.length)} />
            <Stat label="Average time" value={people.length ? minutes(total / people.length) : "—"} />
            <Stat label="Joins" value={String(attendance.data.sessionCount)} className="hidden sm:block" />
          </div>
          {people.length === 0 ? <Empty icon={Users} title="No one has joined" text="Attendance appears once people join this session." /> : (
            <ul className="divide-y divide-border rounded-2xl border border-border">
              {people.map((person) => (
                <li key={person.participantId} className="flex items-center gap-3 px-4 py-3">
                  <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-primary/10 text-xs font-semibold text-primary">{person.displayName.split(/\s+/).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("")}</span>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium text-foreground">{person.displayName}</p>
                    <p className="text-xs capitalize text-muted-foreground">{person.role.replaceAll("_", " ").toLowerCase()}</p>
                  </div>
                  <span className="text-sm tabular-nums text-foreground">{minutes(person.attendanceSeconds)}</span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

function Stat({ label, value, className }: { label: string; value: string; className?: string }) {
  return <div className={cn("rounded-2xl border border-border p-4", className)}><p className="text-xs text-muted-foreground">{label}</p><p className="mt-1 text-xl font-semibold tabular-nums text-foreground">{value}</p></div>;
}

function ReportsTab({ conferenceId, canList, canExport, canRead, canDelete }: { conferenceId: string; canList: boolean; canExport: boolean; canRead: boolean; canDelete: boolean }) {
  const artifacts = useArtifacts(conferenceId, undefined, canList);
  const generate = useGenerateReport(conferenceId);
  const access = useArtifactAccess(conferenceId);
  const remove = useDeleteArtifact(conferenceId);
  const [notice, setNotice] = useState("");
  // Recordings live in their own panel; here: reports, transcripts files and AI notes.
  const items = (artifacts.data?.artifacts ?? []).filter((a) => a.artifactType !== "RECORDING" && a.status !== "DELETED");
  const [downloadError, setFailure] = useState<Error | null>(null);
  const failure = [generate, access, remove].find((m) => m.isError)?.error ?? downloadError;

  const open = async (id: string) => {
    setNotice("");
    try {
      const grant = await access.mutateAsync(id);
      window.open(grant.url, "_blank", "noopener,noreferrer");
    } catch { /* shown below */ }
  };

  return (
    <div className="space-y-4">
      {failure && <RequestError error={failure} />}
      {notice && <p role="status" className="flex items-center gap-2 rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground"><CheckCircle2 className="size-4 text-primary" />{notice}</p>}
      {canExport && (
        <div className="flex flex-wrap gap-2">
          {REPORTS.map(({ value, label }) => (
            <button key={value} type="button" onClick={() => void downloadReport(conferenceId, value, label).catch(setFailure)}
              className="inline-flex h-9 items-center gap-2 rounded-full border border-border px-3 text-xs font-semibold text-foreground hover:bg-muted">
              <Download className="size-3.5" />Download {label.toLowerCase()}
            </button>
          ))}
        </div>
      )}
      {canExport && (
        <div className="grid gap-2 sm:grid-cols-3">
          {REPORTS.map(({ value, label, hint, icon: Icon }) => (
            <button key={value} type="button" disabled={generate.isPending} onClick={() => generate.mutate({ artifactType: value }, { onSuccess: () => setNotice(`${label} report created.`) })}
              className="flex items-center gap-3 rounded-2xl border border-border p-3 text-left transition-colors hover:border-primary/50 hover:bg-primary/5 disabled:opacity-50">
              <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><Icon className="size-5" /></span>
              <span className="min-w-0"><span className="block text-sm font-semibold text-foreground">{label}</span><span className="block truncate text-xs text-muted-foreground">{hint}</span></span>
            </button>
          ))}
        </div>
      )}
      {artifacts.isError && <RequestError error={artifacts.error} retry={() => void artifacts.refetch()} />}
      {artifacts.isLoading && <div className="h-24 animate-pulse rounded-2xl bg-muted" />}
      {canList && !artifacts.isLoading && items.length === 0 && <Empty icon={FileText} title="No reports yet" text={canExport ? "Create one above. It's saved here so you can open it any time." : "Reports a host creates show up here."} />}
      {items.length > 0 && (
        <ul className="divide-y divide-border rounded-2xl border border-border">
          {items.map((item) => (
            <li key={item.id} className="flex items-center gap-3 px-4 py-3">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground"><FileText className="size-4" /></span>
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium text-foreground">{artifactDisplayName(item)}</p>
                <p className="text-xs text-muted-foreground">{artifactStateLabel(item)}{formatBytes(item.sizeBytes) ? ` · ${formatBytes(item.sizeBytes)}` : ""}{item.createdAt ? ` · ${new Date(item.createdAt).toLocaleDateString(undefined, { day: "numeric", month: "short" })}` : ""}</p>
              </div>
              {canRead && artifactAccessible(item) && <button type="button" aria-label={`Open ${artifactDisplayName(item)}`} disabled={access.isPending} onClick={() => void open(item.id)} className={cn(btn, "border border-border text-foreground hover:bg-muted")}><Download className="size-4" /><span className="max-sm:hidden">Open</span></button>}
              {canDelete && !item.deletionPending && (
                <ActionDialog label="Delete" destructive description="This removes the file for everyone. It can't be undone."
                  submit={async () => { const r = await remove.mutateAsync(item.id); setNotice(r.status === "DELETED" ? "Deleted." : "Deleting… this can take a moment."); }} />
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ConsentTab({ conferenceId }: { conferenceId: string }) {
  const requirements = useConsentRequirements(conferenceId, true);
  const record = useRecordConsents(conferenceId);
  const withdraw = useWithdrawConsent(conferenceId);
  const [notice, setNotice] = useState("");
  const failure = [record, withdraw].find((m) => m.isError)?.error;

  return (
    <div className="space-y-3">
      {requirements.isError && <RequestError error={requirements.error} retry={() => void requirements.refetch()} />}
      {failure && <RequestError error={failure} />}
      {notice && <p role="status" className="rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground">{notice}</p>}
      <p className="text-sm text-muted-foreground">Each choice is separate and only applies to you.</p>
      {requirements.data?.requirements.map((item) => (
        <div key={item.consentType} className="flex flex-wrap items-center gap-3 rounded-2xl border border-border p-4">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-semibold text-foreground">{CONSENT_LABEL[item.consentType]}{item.required && <span className="ml-2 text-xs font-normal text-muted-foreground">Required</span>}</p>
            <p className="text-xs text-muted-foreground">{item.active ? "In use in this session" : "Not in use right now"} · {item.decision === "ACCEPTED" ? "You agreed" : item.decision === "DECLINED" ? "You declined" : "No choice yet"}</p>
          </div>
          {item.decision === "ACCEPTED" ? (
            <ActionDialog label="Withdraw" destructive description="Depending on this meeting's rules, withdrawing may stop processing or move you to the waiting room. Files already made stay."
              submit={async () => setNotice(withdrawalText(await withdraw.mutateAsync(item.consentType)))} />
          ) : (
            <div className="flex gap-2">
              {item.decision !== "DECLINED" && <button type="button" disabled={record.isPending} onClick={() => record.mutate({ consentVersion: item.consentVersion, decisions: [{ consentType: item.consentType, decision: "DECLINED" }] }, { onSuccess: () => setNotice(`${CONSENT_LABEL[item.consentType]}: declined.`) })} className={cn(btn, "border border-border text-foreground hover:bg-muted")}>Decline</button>}
              <button type="button" disabled={record.isPending} onClick={() => record.mutate({ consentVersion: item.consentVersion, decisions: [{ consentType: item.consentType, decision: "ACCEPTED" }] }, { onSuccess: () => setNotice(`${CONSENT_LABEL[item.consentType]}: agreed.`) })} className={cn(btn, "bg-primary text-primary-foreground hover:bg-primary/90")}>Agree</button>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

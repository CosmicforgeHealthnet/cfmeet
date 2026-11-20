"use client";

/**
 * Pre-join of one conference (FE-001, FE-017). Keyed by the conference id; reads
 * `GET /api/v2/conferences/{id}/pre-join` (`access.*`, `consents.*`, `notices.*`), the
 * conference status (polled) and the resolved space (role, code). Joins with
 * `POST /api/v2/conferences/{id}/join` including the required consents at their version;
 * waits for admission by re-joining with the same session; a host may start a scheduled
 * occurrence here (`POST /conferences/{id}/start`) within the start window.
 */
import { type ReactNode, useState, useEffect, useRef, useCallback, useMemo } from "react";
import Image from "next/image";
import Link from "next/link";
import { useQueryClient } from "@tanstack/react-query";
import { Mic, MicOff, Video, VideoOff, XCircle, Clock, ShieldCheck, CircleDot, Captions, Sparkles, KeyRound, Ticket, ArrowLeft, CalendarClock, UserRound } from "lucide-react";
import { cn } from "@/lib/utils";
import { conferenceKeys, useConference, usePreJoin, useJoinConference } from "@/hooks/v2/useConferences";
import { useResolveSpace } from "@/hooks/v2/useSpaces";
import { conferencesService } from "@/services/v2/conferences.service";
import { participantsService } from "@/services/v2/participants.service";
import { useRealtime } from "@/hooks/v2/useRealtime";
import { conferencePermission } from "@/lib/meeting-scheduling";
import { CONSENT_TEXT, consentGrants, joinBlocker, joinErrorMessage, preJoinRequirements, roomJoinData, roomPhase, type RoomJoinData } from "@/lib/prejoin";
import { scheduleWindowLabel, viewerTimeZone } from "@/lib/meeting-time";
import type { ConsentKind, JoinConferenceResponseV2 } from "@/types/v2";
import type { JoinConferenceResponse } from "@/types/generated/meeting-service";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Spinner } from "@/components/ui/spinner";
import { useMediaStream } from "@/hooks/useMediaStream";
import { MicVisualizer } from "@/components/ui/mic-visualizer";
import { useProfile } from "@/hooks/useUser";
import { storageStore } from "@/store";
import { accountSession } from "@/lib/auth/session";

export interface MediaPreferences {
  isCameraOn: boolean;
  isMicOn: boolean;
}

interface PreJoinScreenProps {
  conferenceId: string;
  initialInvitationToken?: string;
  onJoin: (data: RoomJoinData, mediaPrefs: MediaPreferences) => void;
}

function Logo() {
  return (
    <div className="flex items-center">
      <Image src="/light-mode-cf-meet.png" alt="CF Meet Logo" width={120} height={30} className="h-auto dark:hidden" priority />
      <Image src="/dark-mode-cfmeet.png" alt="CF Meet Logo" width={120} height={30} className="h-auto hidden dark:block" priority />
    </div>
  );
}

export function RoomMessage({ icon, title, children }: { icon: "missing" | "ended"; title: string; children: ReactNode }) {
  return (
    <div className="flex min-h-screen flex-col bg-muted/40">
      <header className="flex h-16 items-center px-5 sm:px-8"><Logo /></header>
      <main className="flex flex-1 items-center justify-center px-4 pb-16">
        <div className="w-full max-w-md rounded-3xl border border-border bg-card p-8 text-center">
          <span className={cn("mx-auto flex size-16 items-center justify-center rounded-2xl", icon === "missing" ? "bg-destructive/10 text-destructive" : "bg-muted text-muted-foreground")}>
            {icon === "missing" ? <XCircle className="size-8" /> : <Clock className="size-8" />}
          </span>
          <h1 className="mt-6 text-2xl font-bold tracking-tight text-foreground">{title}</h1>
          <p className="mt-2 text-sm text-muted-foreground">{children}</p>
          <Link href="/dashboard" className="mt-8 inline-flex h-11 w-full items-center justify-center rounded-xl bg-primary font-semibold text-primary-foreground hover:bg-primary/90">Back to Home</Link>
        </div>
      </main>
    </div>
  );
}

function countdown(ms: number) {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600), m = Math.floor((total % 3600) / 60), s = total % 60;
  return h > 0 ? `${h}h ${m}m ${s}s` : m > 0 ? `${m}m ${s}s` : `${s}s`;
}

export default function PreJoinScreen({ conferenceId, onJoin, initialInvitationToken = "" }: PreJoinScreenProps) {
  const queryClient = useQueryClient();
  const signedIn = accountSession.hasSession();
  const preJoin = usePreJoin(conferenceId);
  const conference = useConference(conferenceId);
  const spaceId = preJoin.data?.meetingSpaceId || conference.data?.meetingSpaceId || "";
  const space = useResolveSpace(spaceId);
  const joinMutation = useJoinConference();

  const [passcode, setPasscode] = useState("");
  const [invitationToken, setInvitationToken] = useState(initialInvitationToken);
  const [consents, setConsents] = useState<Partial<Record<ConsentKind, boolean>>>({});
  const [isWaiting, setIsWaiting] = useState(false);
  /** Nobody who can admit is in the meeting (audit M3): told while waiting. */
  const [unattended, setUnattended] = useState(false);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const previousSession = useRef<string | undefined>(undefined);
  const pollingBusy = useRef(false);
  const pollingEpoch = useRef(0);
  const pollIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const { data: profileData } = useProfile();
  const storedUser = storageStore.getUser();
  const [displayName, setDisplayName] = useState(() => storedUser?.username || "");
  const [hasAutoFilled, setHasAutoFilled] = useState(false);
  const { videoRef, stream, isCameraOn, isMicOn, toggleCamera, toggleMic, error: mediaError, retry: retryMedia } = useMediaStream({ video: true, audio: true });
  const [editingName, setEditingName] = useState(false);

  const requirements = useMemo(() => preJoinRequirements(preJoin.data, {
    signedIn,
    spaceRequiresPasscode: space.data?.meetingSpace.requiresPasscode,
    hasInvitationToken: !!initialInvitationToken,
  }), [preJoin.data, signedIn, space.data?.meetingSpace.requiresPasscode, initialInvitationToken]);
  const grantedConsents = useMemo(() => consentGrants(requirements, consents), [requirements, consents]);
  const status = conference.data?.status ?? preJoin.data?.conferenceStatus;
  const phase = roomPhase(status);
  const scheduledStartAt = conference.data?.scheduledStartAt ?? preJoin.data?.scheduledStartAt ?? null;
  const meetingZone = space.data?.meetingSpace.timezone ?? undefined;
  const canStart = !!space.data && conferencePermission(space.data.meetingSpace, "start");
  const blocker = joinBlocker(requirements, { displayName, passcode, invitationToken, checked: consents });
  const title = preJoin.data?.title || space.data?.meetingSpace.title || "Meeting";
  const meetingCode = space.data?.meetingSpace.meetingCode;
  const timeUntilStart = phase === "not-started" && scheduledStartAt ? Math.max(0, Date.parse(scheduledStartAt) - now) : null;

  // Sync the profile name once when it loads (an external async source).
  useEffect(() => {
    if (profileData?.username && !displayName && !hasAutoFilled) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setHasAutoFilled(true);
      setDisplayName(profileData.username);
    }
  }, [profileData?.username, displayName, hasAutoFilled]);

  useEffect(() => () => {
    if (pollIntervalRef.current) clearInterval(pollIntervalRef.current);
    pollingEpoch.current += 1;
  }, []);

  // Countdown tick while a scheduled occurrence has not started.
  useEffect(() => {
    if (phase !== "not-started" || !scheduledStartAt) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [phase, scheduledStartAt]);

  // Guests may not read the conference itself: poll the pre-join status until the host starts.
  const refetchPreJoin = preJoin.refetch;
  useEffect(() => {
    if (phase !== "not-started" || !conference.isError) return;
    const id = setInterval(() => void refetchPreJoin(), 10_000);
    return () => clearInterval(id);
  }, [phase, conference.isError, refetchPreJoin]);

  // The pre-join answer depends on the conference state: refresh it when the status changes.
  useEffect(() => {
    if (status && preJoin.data && status !== preJoin.data.conferenceStatus) void queryClient.invalidateQueries({ queryKey: conferenceKeys.preJoin(conferenceId) });
  }, [status, preJoin.data, queryClient, conferenceId]);

  const toRoom = useCallback((response: JoinConferenceResponseV2): RoomJoinData | null => {
    if (response.participantToken) storageStore.setParticipantToken(response.participantToken);
    previousSession.current = response.participantSession?.id;
    storageStore.setV2JoinData(response);
    return roomJoinData(response as unknown as JoinConferenceResponse, !signedIn);
  }, [signedIn]);

  const joinRequest = useCallback(() => ({
    displayName: displayName.trim(),
    passcode: passcode.trim() || undefined,
    invitationToken: invitationToken.trim() || undefined,
    previousSessionId: previousSession.current,
    consents: grantedConsents,
  }), [displayName, passcode, invitationToken, grantedConsents]);

  // Waiting room: re-join with the same session until media credentials are issued.
  const startPolling = useCallback(() => {
    const epoch = ++pollingEpoch.current;
    if (pollIntervalRef.current) clearInterval(pollIntervalRef.current);
    pollIntervalRef.current = setInterval(async () => {
      if (pollingBusy.current) return;
      pollingBusy.current = true;
      try {
        // A wait the host (or the waiting-room policy) ended stays ended: never knock again.
        const mine = await participantsService.me(conferenceId).catch(() => null);
        if (mine?.admissionState === "DENIED") {
          if (pollIntervalRef.current) { clearInterval(pollIntervalRef.current); pollIntervalRef.current = null; }
          setIsWaiting(false);
          setUnattended(false);
          setError(mine.denialMessage || "The host didn't let you in.");
          return;
        }
        const result = await joinMutation.mutateAsync({ id: conferenceId, data: joinRequest() });
        if (epoch !== pollingEpoch.current) {
          if (result.participantToken) storageStore.setParticipantToken(result.participantToken);
          await conferencesService.leave(conferenceId, { participantSessionId: result.participantSession?.id });
          storageStore.clearParticipantToken(); storageStore.clearV2JoinData();
          return;
        }
        const data = toRoom(result);
        if (data) {
          if (pollIntervalRef.current) { clearInterval(pollIntervalRef.current); pollIntervalRef.current = null; }
          setIsWaiting(false);
          onJoin(data, { isCameraOn, isMicOn });
        }
      } catch (cause) {
        const code = (cause as { response?: { status?: number } })?.response?.status;
        if (code && code >= 400 && code < 500) {
          if (pollIntervalRef.current) clearInterval(pollIntervalRef.current);
          setIsWaiting(false);
          setError(joinErrorMessage(cause));
        }
      } finally {
        pollingBusy.current = false;
      }
    }, 3000);
  }, [conferenceId, joinMutation, joinRequest, toRoom, onJoin, isCameraOn, isMicOn]);

  // Waiting-room notices (audit M3): whether anyone who can admit is in the meeting.
  useRealtime({
    conferenceId,
    enabled: isWaiting,
    onEvent: (event) => {
      if (event.eventType === "conference.waiting_room_unattended") setUnattended(true);
      else if (event.eventType === "conference.waiting_room_attended") setUnattended(false);
    },
  });

  const handleJoin = async () => {
    setError("");
    if (blocker) { setError(blocker); return; }
    try {
      const data = toRoom(await joinMutation.mutateAsync({ id: conferenceId, data: joinRequest() }));
      if (!data) { setIsWaiting(true); startPolling(); } else onJoin(data, { isCameraOn, isMicOn });
    } catch (cause) {
      setError(joinErrorMessage(cause));
      if ((cause as { response?: { data?: { error?: { code?: string } } } })?.response?.data?.error?.code === "CONSENT_REQUIRED") void preJoin.refetch();
    }
  };

  const handleStart = async () => {
    setError("");
    setStarting(true);
    try {
      await conferencesService.start(conferenceId);
      await queryClient.invalidateQueries({ queryKey: conferenceKeys.detail(conferenceId) });
      await handleJoin();
    } catch (cause) {
      setError(joinErrorMessage(cause));
    } finally {
      setStarting(false);
    }
  };

  const handleCancel = async () => {
    pollingEpoch.current += 1;
    if (pollIntervalRef.current) { clearInterval(pollIntervalRef.current); pollIntervalRef.current = null; }
    setError("");
    try {
      await conferencesService.leave(conferenceId, { participantSessionId: previousSession.current });
      storageStore.clearParticipantToken(); storageStore.clearV2JoinData(); previousSession.current = undefined;
      setIsWaiting(false);
    } catch { setError("Could not close the waiting session. Please retry leaving."); }
  };

  if (preJoin.isLoading && conference.isLoading) {
    return (
      <div className="flex min-h-screen flex-col bg-muted/40">
        <header className="flex h-16 items-center px-5 sm:px-8"><Logo /></header>
        <main className="mx-auto grid w-full max-w-6xl flex-1 items-center gap-6 px-4 pb-10 lg:grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)]" aria-busy="true">
          <div className="aspect-video animate-pulse rounded-3xl bg-muted" />
          <div className="h-80 animate-pulse rounded-3xl bg-muted" />
          <span className="sr-only"><Spinner label="Loading meeting info..." /></span>
        </main>
      </div>
    );
  }
  if (preJoin.isError && conference.isError) {
    return <RoomMessage icon="missing" title="Meeting not found">This meeting doesn&apos;t exist or you don&apos;t have access to it.</RoomMessage>;
  }
  if (phase === "closed") {
    return <RoomMessage icon="ended" title={status === "CANCELLED" ? "Meeting cancelled" : "This meeting has ended"}>The meeting <span className="font-medium text-foreground">&quot;{title}&quot;</span> {status === "CANCELLED" ? "was cancelled" : "has ended"}.</RoomMessage>;
  }

  const schedule = scheduledStartAt ? scheduleWindowLabel(scheduledStartAt, null, meetingZone, viewerTimeZone()) : null;
  const meetingType = preJoin.data?.meetingType ?? space.data?.meetingSpace.meetingType;

  const notices: { icon: typeof ShieldCheck; text: string; tone?: string }[] = [
    ...(requirements.waitingRoom ? [{ icon: ShieldCheck, text: "A host lets people in from the waiting room" }] : []),
    ...(preJoin.data?.notices.recordingActive ? [{ icon: CircleDot, text: "This meeting is being recorded", tone: "text-destructive" }] : []),
    ...(preJoin.data?.notices.transcriptionActive ? [{ icon: Captions, text: "This meeting is being transcribed", tone: "text-amber-600 dark:text-amber-400" }] : []),
    ...(preJoin.data?.notices.aiNotice ? [{ icon: Sparkles, text: "AI features, like summaries, may process this meeting" }] : []),
  ];
  const policy = space.data?.meetingSpace.schedulePolicy;
  const startMs = scheduledStartAt ? Date.parse(scheduledStartAt) : null;
  const endMs = startMs === null ? null : conference.data?.scheduledEndAt ? Date.parse(conference.data.scheduledEndAt) : startMs + 60 * 60_000;
  const opensMs = startMs === null ? null : startMs - (policy?.earlyStartMinutes ?? 15) * 60_000;
  const closesMs = endMs === null ? null : endMs + (policy?.lateStartGraceMinutes ?? 60) * 60_000;
  type Window = "none" | "too-early" | "early" | "due" | "late" | "closed";
  const startWindow: Window = phase !== "not-started" || startMs === null || opensMs === null || endMs === null || closesMs === null
    ? "none"
    : now < opensMs ? "too-early" : now < startMs ? "early" : now <= endMs ? "due" : now <= closesMs ? "late" : "closed";
  const clock = (ms: number | null) => (ms === null ? "" : new Date(ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" }));
  const statusPill = phase === "live"
    ? { label: "Live now", tone: "bg-[var(--success)]/12 text-[var(--success)]", dot: true }
    : timeUntilStart
      ? { label: `Starts in ${countdown(timeUntilStart)}`, tone: "bg-primary/10 text-primary", dot: false }
      : startWindow === "due"
        ? { label: "Due to start now", tone: "bg-primary/10 text-primary", dot: false }
        : startWindow === "late"
          ? { label: `Ran past its end time (${clock(endMs)})`, tone: "bg-amber-500/10 text-amber-700 dark:text-amber-300", dot: false }
          : startWindow === "closed"
            ? { label: "This session's time has passed", tone: "bg-muted text-muted-foreground", dot: false }
            : { label: "Not started yet", tone: "bg-muted text-muted-foreground", dot: false };
  const field = "h-12 rounded-xl px-4 py-0 text-base sm:h-12 sm:px-4 sm:py-0 sm:text-base";

  return (
    <div className="flex min-h-screen flex-col bg-muted/40">
      <header className="flex h-16 items-center justify-between px-5 sm:px-8">
        <Logo />
        <Link href={signedIn ? "/dashboard" : "/"} className="inline-flex items-center gap-1.5 rounded-xl px-3 py-2 text-sm font-medium text-muted-foreground hover:bg-muted hover:text-foreground">
          <ArrowLeft className="size-4" /> {signedIn ? "Home" : "Back"}
        </Link>
      </header>

      <main className="mx-auto grid w-full max-w-6xl flex-1 content-start items-center gap-4 px-4 pb-10 pt-2 sm:px-6 lg:content-center lg:grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)] lg:gap-8 lg:pt-0">
        {/* Camera preview */}
        <section aria-label="Camera preview" className="relative aspect-[4/5] w-full overflow-hidden rounded-3xl sm:aspect-video bg-slate-900 shadow-2xl shadow-slate-900/20 ring-1 ring-black/5">
          {isCameraOn ? (
            <video ref={videoRef} autoPlay playsInline muted className="size-full object-cover" style={{ transform: "scaleX(-1)" }} />
          ) : (
            mediaError ? (
              <div className="flex size-full flex-col items-center justify-center gap-3 px-6 pb-4 text-center">
                <span className="flex size-14 items-center justify-center rounded-2xl bg-white/10 text-white"><VideoOff className="size-6" /></span>
                <p className="text-base font-semibold text-white">{mediaError === "denied" ? "Allow CF Meet to use your camera and microphone" : "No camera or microphone found"}</p>
                <p className="max-w-sm text-sm text-white/60">{mediaError === "denied" ? "Click the camera icon in your browser's address bar, choose Allow, then try again." : "Connect a camera or microphone, then try again. You can still join without them."}</p>
                <button type="button" onClick={retryMedia} className="mt-1 inline-flex h-10 items-center rounded-xl bg-white px-4 text-sm font-semibold text-slate-900 hover:bg-white/90">Try again</button>
              </div>
            ) : (
            <div className="flex size-full flex-col items-center justify-center gap-3 pb-16">
              <Avatar className="size-20 ring-4 ring-white/10 sm:size-28"><AvatarFallback className="bg-primary text-3xl font-bold text-primary-foreground">{(displayName || "Guest").slice(0, 2).toUpperCase()}</AvatarFallback></Avatar>
              <span className="text-sm text-white/60">Your camera is off</span>
            </div>
            )
          )}

          {/* Name + live mic level */}
          <div className="absolute left-4 top-4 flex items-center gap-2 rounded-full bg-black/50 py-1.5 pl-3 pr-3.5 text-sm text-white backdrop-blur">
            {isMicOn ? <Mic className="size-3.5 text-green-400" /> : <MicOff className="size-3.5 text-red-400" />}
            <span className="max-w-40 truncate font-medium">{displayName.trim() || "You"}</span>
            {isMicOn && <span className="flex h-3.5 items-center"><MicVisualizer stream={stream} isOn={isMicOn} barColor="bg-green-400" count={5} /></span>}
          </div>

          {/* Controls */}
          <div className={cn("absolute inset-x-0 bottom-4 flex justify-center gap-3", mediaError && "hidden")}>
            <ToggleButton isOn={isMicOn} onToggle={toggleMic} iconOn={<Mic className="size-5" />} iconOff={<MicOff className="size-5" />} label={isMicOn ? "Turn off microphone" : "Turn on microphone"} />
            <ToggleButton isOn={isCameraOn} onToggle={toggleCamera} iconOn={<Video className="size-5" />} iconOff={<VideoOff className="size-5" />} label={isCameraOn ? "Turn off camera" : "Turn on camera"} />
          </div>
        </section>

        {/* Join card */}
        <section className="rounded-3xl border border-border bg-card p-5 sm:p-8">
          {isWaiting ? (
            <div className="flex flex-col items-center py-6 text-center">
              <span className="relative flex size-24 items-center justify-center">
                <span className="absolute inset-0 animate-ping rounded-full bg-primary/15 [animation-duration:2s]" />
                <span className="absolute inset-3 animate-ping rounded-full bg-primary/20 [animation-duration:2s] [animation-delay:0.5s]" />
                <Avatar className="relative size-16"><AvatarFallback className="bg-primary text-xl font-bold text-primary-foreground">{(displayName || "Guest").slice(0, 2).toUpperCase()}</AvatarFallback></Avatar>
              </span>
              <h1 className="mt-6 text-xl font-bold tracking-tight text-foreground">Waiting for the host</h1>
              <p className="mt-2 max-w-xs text-sm text-muted-foreground">The host will let you into <span className="font-medium text-foreground">{title}</span> soon. You&apos;ll join automatically.</p>
              {unattended && <p role="status" className="mt-3 max-w-xs rounded-xl bg-amber-500/10 px-4 py-2.5 text-sm text-amber-800 dark:text-amber-300">Nobody who can let you in is in the meeting right now. If nobody returns in a few minutes, your request will end and you can try again later.</p>}
              {error && <p role="alert" className="mt-4 w-full rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p>}
              <button type="button" onClick={handleCancel} className="mt-6 inline-flex h-11 items-center rounded-xl border border-border px-5 text-sm font-semibold text-foreground hover:bg-muted">Cancel</button>
            </div>
          ) : (
            <>
              <span className={cn("inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-semibold", statusPill.tone)}>
                {statusPill.dot && <span className="size-1.5 animate-pulse rounded-full bg-[var(--success)]" />}
                <span suppressHydrationWarning>{statusPill.label}</span>
              </span>
              <h1 className="mt-3 break-words text-2xl font-bold tracking-tight text-foreground">{title}</h1>
              <ul className="mt-3 space-y-1.5 text-sm text-muted-foreground">
                {preJoin.data?.organizer?.displayName || preJoin.data?.organization?.name ? (
                  <li className="flex items-center gap-2"><UserRound className="size-4 shrink-0" /> Hosted by {canStart && signedIn ? "you" : [preJoin.data?.organizer?.displayName, preJoin.data?.organization?.name].filter(Boolean).join(" · ")}</li>
                ) : null}
                {schedule ? <li className="flex items-center gap-2" suppressHydrationWarning><CalendarClock className="size-4 shrink-0" /> {schedule.primary}{schedule.secondary ? ` · ${schedule.secondary}` : ""}</li> : null}
                {meetingCode ? <li className="flex items-center gap-2"><KeyRound className="size-4 shrink-0" /> <span className="font-mono text-foreground">{meetingCode}</span></li> : null}
              </ul>
              {(meetingType === "WEBINAR" || meetingType === "TOWN_HALL") && meetingCode && <Link className="mt-3 inline-block text-sm font-semibold text-primary hover:underline" href={`/register/${encodeURIComponent(meetingCode)}?conferenceId=${encodeURIComponent(conferenceId)}`}>Register for this event →</Link>}

              <div className="mt-6 space-y-3 border-t border-border pt-6">
                {signedIn && !editingName && displayName.trim() ? (
                  <div className="flex items-center gap-3 rounded-xl bg-muted/60 px-4 py-3">
                    <Avatar className="size-9"><AvatarFallback className="bg-primary text-sm font-semibold text-primary-foreground">{displayName.slice(0, 2).toUpperCase()}</AvatarFallback></Avatar>
                    <p className="min-w-0 flex-1 truncate text-sm text-muted-foreground">Joining as <span className="font-semibold text-foreground">{displayName}</span></p>
                    {(preJoin.data?.displayName?.editable ?? true) && <button type="button" onClick={() => setEditingName(true)} className="text-sm font-semibold text-primary hover:underline">Change</button>}
                  </div>
                ) : (
                  <label className="grid gap-2 text-sm font-medium text-foreground">
                    Your name
                    <Input type="text" autoFocus={editingName} placeholder="How others will see you" value={displayName} disabled={preJoin.data?.displayName ? !preJoin.data.displayName.editable : false} onChange={(e) => { setDisplayName(e.target.value); setError(""); }} className={field} maxLength={100} />
                  </label>
                )}
                {requirements.passcode && <label className="grid gap-2 text-sm font-medium text-foreground">Passcode<Input type="password" placeholder="Meeting passcode" value={passcode} onChange={e => setPasscode(e.target.value)} className={field} /></label>}
                {requirements.invitation && <label className="grid gap-2 text-sm font-medium text-foreground"><span className="flex items-center gap-1.5"><Ticket className="size-4" /> Invitation</span><Input type="password" autoComplete="off" placeholder="Paste your invitation link or code" value={invitationToken} onChange={e => setInvitationToken(e.target.value)} className={field} /></label>}
                {requirements.signIn && <p className="rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground">This meeting needs a CF Meet account. <Link className="font-semibold text-primary hover:underline" href="/login">Sign in to join</Link></p>}
                {requirements.externalAuthorization && !requirements.alreadyAuthorized && <p className="rounded-xl bg-muted/60 px-4 py-3 text-sm text-muted-foreground">The organizer&apos;s appointment system confirms your access when you join.</p>}
              </div>

              {notices.length > 0 && (
                <ul className="mt-4 space-y-2">
                  {notices.map(({ icon: Icon, text, tone }) => (
                    <li key={text} className="flex items-start gap-2.5 text-sm text-muted-foreground"><Icon className={cn("mt-0.5 size-4 shrink-0", tone)} /> {text}</li>
                  ))}
                </ul>
              )}

              {requirements.consents.length > 0 && (
                <fieldset className="mt-4 space-y-2">
                  <legend className="mb-2 text-sm font-medium text-foreground">Before you join</legend>
                  {requirements.consents.map(kind => (
                    <label key={kind} className={cn("flex cursor-pointer items-start gap-3 rounded-xl border p-3 text-sm transition-colors", consents[kind] ? "border-primary bg-primary/5" : "border-border hover:bg-muted/50")}>
                      <Checkbox className="mt-0.5" checked={!!consents[kind]} onCheckedChange={value => setConsents(current => ({ ...current, [kind]: value === true }))} />
                      <span className="text-foreground">{CONSENT_TEXT[kind] ?? kind.replaceAll("_", " ")}</span>
                    </label>
                  ))}
                </fieldset>
              )}

              {preJoin.isError && <p role="alert" className="mt-4 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">Couldn&apos;t load what this meeting needs. <button type="button" className="font-semibold underline" onClick={() => void preJoin.refetch()}>Try again</button></p>}
              {error && <p role="alert" className="mt-4 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p>}

              <div className="mt-6">
                {phase === "live" ? (
                  <Button className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" onClick={handleJoin} disabled={joinMutation.isPending || preJoin.isLoading || !!blocker} title={blocker ?? undefined}>
                    {joinMutation.isPending ? "Joining…" : "Join now"}
                  </Button>
                ) : startWindow === "closed" ? (
                  <div className="rounded-xl bg-muted/60 px-4 py-4 text-center">
                    <p className="text-sm font-semibold text-foreground">This session can&apos;t be started anymore</p>
                    <p className="mt-1 text-sm text-muted-foreground">
                      {canStart ? <>Its start window closed at {clock(closesMs)}. Add a new session from the meeting page.</> : "Its time has passed. Ask the host for a new time."}
                    </p>
                    {canStart && spaceId && <Link href={`/dashboard/meetings/${spaceId}`} className="mt-3 inline-flex h-10 items-center rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Open meeting page</Link>}
                  </div>
                ) : canStart ? (
                  <>
                    <Button className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" onClick={() => void handleStart()} disabled={startWindow === "too-early" || starting || joinMutation.isPending || !!blocker} title={blocker ?? undefined}>
                      {starting ? "Starting…" : startWindow === "late" ? "Start now" : "Start meeting"}
                    </Button>
                    <p className="mt-2 text-center text-xs text-muted-foreground">
                      {startWindow === "too-early" ? `You can start it from ${clock(opensMs)}, 15 minutes before.`
                        : startWindow === "early" ? "You're the host, so you can start it a little early."
                        : startWindow === "late" ? `It was meant to end at ${clock(endMs)}. You can still start it until ${clock(closesMs)}.`
                        : "You're the host. Start it when you're ready."}
                    </p>
                  </>
                ) : (
                  <>
                    <Button className="h-12 w-full rounded-xl text-base shadow-none" disabled>Waiting for the host to start</Button>
                    <p className="mt-2 text-center text-xs text-muted-foreground">{startWindow === "due" || startWindow === "late" ? "The host hasn't started it yet. " : ""}This page updates on its own when the meeting begins.</p>
                  </>
                )}
                {blocker && phase === "live" && <p className="mt-2 text-center text-xs text-muted-foreground">{blocker}</p>}
              </div>
            </>
          )}
        </section>
      </main>
    </div>
  );
}

function ToggleButton({ isOn, onToggle, iconOn, iconOff, label }: { isOn: boolean; onToggle: () => void; iconOn: ReactNode; iconOff: ReactNode; label: string }) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-label={label}
      aria-pressed={!isOn}
      title={label}
      className={cn(
        "flex size-12 items-center justify-center rounded-full backdrop-blur transition-all hover:scale-105 sm:size-14",
        isOn ? "bg-white/15 text-white ring-1 ring-white/25 hover:bg-white/25" : "bg-destructive text-white hover:bg-destructive/90",
      )}
    >
      {isOn ? iconOn : iconOff}
    </button>
  );
}

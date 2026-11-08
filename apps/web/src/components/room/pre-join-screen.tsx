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
import { Mic, MicOff, Video, VideoOff, Volume2, VolumeX, XCircle, Clock } from "lucide-react";
import { conferenceKeys, useConference, usePreJoin, useJoinConference } from "@/hooks/v2/useConferences";
import { useResolveSpace } from "@/hooks/v2/useSpaces";
import { conferencesService } from "@/services/v2/conferences.service";
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
    <div className="flex items-center justify-center">
      <Image src="/light-mode-cf-meet.png" alt="CF Meet Logo" width={120} height={30} className="h-auto dark:hidden" priority />
      <Image src="/dark-mode-cfmeet.png" alt="CF Meet Logo" width={120} height={30} className="h-auto hidden dark:block" priority />
    </div>
  );
}

export function RoomMessage({ icon, title, children }: { icon: "missing" | "ended"; title: string; children: ReactNode }) {
  return (
    <div className="flex items-center justify-center min-h-screen bg-background">
      <div className="flex flex-col items-center gap-6 w-full max-w-md px-6 text-center">
        <Logo />
        <div className={`w-20 h-20 rounded-full flex items-center justify-center ${icon === "missing" ? "bg-red-100 dark:bg-red-900/20" : "bg-muted"}`}>
          {icon === "missing" ? <XCircle className="w-10 h-10 text-red-500" /> : <Clock className="w-10 h-10 text-muted-foreground" />}
        </div>
        <div className="space-y-2">
          <h1 className="text-2xl font-bold text-foreground">{title}</h1>
          <p className="text-muted-foreground">{children}</p>
        </div>
        <Link href="/dashboard" className="w-full max-w-xs"><Button size="lg" className="w-full">Back to Home</Button></Link>
      </div>
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
  const [isSpeakerOn, setIsSpeakerOn] = useState(true);
  const [isWaiting, setIsWaiting] = useState(false);
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
  const { videoRef, stream, isCameraOn, isMicOn, toggleCamera, toggleMic } = useMediaStream({ video: true, audio: true });

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
    return <div className="flex flex-col items-center justify-center min-h-screen bg-background gap-6"><Logo /><Spinner label="Loading meeting info..." /></div>;
  }
  if (preJoin.isError && conference.isError) {
    return <RoomMessage icon="missing" title="Meeting Not Found">This meeting doesn&apos;t exist or you don&apos;t have access to it.</RoomMessage>;
  }
  if (phase === "closed") {
    return <RoomMessage icon="ended" title={status === "CANCELLED" ? "Meeting Cancelled" : "Meeting Has Ended"}>The meeting <span className="font-medium text-foreground">&quot;{title}&quot;</span> {status === "CANCELLED" ? "was cancelled" : "has ended"}.</RoomMessage>;
  }

  const schedule = scheduledStartAt ? scheduleWindowLabel(scheduledStartAt, null, meetingZone, viewerTimeZone()) : null;
  const meetingType = preJoin.data?.meetingType ?? space.data?.meetingSpace.meetingType;

  return (
    <div className="flex items-center justify-center min-h-screen py-10 bg-background">
      <div className="flex flex-col items-center gap-6 w-full max-w-lg px-6">
        <Logo />

        <div className="text-center space-y-1">
          <h1 className="text-2xl font-bold text-foreground">Ready to join?</h1>
          <p className="text-sm text-muted-foreground">{title}{meetingCode ? <> - <span className="font-medium text-primary">{meetingCode}</span></> : null}</p>
          {preJoin.data?.organizer?.displayName || preJoin.data?.organization?.name ? <p className="text-xs text-muted-foreground">Hosted by {[preJoin.data?.organizer?.displayName, preJoin.data?.organization?.name].filter(Boolean).join(" · ")}</p> : null}
          {schedule ? <p className="text-xs text-muted-foreground" suppressHydrationWarning>{schedule.primary}{schedule.secondary ? ` · ${schedule.secondary}` : ""}</p> : null}
          <div className="flex items-center justify-center gap-2 mt-2">
            {requirements.passcode && <span className="px-2 py-0.5 text-xs bg-amber-100 text-amber-700 rounded-full font-medium">Passcode required</span>}
            {status && <span className={`px-2 py-0.5 text-xs rounded-full font-medium ${status === "LIVE" ? "bg-green-100 text-green-700" : "bg-blue-100 text-blue-700"}`}>{status === "LIVE" ? "Live" : status === "CREATED" ? "Scheduled" : status}</span>}
          </div>
          {(meetingType === "WEBINAR" || meetingType === "TOWN_HALL") && meetingCode && <Link className="mt-2 inline-block text-sm text-primary underline" href={`/register/${encodeURIComponent(meetingCode)}?conferenceId=${encodeURIComponent(conferenceId)}`}>Register for this event</Link>}
        </div>

        <div className="relative w-full aspect-video rounded-2xl overflow-hidden bg-card dark:bg-background shadow-lg">
          {isCameraOn ? (
            <video ref={videoRef} autoPlay playsInline muted className="w-full h-full object-cover mirror" style={{ transform: "scaleX(-1)" }} />
          ) : (
            <div className="w-full h-full flex flex-col items-center justify-center gap-3">
              <Avatar className="h-24 w-24 border-4 border-white/10"><AvatarFallback className="text-3xl font-bold bg-primary/10 text-primary">{(displayName || "Guest").slice(0, 2).toUpperCase()}</AvatarFallback></Avatar>
              <span className="text-white/50 text-sm">Camera is off</span>
            </div>
          )}
          <div className="absolute bottom-3 left-3 bg-black/60 backdrop-blur-sm rounded-full px-3 py-1.5 flex items-center gap-1.5">
            {isMicOn ? <><Mic className="w-3.5 h-3.5 text-green-400" /><div className="h-3.5 flex items-center"><MicVisualizer stream={stream} isOn={isMicOn} barColor="bg-green-400" count={5} /></div></> : <><MicOff className="w-3.5 h-3.5 text-red-400" /><span className="text-white text-xs">Muted</span></>}
          </div>
        </div>

        {!isWaiting && (
          <div className="w-full max-w-sm space-y-3">
            <Input type="text" aria-label="Your name" placeholder="Enter your name" value={displayName} disabled={preJoin.data?.displayName ? !preJoin.data.displayName.editable : false} onChange={(e) => { setDisplayName(e.target.value); setError(""); }} className="text-center h-11" maxLength={100} />
            {requirements.passcode && <Input aria-label="Meeting passcode" type="password" placeholder="Meeting passcode" value={passcode} onChange={e => setPasscode(e.target.value)} />}
            {requirements.invitation && <Input aria-label="Invitation token" type="password" autoComplete="off" placeholder="Invitation token or link" value={invitationToken} onChange={e => setInvitationToken(e.target.value)} />}
            {requirements.signIn && <p className="text-sm text-muted-foreground">This meeting requires a CF Meet account. <Link className="text-primary underline" href="/login">Sign in to join</Link>.</p>}
            {requirements.externalAuthorization && !requirements.alreadyAuthorized && <p className="text-sm text-muted-foreground">Admission is confirmed by the organizer&apos;s appointment system when you join.</p>}
            {requirements.waitingRoom && <p className="text-sm text-muted-foreground">This meeting uses a waiting room. A host may need to admit you.</p>}
            {preJoin.data?.notices.recordingActive && <p className="text-sm text-amber-700 dark:text-amber-400">This meeting is being recorded.</p>}
            {preJoin.data?.notices.transcriptionActive && <p className="text-sm text-amber-700 dark:text-amber-400">This meeting is being transcribed.</p>}
            {preJoin.data?.notices.aiNotice && <p className="text-sm text-muted-foreground">AI features (for example summaries) may process this meeting.</p>}
            {preJoin.isError && <p role="alert" className="text-sm text-destructive">Could not load the admission requirements. <button type="button" className="underline" onClick={() => void preJoin.refetch()}>Retry</button></p>}
            {requirements.consents.length > 0 && <fieldset className="space-y-2 rounded-lg border border-border p-3 text-left"><legend className="px-1 text-xs font-medium text-muted-foreground">Required before joining (version {requirements.consentVersion})</legend>
              {requirements.consents.map(kind => <label key={kind} className="flex items-start gap-2 text-sm"><Checkbox checked={!!consents[kind]} onCheckedChange={value => setConsents(current => ({ ...current, [kind]: value === true }))} /><span>{CONSENT_TEXT[kind] ?? kind.replaceAll("_", " ")}</span></label>)}
            </fieldset>}
            {error && <p role="alert" className="text-sm text-red-500 mt-2 text-center">{error}</p>}
          </div>
        )}

        <div className="flex items-center gap-4">
          <ToggleButton isOn={isMicOn} onToggle={toggleMic} iconOn={<Mic className="w-5 h-5" />} iconOff={<MicOff className="w-5 h-5" />} label="Mic" />
          <ToggleButton isOn={isCameraOn} onToggle={toggleCamera} iconOn={<Video className="w-5 h-5" />} iconOff={<VideoOff className="w-5 h-5" />} label="Camera" />
          <ToggleButton isOn={isSpeakerOn} onToggle={() => setIsSpeakerOn(!isSpeakerOn)} iconOn={<Volume2 className="w-5 h-5" />} iconOff={<VolumeX className="w-5 h-5" />} label="Speaker" />
        </div>

        {isWaiting ? (
          <div className="flex flex-col items-center gap-3">
            {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
            <div className="flex items-center gap-2">
              <span className="relative flex h-3 w-3"><span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-primary opacity-75" /><span className="relative inline-flex rounded-full h-3 w-3 bg-primary" /></span>
              <span className="text-sm font-medium text-foreground">Waiting for the host to let you in...</span>
            </div>
            <Button variant="ghost" size="sm" onClick={handleCancel}>Cancel</Button>
          </div>
        ) : phase === "live" ? (
          <Button size="lg" className="w-full max-w-xs" onClick={handleJoin} disabled={joinMutation.isPending || preJoin.isLoading || !!blocker} title={blocker ?? undefined}>
            {joinMutation.isPending ? "Joining..." : "Join Meeting"}
          </Button>
        ) : canStart ? (
          <div className="flex flex-col items-center gap-2 w-full max-w-xs">
            {timeUntilStart ? <p className="text-sm text-center text-muted-foreground">Scheduled to start in <span className="font-semibold text-primary">{countdown(timeUntilStart)}</span></p> : null}
            <Button size="lg" className="w-full" onClick={() => void handleStart()} disabled={starting || joinMutation.isPending || !!blocker} title={blocker ?? undefined}>{starting ? "Starting..." : "Start meeting"}</Button>
            <p className="text-xs text-center text-muted-foreground">You can start from the early-start window before the scheduled time.</p>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-2 w-full max-w-xs">
            <p className="text-sm text-center text-muted-foreground">
              {timeUntilStart ? <>Meeting starts in <span className="font-semibold text-primary">{countdown(timeUntilStart)}</span>. </> : null}
              Waiting for the host to start the meeting. This page updates automatically.
            </p>
            <Button size="lg" className="w-full" disabled>Meeting Not Started</Button>
          </div>
        )}
      </div>
    </div>
  );
}

function ToggleButton({ isOn, onToggle, iconOn, iconOff, label }: { isOn: boolean; onToggle: () => void; iconOn: ReactNode; iconOff: ReactNode; label: string }) {
  return (
    <button onClick={onToggle} className="flex flex-col items-center gap-1.5 group">
      <div className={`w-12 h-12 rounded-full flex items-center justify-center transition-all ${isOn ? "bg-primary text-white shadow-md" : "bg-muted text-muted-foreground"} group-hover:scale-105`}>{isOn ? iconOn : iconOff}</div>
      <span className={`text-xs font-medium ${isOn ? "text-primary" : "text-muted-foreground"}`}>{label}</span>
    </button>
  );
}

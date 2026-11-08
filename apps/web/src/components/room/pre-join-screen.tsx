"use client";

import {
  type ReactNode,
  useState,
  useEffect,
  useRef,
  useCallback,
  useMemo,
} from "react";
import Image from "next/image";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useConference, usePreJoin } from "@/hooks/v2/useConferences";
import { conferencesService } from "@/services/v2/conferences.service";
import type { ConsentKind } from "@/types/v2";
import { Checkbox } from "@/components/ui/checkbox";
import { Mic, MicOff, Video, VideoOff, Volume2, VolumeX, XCircle, Clock } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Spinner } from "@/components/ui/spinner";
import { useMediaStream } from "@/hooks/useMediaStream";
import { MicVisualizer } from "@/components/ui/mic-visualizer";
import { useProfile } from "@/hooks";
import { useResolveSpace, useJoinConference } from "@/hooks/v2";
import { storageStore } from "@/store";
import { accountSession } from "@/lib/auth/session";
import type { JoinMeetingData } from "@/types/meeting";

interface MediaPreferences {
  isCameraOn: boolean;
  isMicOn: boolean;
}

interface PreJoinScreenProps {
  roomId: string;
  initialInvitationToken?: string;
  selectedConferenceId?: string;
  onJoin: (data: JoinMeetingData, mediaPrefs: MediaPreferences) => void;
}

function Logo() {
  return (
    <div className="flex items-center justify-center">
      <Image
        src="/light-mode-cf-meet.png"
        alt="CF Meet Logo"
        width={120}
        height={30}
        className="h-auto dark:hidden"
        priority
      />
      <Image
        src="/dark-mode-cfmeet.png"
        alt="CF Meet Logo"
        width={120}
        height={30}
        className="h-auto hidden dark:block"
        priority
      />
    </div>
  );
}

function MeetingNotFound({ roomId }: { roomId: string }) {
  return (
    <div className="flex items-center justify-center min-h-screen bg-background">
      <div className="flex flex-col items-center gap-6 w-full max-w-md px-6 text-center">
        <Logo />

        <div className="w-20 h-20 rounded-full bg-red-100 dark:bg-red-900/20 flex items-center justify-center">
          <XCircle className="w-10 h-10 text-red-500" />
        </div>

        <div className="space-y-2">
          <h1 className="text-2xl font-bold text-foreground">Meeting Not Found</h1>
          <p className="text-muted-foreground">
            The meeting <span className="font-medium text-foreground">{roomId}</span> doesn&apos;t exist or you don&apos;t have access to it.
          </p>
        </div>

        <Link href="/dashboard" className="w-full max-w-xs">
          <Button size="lg" className="w-full">
            Back to Home
          </Button>
        </Link>
      </div>
    </div>
  );
}

function MeetingEnded({ roomId, title }: { roomId: string; title?: string }) {
  return (
    <div className="flex items-center justify-center min-h-screen bg-background">
      <div className="flex flex-col items-center gap-6 w-full max-w-md px-6 text-center">
        <Logo />

        <div className="w-20 h-20 rounded-full bg-muted flex items-center justify-center">
          <Clock className="w-10 h-10 text-muted-foreground" />
        </div>

        <div className="space-y-2">
          <h1 className="text-2xl font-bold text-foreground">Meeting Has Ended</h1>
          <p className="text-muted-foreground">
            {title ? (
              <>The meeting <span className="font-medium text-foreground">&quot;{title}&quot;</span> has ended.</>
            ) : (
              <>The meeting <span className="font-medium text-foreground">{roomId}</span> has ended.</>
            )}
          </p>
        </div>

        <Link href="/dashboard" className="w-full max-w-xs">
          <Button size="lg" className="w-full">
            Back to Home
          </Button>
        </Link>
      </div>
    </div>
  );
}

export default function PreJoinScreen({ roomId, onJoin, initialInvitationToken = "", selectedConferenceId }: PreJoinScreenProps) {
  const search = useSearchParams();
  const selectedId = selectedConferenceId || search.get("conference") || "";
  const selected = useConference(selectedId);
  const [passcode, setPasscode] = useState("");
  const [invitationToken, setInvitationToken] = useState(initialInvitationToken);
  const [consents, setConsents] = useState<Partial<Record<ConsentKind, boolean>>>({});
  const previousSession = useRef<string | undefined>(undefined);
  const pollingBusy = useRef(false);
  const pollingEpoch = useRef(0);
  // State for media controls (speaker, camera, mic)
  const [isSpeakerOn, setIsSpeakerOn] = useState(true);

  // State to track if user is in waiting room
  const [isWaiting, setIsWaiting] = useState(false);

  // Get authenticated user info
  const { data: profileData } = useProfile();
  const storedUser = storageStore.getUser();

  // Display name state - auto-populated from profile/storage
  const [displayName, setDisplayName] = useState(
    () => storedUser?.username || "",
  );

  // Track if we've already auto-filled from async profile data
  const [hasAutoFilled, setHasAutoFilled] = useState(false);

  // UI error state
  const [error, setError] = useState("");

  // Ref for polling interval - used to check admission status
  const pollIntervalRef = useRef<NodeJS.Timeout | null>(null);

  // Tick counter to force re-render for countdown (updated by interval)
  const [now, setNow] = useState(() => Date.now());

  const { videoRef, stream, isCameraOn, isMicOn, toggleCamera, toggleMic } =
    useMediaStream({ video: true, audio: true });

  // Get meeting info via V2 resolve (works for V2 codes and legacy codes)
  const { data: canonical, isLoading: loadingSpace, isError: spaceError } =
    useResolveSpace(roomId);
  const currentConference = useConference(selectedId ? "" : canonical?.conference?.id || "");
  const validSelection = !selectedId || selected.data?.meetingSpaceId === canonical?.meetingSpace.id;
  const resolveData = canonical && validSelection ? { ...canonical, conference: selectedId ? selected.data : currentConference.data || canonical.conference } : undefined;
  const isLoadingMeeting = loadingSpace || (!!selectedId && selected.isLoading);
  const isError = spaceError || (!!selectedId && (selected.isError || (!selected.isLoading && !validSelection)));
  const preJoin = usePreJoin(resolveData?.conference?.id || "");
  const consentRequirements = useMemo(
    () => Array.isArray(preJoin.data?.consentRequirements)
      ? preJoin.data.consentRequirements.filter(
          (item) => item && typeof item.consentType === "string",
        )
      : [],
    [preJoin.data?.consentRequirements],
  );
  const grantedConsents = useMemo(
    () => consentRequirements.filter(item => consents[item.consentType])
      .map(item => ({ type: item.consentType, version: item.consentVersion })),
    [consentRequirements, consents],
  );

  // Join conference mutation (V2 API)
  const joinMutation = useJoinConference();

  // Compute time until start from meeting data (re-computed on each tick)
  const timeUntilStart = useMemo(() => {
    const conference = resolveData?.conference;
    // Only show countdown for scheduled conferences that haven't started
    if (!conference?.scheduledStartAt || conference?.startedAt) {
      return null;
    }
    // Check if conference is in a "not yet started" state
    const notStartedStatuses = ["CREATED", "PREPARING", "WAITING"];
    if (!notStartedStatuses.includes(conference.status)) {
      return null;
    }
    const startTime = new Date(conference.scheduledStartAt).getTime();
    const remaining = startTime - now;
    return remaining > 0 ? remaining : 0;
  }, [resolveData?.conference, now]);

  // Auto-fill display name from profile data when it loads (only once)
  // This is a legitimate use case: syncing state from async external source
  useEffect(() => {
    if (profileData?.username && !displayName && !hasAutoFilled) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setHasAutoFilled(true);
      setDisplayName(profileData.username);
    }
  }, [profileData?.username, displayName, hasAutoFilled]);

  // Cleanup polling interval on unmount
  useEffect(() => {
    return () => {
      if (pollIntervalRef.current) {
        clearInterval(pollIntervalRef.current);
      }
      pollingEpoch.current += 1;
    };
  }, []);

  // Countdown timer interval - only triggers tick updates
  useEffect(() => {
    const conference = resolveData?.conference;
    if (!conference?.scheduledStartAt || conference?.startedAt) {
      return;
    }
    const notStartedStatuses = ["CREATED", "PREPARING", "WAITING"];
    if (!notStartedStatuses.includes(conference.status)) {
      return;
    }

    const startTime = new Date(conference.scheduledStartAt).getTime();
    if (Date.now() >= startTime) {
      return; // Meeting already started, no need for countdown
    }

    const intervalId = setInterval(() => {
      setNow(Date.now());
    }, 1000);

    return () => clearInterval(intervalId);
  }, [resolveData?.conference]);

  // Helper to map V2 join response to the JoinMeetingData format expected by LiveRoom
  const mapV2JoinToLegacy = useCallback((v2Response: import("@/types/v2").JoinConferenceResponseV2): JoinMeetingData | null => {
    if (v2Response.participantToken) storageStore.setParticipantToken(v2Response.participantToken);
    previousSession.current = v2Response.participantSession?.id;
    storageStore.setV2JoinData(v2Response);
    // If no RTC credentials, user is in waiting room
    if (!v2Response.rtc) {
      if (v2Response.rtcStatus !== "NOT_ISSUED_WAITING" || v2Response.participant.admissionState !== "WAITING") throw new Error("Media access is unavailable for this conference.");
      return null;
    }
    const rtcCredential = v2Response.rtc.credential;
    return {
      meeting_id: v2Response.conference.meetingSpaceId,
      participant_id: v2Response.participant.id,
      role: ["OWNER", "HOST", "CO_HOST"].includes(v2Response.participant.role) ? "host" : v2Response.participant.role === "VIEWER" ? "viewer" : "participant",
      conference_id: v2Response.conference.id,
      participant_session_id: v2Response.participantSession?.id,
      conference_role: v2Response.participant.role,
      permissions: v2Response.permissions,
      media_context: v2Response.mediaContext,
      is_guest: !accountSession.hasSession(),
      join_token: rtcCredential,
      livekit_url: v2Response.rtc.url,
      room_name: v2Response.conference.id,
    };
  }, []);

  // Poll to check if participant has been admitted to the meeting
  // This runs when a user is placed in the waiting room (isWaiting = true)
  const startPolling = useCallback(() => {
    const epoch = ++pollingEpoch.current;
    // Clear any existing interval to prevent duplicates
    if (pollIntervalRef.current) {
      clearInterval(pollIntervalRef.current);
    }

    const conferenceId = resolveData?.conference?.id;
    if (!conferenceId) return;

    pollIntervalRef.current = setInterval(async () => {
      if (pollingBusy.current) return;
      pollingBusy.current = true;
      try {
        // Attempt to join again to check status.
        // If admitted, the backend will return RTC credentials.
        const result = await joinMutation.mutateAsync({
          id: conferenceId,
          data: {
            displayName: displayName.trim(),
            passcode: passcode || undefined,
            invitationToken: invitationToken || undefined,
            previousSessionId: previousSession.current,
            consents: grantedConsents,
          },
        });
        if (epoch !== pollingEpoch.current) {
          if (result.participantToken) storageStore.setParticipantToken(result.participantToken);
          await conferencesService.leave(conferenceId, { participantSessionId: result.participantSession?.id });
          storageStore.clearParticipantToken(); storageStore.clearV2JoinData();
          return;
        }

        // If we get RTC credentials, it means we've been admitted!
        const joinData = mapV2JoinToLegacy(result);
        if (joinData) {
          if (pollIntervalRef.current) {
            clearInterval(pollIntervalRef.current);
            pollIntervalRef.current = null;
          }
          setIsWaiting(false);
          // Proceed to the main live room with media preferences
          onJoin(joinData, { isCameraOn, isMicOn });
        }
      } catch (cause) {
        const status = (cause as { response?: { status?: number } })?.response?.status;
        if (status && status >= 400 && status < 500) {
          if (pollIntervalRef.current) clearInterval(pollIntervalRef.current);
          setIsWaiting(false);
          setError("Admission is unavailable. Review the meeting status and retry.");
        }
      } finally {
        pollingBusy.current = false;
      }
    }, 3000); // Check every 3 seconds
  }, [resolveData?.conference?.id, displayName, passcode, invitationToken, grantedConsents, joinMutation, onJoin, isCameraOn, isMicOn, mapV2JoinToLegacy]);

  const handleJoin = async () => {
    setError("");

    if (!displayName.trim()) {
      setError("Please enter your name");
      return;
    }

    const conferenceId = resolveData?.conference?.id;
    if (!conferenceId) {
      setError("No active conference found for this meeting");
      return;
    }

    try {
      // Attempt to join the conference via V2 API
      const result = await joinMutation.mutateAsync({
        id: conferenceId,
        data: {
          displayName: displayName.trim(),
          passcode: passcode || undefined,
          invitationToken: invitationToken || undefined,
          previousSessionId: previousSession.current,
          consents: grantedConsents,
        },
      });

      // Map V2 response to legacy format
      const joinData = mapV2JoinToLegacy(result);

      // Check if user is in waiting room (no RTC credentials means waiting)
      if (!joinData) {
        setIsWaiting(true);
        // Start polling for admission status
        startPolling();
      } else {
        // Host or admitted immediately - proceed to room with media preferences
        onJoin(joinData, { isCameraOn, isMicOn });
      }
    } catch (err) {
      console.error("Join error:", err);
      let errorMessage = "Failed to join meeting";
      if (
        err &&
        typeof err === "object" &&
        "response" in err &&
        err.response &&
        typeof err.response === "object" &&
        "data" in err.response
      ) {
        const data = err.response.data as { error?: { message?: string } };
        errorMessage = data?.error?.message || errorMessage;
      } else if (err instanceof Error) {
        errorMessage = err.message;
      }
      setError(errorMessage);
    }
  };

  const handleCancel = async () => {
    pollingEpoch.current += 1;
    if (pollIntervalRef.current) {
      clearInterval(pollIntervalRef.current);
      pollIntervalRef.current = null;
    }
    setError("");
    if (resolveData?.conference?.id) {
      try {
        await conferencesService.leave(resolveData.conference.id, { participantSessionId: previousSession.current });
        storageStore.clearParticipantToken(); storageStore.clearV2JoinData(); previousSession.current = undefined;
        setIsWaiting(false);
      } catch { setError("Could not close the waiting session. Please retry leaving."); }
    }
  };

  if (isLoadingMeeting) {
    return (
      <div className="flex flex-col items-center justify-center min-h-screen bg-background gap-6">
        <Logo />
        <Spinner label="Loading meeting info..." />
      </div>
    );
  }

  // Meeting not found
  if (isError || !resolveData?.meetingSpace) {
    return <MeetingNotFound roomId={roomId} />;
  }

  // Conference ended or cancelled
  const conferenceStatus = resolveData.conference?.status;
  if (conferenceStatus && ["ENDING", "ENDED", "CANCELLED", "FAILED"].includes(conferenceStatus)) {
    return <MeetingEnded roomId={roomId} title={resolveData.meetingSpace.title} />;
  }

  return (
    <div className="flex items-center justify-center min-h-screen py-10 bg-background">
      <div className="flex flex-col items-center gap-6 w-full max-w-lg px-6">
        {/* Logo */}
        <Logo />

        {/* Title */}
        <div className="text-center space-y-1">
          <h1 className="text-2xl font-bold text-foreground">Ready to join?</h1>
          <p className="text-sm text-muted-foreground">
            {resolveData.meetingSpace.title || "Meeting"} -{" "}
            <span className="font-medium text-primary">{roomId}</span>
          </p>
          <div className="flex items-center justify-center gap-2 mt-2">
            {resolveData.meetingSpace.accessPolicy !== "PUBLIC" && (
              <span className="px-2 py-0.5 text-xs bg-amber-100 text-amber-700 rounded-full font-medium">
                {resolveData.meetingSpace.requiresPasscode ? "Passcode Required" : "Private"}
              </span>
            )}
            {conferenceStatus && (
              <span
                className={`px-2 py-0.5 text-xs rounded-full font-medium ${
                  conferenceStatus === "LIVE"
                    ? "bg-green-100 text-green-700"
                    : conferenceStatus === "CREATED" || conferenceStatus === "PREPARING" || conferenceStatus === "WAITING"
                      ? "bg-blue-100 text-blue-700"
                      : "bg-muted text-foreground"
                }`}
              >
                {conferenceStatus === "LIVE" ? "Live" : conferenceStatus === "CREATED" ? "Scheduled" : conferenceStatus}
              </span>
            )}
          </div>
          {(resolveData.meetingSpace.meetingType === "WEBINAR" || resolveData.meetingSpace.meetingType === "TOWN_HALL") && <Link className="mt-2 inline-block text-sm text-primary underline" href={`/register/${encodeURIComponent(resolveData.meetingSpace.meetingCode)}${resolveData.conference?.id ? `?conferenceId=${encodeURIComponent(resolveData.conference.id)}` : ""}`}>Register for this event</Link>}
        </div>

        {/* Camera Preview */}
        <div className="relative w-full aspect-video rounded-2xl overflow-hidden bg-card dark:bg-background shadow-lg">
          {isCameraOn ? (
            <video
              ref={videoRef}
              autoPlay
              playsInline
              muted
              className="w-full h-full object-cover mirror"
              style={{ transform: "scaleX(-1)" }}
            />
          ) : (
            <div className="w-full h-full flex flex-col items-center justify-center gap-3">
              <Avatar className="h-24 w-24 border-4 border-white/10">
                <AvatarFallback className="text-3xl font-bold bg-primary/10 text-primary">
                  {(displayName || "Guest").slice(0, 2).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <span className="text-white/50 text-sm">Camera is off</span>
            </div>
          )}

          {/* Mic status badge */}
          <div className="absolute bottom-3 left-3 bg-black/60 backdrop-blur-sm rounded-full px-3 py-1.5 flex items-center gap-1.5">
            {isMicOn ? (
              <>
                <Mic className="w-3.5 h-3.5 text-green-400" />
                <div className="h-3.5 flex items-center">
                  <MicVisualizer
                    stream={stream}
                    isOn={isMicOn}
                    barColor="bg-green-400"
                    count={5}
                  />
                </div>
              </>
            ) : (
              <>
                <MicOff className="w-3.5 h-3.5 text-red-400" />
                <span className="text-white text-xs">Muted</span>
              </>
            )}
          </div>
        </div>

        {/* Display Name Input */}
        {!isWaiting && (
          <div className="w-full max-w-xs space-y-3">
            <Input
              type="text"
              placeholder="Enter your name"
              value={displayName}
              onChange={(e) => {
                setDisplayName(e.target.value);
                setError("");
              }}
              className="text-center h-11"
              maxLength={100}
            />
            {(preJoin.data?.requiresPasscode || resolveData.meetingSpace.requiresPasscode) && <Input aria-label="Meeting passcode" type="password" placeholder="Meeting passcode" value={passcode} onChange={e => setPasscode(e.target.value)} />}
            {preJoin.data?.requiresInvitation && <Input aria-label="Invitation token" placeholder="Invitation token" value={invitationToken} onChange={e => setInvitationToken(e.target.value)} />}
            {preJoin.data?.requiresAuth && !accountSession.hasSession() && <p className="text-sm text-muted-foreground">Sign in to join this conference.</p>}
            {preJoin.data?.waitingRoomEnabled && <p className="text-sm text-muted-foreground">This conference uses a waiting room. A host may need to admit you before you can join.</p>}
            {preJoin.isError && <p role="alert" className="text-sm text-destructive">Could not load admission requirements. <button type="button" className="underline" onClick={() => void preJoin.refetch()}>Retry</button></p>}
            {consentRequirements.map(item => <label key={item.consentType} className="flex items-start gap-2 text-sm"><Checkbox checked={!!consents[item.consentType]} onCheckedChange={value => setConsents(current => ({ ...current, [item.consentType]: value === true }))} /><span>{item.description || item.consentType.replaceAll("_", " ")}{item.required ? " (required)" : " (optional)"}</span></label>)}
            {error && (
              <p className="text-sm text-red-500 mt-2 text-center">{error}</p>
            )}
          </div>
        )}

        {/* Toggle Controls */}
        <div className="flex items-center gap-4">
          <ToggleButton
            isOn={isMicOn}
            onToggle={toggleMic}
            iconOn={<Mic className="w-5 h-5" />}
            iconOff={<MicOff className="w-5 h-5" />}
            label="Mic"
          />
          <ToggleButton
            isOn={isCameraOn}
            onToggle={toggleCamera}
            iconOn={<Video className="w-5 h-5" />}
            iconOff={<VideoOff className="w-5 h-5" />}
            label="Camera"
          />
          <ToggleButton
            isOn={isSpeakerOn}
            onToggle={() => setIsSpeakerOn(!isSpeakerOn)}
            iconOn={<Volume2 className="w-5 h-5" />}
            iconOff={<VolumeX className="w-5 h-5" />}
            label="Speaker"
          />
        </div>

        {/* Join / Waiting */}
        {isWaiting ? (
          <div className="flex flex-col items-center gap-3">
            {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
            <div className="flex items-center gap-2">
              <span className="relative flex h-3 w-3">
                <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-primary opacity-75" />
                <span className="relative inline-flex rounded-full h-3 w-3 bg-primary" />
              </span>
              <span className="text-sm font-medium text-foreground">
                Waiting for the host to let you in...
              </span>
            </div>
            <Button variant="ghost" size="sm" onClick={handleCancel}>
              Cancel
            </Button>
          </div>
        ) : (
          <>
            {/* Countdown for scheduled meetings that haven't started */}
            {timeUntilStart !== null && timeUntilStart > 0 && (() => {
              const totalSeconds = Math.floor(timeUntilStart / 1000);
              const hours = Math.floor(totalSeconds / 3600);
              const minutes = Math.floor((totalSeconds % 3600) / 60);
              const seconds = totalSeconds % 60;

              let timeDisplay: string;
              if (hours > 0) {
                timeDisplay = `${hours}h ${minutes}m ${seconds}s`;
              } else if (minutes > 0) {
                timeDisplay = `${minutes}m ${seconds}s`;
              } else {
                timeDisplay = `${seconds}s`;
              }

              return (
                <div className="flex flex-col items-center gap-2 w-full max-w-xs">
                  <p className="text-sm text-center text-muted-foreground">
                    Meeting starts in <span className="font-semibold text-primary">{timeDisplay}</span>
                  </p>
                  <Button size="lg" className="w-full" disabled>
                    Meeting Not Started
                  </Button>
                </div>
              );
            })()}

            {(timeUntilStart === null || timeUntilStart <= 0) && (
              <Button
                size="lg"
                className="w-full max-w-xs"
                onClick={handleJoin}
                disabled={joinMutation.isPending || preJoin.isLoading || preJoin.isError || !resolveData.conference || (!!preJoin.data?.requiresAuth && !accountSession.hasSession()) || consentRequirements.some(item => item.required && !consents[item.consentType])}
              >
                {joinMutation.isPending ? "Joining..." : "Join Meeting"}
              </Button>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function ToggleButton({
  isOn,
  onToggle,
  iconOn,
  iconOff,
  label,
}: {
  isOn: boolean;
  onToggle: () => void;
  iconOn: ReactNode;
  iconOff: ReactNode;
  label: string;
}) {
  return (
    <button
      onClick={onToggle}
      className={`flex flex-col items-center gap-1.5 group`}
    >
      <div
        className={`w-12 h-12 rounded-full flex items-center justify-center transition-all ${
          isOn
            ? "bg-primary text-white shadow-md"
            : "bg-muted text-muted-foreground"
        } group-hover:scale-105`}
      >
        {isOn ? iconOn : iconOff}
      </div>
      <span
        className={`text-xs font-medium ${
          isOn ? "text-primary" : "text-muted-foreground"
        }`}
      >
        {label}
      </span>
    </button>
  );
}

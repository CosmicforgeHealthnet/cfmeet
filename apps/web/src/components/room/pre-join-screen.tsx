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
import { Mic, MicOff, Video, VideoOff, Volume2, VolumeX, XCircle, Clock } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Spinner } from "@/components/ui/spinner";
import { useMediaStream } from "@/hooks/useMediaStream";
import { MicVisualizer } from "@/components/ui/mic-visualizer";
import { useProfile } from "@/hooks";
import { useResolveSpace, useJoinConference } from "@/hooks/v2";
import { storageStore, cookieStore } from "@/store";
import type { JoinMeetingData } from "@/types/meeting";

interface MediaPreferences {
  isCameraOn: boolean;
  isMicOn: boolean;
}

interface PreJoinScreenProps {
  roomId: string;
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

export default function PreJoinScreen({ roomId, onJoin }: PreJoinScreenProps) {
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
  const [tick, setTick] = useState(0);

  const { videoRef, stream, isCameraOn, isMicOn, toggleCamera, toggleMic } =
    useMediaStream({ video: true, audio: true });

  // Get meeting info via V2 resolve (works for V2 codes and legacy codes)
  const { data: resolveData, isLoading: isLoadingMeeting, isError } =
    useResolveSpace(roomId);

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
    const remaining = startTime - Date.now();
    return remaining > 0 ? remaining : 0;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resolveData?.conference?.status, resolveData?.conference?.scheduledStartAt, resolveData?.conference?.startedAt, tick]);

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
      setTick((t) => t + 1);
    }, 1000);

    return () => clearInterval(intervalId);
  }, [resolveData?.conference?.status, resolveData?.conference?.scheduledStartAt, resolveData?.conference?.startedAt]);

  // Helper to map V2 join response to the JoinMeetingData format expected by LiveRoom
  const mapV2JoinToLegacy = useCallback((v2Response: Awaited<ReturnType<typeof joinMutation.mutateAsync>>): JoinMeetingData | null => {
    // If no RTC credentials, user is in waiting room
    if (!v2Response.rtc) {
      return null;
    }
    return {
      meeting_id: v2Response.conference.meetingSpaceId,
      participant_id: v2Response.participant.id,
      role: v2Response.participant.role.toLowerCase() as "host" | "participant" | "viewer",
      join_token: v2Response.rtc.credential,
      livekit_url: v2Response.rtc.url,
      room_name: v2Response.conference.id,
    };
  }, []);

  // Poll to check if participant has been admitted to the meeting
  // This runs when a user is placed in the waiting room (isWaiting = true)
  const startPolling = useCallback(() => {
    // Clear any existing interval to prevent duplicates
    if (pollIntervalRef.current) {
      clearInterval(pollIntervalRef.current);
    }

    const conferenceId = resolveData?.conference?.id;
    if (!conferenceId) return;

    pollIntervalRef.current = setInterval(async () => {
      try {
        // Attempt to join again to check status.
        // If admitted, the backend will return RTC credentials.
        const result = await joinMutation.mutateAsync({
          id: conferenceId,
          data: {
            displayName: displayName.trim(),
          },
        });

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
      } catch {
        // Still in waiting room or temporary network error - continue polling
      }
    }, 3000); // Check every 3 seconds
  }, [resolveData?.conference?.id, displayName, joinMutation, onJoin, isCameraOn, isMicOn, mapV2JoinToLegacy]);

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

  const handleCancel = () => {
    if (pollIntervalRef.current) {
      clearInterval(pollIntervalRef.current);
      pollIntervalRef.current = null;
    }
    setIsWaiting(false);
    setError("");
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
  if (conferenceStatus === "ENDED" || conferenceStatus === "CANCELLED") {
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
          <div className="w-full max-w-xs">
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
                disabled={joinMutation.isPending}
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

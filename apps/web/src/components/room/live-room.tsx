"use client";

import { useEffect, useState, useRef, useCallback, type MouseEvent } from "react";
import {
  LiveKitRoom,
  GridLayout,
  ParticipantTile,
  RoomAudioRenderer,
  useTracks,
  LayoutContextProvider,
  useParticipantContext,
  useMaybeTrackRefContext,
  VideoTrack,
  ParticipantName,
  TrackMutedIndicator,
  useRoomContext,
  type TrackReference,
} from "@livekit/components-react";
import { Track, RoomEvent } from "livekit-client";
import Sidebar from "@/components/room/sidebar";
import FooterControls from "@/components/room/footer-controls";
import { ChatProvider } from "@/components/room/ChatProvider";
import type { JoinMeetingData } from "@/types/meeting";
import { useRouter } from "next/navigation";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Maximize, Minimize, UserPlus, UserMinus } from "lucide-react";
import { usePublicMeeting } from "@/hooks";
import { cookieStore } from "@/store";
import { useQueryClient } from "@tanstack/react-query";

// Notification type for participant events
interface ParticipantNotification {
  id: string;
  name: string;
  type: "joined" | "left";
}

// Tile background colors for participants without video
const TILE_COLORS = [
  "bg-orange-100",
  "bg-blue-100",
  "bg-green-100",
  "bg-purple-100",
  "bg-pink-100",
  "bg-yellow-100",
  "bg-sky-100",
  "bg-rose-100",
  "bg-teal-100",
  "bg-indigo-100",
  "bg-amber-100",
  "bg-emerald-100",
  "bg-cyan-100",
  "bg-violet-100",
  "bg-lime-100",
  "bg-fuchsia-100",
] as const;

function getParticipantColor(identity: string): string {
  let hash = 0;
  for (let i = 0; i < identity.length; i++) {
    hash = identity.charCodeAt(i) + ((hash << 5) - hash);
  }
  const index = Math.abs(hash) % TILE_COLORS.length;
  return TILE_COLORS[index];
}

// ...

interface LiveRoomProps {
  joinData: JoinMeetingData;
  initialVideo?: boolean;
  initialAudio?: boolean;
}
export default function LiveRoom({ joinData, initialVideo = true, initialAudio = true }: LiveRoomProps) {
  const router = useRouter();
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const isDisconnectingRef = useRef(false);

  // Guest detection: backend only returns access_token for guests
  // Authenticated users (host or logged-in participants) don't receive tokens in join response
  const isGuest = !!joinData.access_token;

  // Poll meeting status to auto-redirect if meeting ends
  // We use usePublicMeeting because useMeeting requires authentication,
  // which would fail for guest users.
  const { data: meetingData } = usePublicMeeting(joinData.room_name);

  useEffect(() => {
    if (isDisconnectingRef.current) return;
    if (meetingData?.data?.status === "ended") {
      isDisconnectingRef.current = true;
      if (isGuest) {
        cookieStore.clearTokens();
        router.replace("/");
      } else {
        router.replace("/dashboard");
      }
    }
  }, [meetingData, router, isGuest]);

  const handleDisconnect = useCallback(() => {
    if (isDisconnectingRef.current) return;
    isDisconnectingRef.current = true;
    if (isGuest) {
      // Guest: clear temporary tokens and go to home
      cookieStore.clearTokens();
      router.replace("/");
    } else {
      // Authenticated user/host: keep tokens and go to dashboard
      router.replace("/dashboard");
    }
  }, [router, isGuest]);

  if (!joinData.join_token) {
    return (
      <div className="flex items-center justify-center h-screen">
        <p>Invalid Join Token</p>
      </div>
    );
  }

  return (
    <LayoutContextProvider>
      <LiveKitRoom
        video={initialVideo}
        audio={initialAudio}
        token={joinData.join_token}
        serverUrl={joinData.livekit_url}
        data-lk-theme="default"
        style={{ height: "100vh" }}
        onDisconnected={handleDisconnect}
      >
        <ChatProvider meetingId={joinData.meeting_id} participantId={joinData.participant_id}>
          <RoomContent
            joinData={joinData}
            isSidebarOpen={isSidebarOpen}
            setIsSidebarOpen={setIsSidebarOpen}
          />
        </ChatProvider>
        <RoomAudioRenderer />
      </LiveKitRoom>
    </LayoutContextProvider>
  );
}

// Room content with participant notifications
interface RoomContentProps {
  joinData: JoinMeetingData;
  isSidebarOpen: boolean;
  setIsSidebarOpen: (open: boolean) => void;
}

function RoomContent({ joinData, isSidebarOpen, setIsSidebarOpen }: RoomContentProps) {
  const room = useRoomContext();
  const queryClient = useQueryClient();
  const [notifications, setNotifications] = useState<ParticipantNotification[]>([]);

  // Listen for participant join/leave events
  useEffect(() => {
    if (!room) return;

    const handleParticipantConnected = (participant: { identity: string; name?: string }) => {
      const name = participant.name || participant.identity || "Someone";
      const notification: ParticipantNotification = {
        id: `${participant.identity}-${Date.now()}`,
        name,
        type: "joined",
      };
      setNotifications((prev) => [...prev, notification]);

      // Invalidate participants query to refresh the list
      queryClient.invalidateQueries({ queryKey: ["participants", joinData.meeting_id] });

      // Auto-remove notification after 3 seconds
      setTimeout(() => {
        setNotifications((prev) => prev.filter((n) => n.id !== notification.id));
      }, 3000);
    };

    const handleParticipantDisconnected = (participant: { identity: string; name?: string }) => {
      const name = participant.name || participant.identity || "Someone";
      const notification: ParticipantNotification = {
        id: `${participant.identity}-${Date.now()}`,
        name,
        type: "left",
      };
      setNotifications((prev) => [...prev, notification]);

      // Invalidate participants query to refresh the list
      queryClient.invalidateQueries({ queryKey: ["participants", joinData.meeting_id] });

      // Auto-remove notification after 3 seconds
      setTimeout(() => {
        setNotifications((prev) => prev.filter((n) => n.id !== notification.id));
      }, 3000);
    };

    room.on(RoomEvent.ParticipantConnected, handleParticipantConnected);
    room.on(RoomEvent.ParticipantDisconnected, handleParticipantDisconnected);

    return () => {
      room.off(RoomEvent.ParticipantConnected, handleParticipantConnected);
      room.off(RoomEvent.ParticipantDisconnected, handleParticipantDisconnected);
    };
  }, [room, queryClient, joinData.meeting_id]);

  return (
    <>
      <div className="flex h-screen bg-white overflow-hidden">
        <div className="flex flex-col flex-1 overflow-hidden">
          {/* Main Video Area */}
          <main className="flex-1 overflow-hidden transition-all duration-300 ease-in-out relative bg-gray-100">
            <VideoGridContent />

            {/* Participant Notifications */}
            <div className="absolute top-4 left-1/2 -translate-x-1/2 z-50 flex flex-col gap-2">
              {notifications.map((notification) => (
                <div
                  key={notification.id}
                  className={`flex items-center gap-2 px-4 py-2 rounded-full shadow-lg animate-fade-in ${
                    notification.type === "joined"
                      ? "bg-green-500 text-white"
                      : "bg-red-500 text-white"
                  }`}
                >
                  {notification.type === "joined" ? (
                    <UserPlus size={16} />
                  ) : (
                    <UserMinus size={16} />
                  )}
                  <span className="text-sm font-medium">
                    {notification.name} {notification.type === "joined" ? "joined" : "left"} the call
                  </span>
                </div>
              ))}
            </div>
          </main>

          {/* Custom Footer Controls */}
          <FooterControls
            onToggleSidebar={() => setIsSidebarOpen(!isSidebarOpen)}
            isSidebarOpen={isSidebarOpen}
            meetingId={joinData.meeting_id}
            participantId={joinData.participant_id}
          />
        </div>

        {/* Conditional Sidebar */}
        {isSidebarOpen && (
          <aside className="fixed inset-y-0 right-0 z-50 w-[85%] sm:w-80 bg-white sm:bg-transparent shadow-2xl sm:shadow-none sm:relative sm:z-0 border-l sm:border-none border-gray-200 flex flex-col h-full transition-all duration-300 sm:p-4">
            <Sidebar
              meetingId={joinData.meeting_id}
              isHost={joinData.role === "host"}
              participantId={joinData.participant_id}
              onClose={() => setIsSidebarOpen(false)}
            />
          </aside>
        )}
      </div>
    </>
  );
}

// Calculate optimal grid layout for square-ish tiles
function getGridLayout(count: number, isMobile: boolean) {
  if (count === 0) return { cols: 1, rows: 1 };
  if (count === 1) return { cols: 1, rows: 1 };
  if (count === 2) return isMobile ? { cols: 1, rows: 2 } : { cols: 2, rows: 1 };
  if (count === 3) return { cols: 2, rows: 2 };
  if (count === 4) return { cols: 2, rows: 2 };
  if (count <= 6) return isMobile ? { cols: 2, rows: 3 } : { cols: 3, rows: 2 };
  if (count <= 9) return { cols: 3, rows: 3 };
  if (count <= 10) return isMobile ? { cols: 2, rows: 5 } : { cols: 5, rows: 2 };
  // For more participants, calculate optimal grid
  const cols = isMobile ? 2 : Math.min(5, Math.ceil(Math.sqrt(count)));
  const rows = Math.ceil(count / cols);
  return { cols, rows };
}

function VideoGridContent() {
  const [isMobile, setIsMobile] = useState(false);

  useEffect(() => {
    const checkMobile = () => setIsMobile(window.innerWidth < 768);
    checkMobile();
    window.addEventListener("resize", checkMobile);
    return () => window.removeEventListener("resize", checkMobile);
  }, []);

  const cameraTracks = useTracks(
    [{ source: Track.Source.Camera, withPlaceholder: true }],
    { onlySubscribed: false },
  );

  const screenShareTracks = useTracks(
    [{ source: Track.Source.ScreenShare, withPlaceholder: false }],
    { onlySubscribed: false },
  );

  const hasScreenShare = screenShareTracks.length > 0;
  const { cols, rows } = getGridLayout(cameraTracks.length, isMobile);

  if (hasScreenShare) {
    const screenShareParticipantLayout = getGridLayout(cameraTracks.length, isMobile);
    return (
      <div className="flex flex-col md:flex-row h-full w-full overflow-hidden">
        {/* Screen share - takes half the screen */}
        <div className="h-1/2 md:h-full md:w-1/2 p-2 overflow-hidden">
          <GridLayout tracks={screenShareTracks} style={{ height: "100%" }} className="border-2 border-[#272EA766] rounded-lg">
            <ParticipantTile>
              <CustomTileContent />
            </ParticipantTile>
          </GridLayout>
        </div>
        {/* Participants grid - other half */}
        <div className="h-1/2 md:h-full md:w-1/2 p-2 overflow-y-auto overflow-x-hidden">
          <div
            className="grid gap-2 h-full w-full place-items-center"
            style={{
              gridTemplateColumns: `repeat(${screenShareParticipantLayout.cols}, 1fr)`,
              gridTemplateRows: `repeat(${screenShareParticipantLayout.rows}, 1fr)`,
            }}
          >
            {cameraTracks.map((track) => (
              <ParticipantTile key={track.participant.sid} trackRef={track} className="rounded-lg overflow-hidden w-full h-full border-2 border-[#272EA766] aspect-square max-h-full">
                <CustomTileContent />
              </ParticipantTile>
            ))}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full w-full p-2 md:p-4 overflow-y-auto overflow-x-hidden">
      <div
        className="grid gap-2 md:gap-3 h-full w-full place-items-center"
        style={{
          gridTemplateColumns: `repeat(${cols}, 1fr)`,
          gridTemplateRows: `repeat(${rows}, 1fr)`,
        }}
      >
        {cameraTracks.map((track) => (
          <ParticipantTile key={track.participant.sid} trackRef={track} className="rounded-lg overflow-hidden w-full h-full border-2 border-[#272EA766]">
            <CustomTileContent />
          </ParticipantTile>
        ))}
      </div>
    </div>
  );
}

function CustomTileContent() {
  const participant = useParticipantContext();
  const trackRef = useMaybeTrackRefContext();
  const tileRef = useRef<HTMLDivElement>(null);
  const [isFullscreen, setIsFullscreen] = useState(false);

  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsFullscreen(document.fullscreenElement === tileRef.current);
    };
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    return () => document.removeEventListener("fullscreenchange", handleFullscreenChange);
  }, []);

  const toggleFullscreen = (e: MouseEvent) => {
    e.stopPropagation();
    if (!tileRef.current) return;

    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else {
      tileRef.current.requestFullscreen();
    }
  };

  if (!participant) return null;

  const isScreenShare = trackRef?.source === Track.Source.ScreenShare;
  const cameraPublication = participant.getTrackPublication(
    Track.Source.Camera,
  );
  const micPublication = participant.getTrackPublication(
    Track.Source.Microphone,
  );

  // Handle screen share tracks
  if (isScreenShare && trackRef) {
    return (
      <div
        ref={tileRef}
        className="relative w-full h-full group bg-black"
      >
        <VideoTrack
          trackRef={trackRef as TrackReference}
          className="w-full h-full object-contain bg-black"
        />
        {/* Fullscreen icon button */}
        <button
          onClick={toggleFullscreen}
          className="absolute top-3 right-3 bg-black/60 backdrop-blur-sm text-white p-2 rounded-lg cursor-pointer hover:bg-black/80 transition-all"
        >
          {isFullscreen ? <Minimize size={20} /> : <Maximize size={20} />}
        </button>
        {/* Screen share label */}
        <div className="absolute bottom-3 left-3 flex items-center gap-2">
          <div className="bg-black/60 backdrop-blur-sm text-white px-3 py-1.5 rounded-full text-sm flex items-center gap-2">
            <span className="text-green-400">●</span>
            <ParticipantName />
            <span className="text-xs opacity-70">(Screen)</span>
          </div>
        </div>
      </div>
    );
  }

  // Handle camera tracks (default behavior)
  return (
    <div
      ref={tileRef}
      className="relative w-full h-full group"
    >
      {participant.isCameraEnabled && cameraPublication ? (
        <VideoTrack
          trackRef={
            {
              participant,
              source: Track.Source.Camera,
              publication: cameraPublication,
            } as TrackReference
          }
          className="w-full h-full object-cover"
        />
      ) : (
        <div className={`w-full h-full flex items-center justify-center ${getParticipantColor(participant.identity)}`}>
          <Avatar className="h-24 w-24 border-4 border-white/30 bg-transparent">
            <AvatarFallback className="text-3xl font-bold bg-white/80 text-gray-700">
              {(participant.name || participant.identity || "Guest")
                .slice(0, 2)
                .toUpperCase()}
            </AvatarFallback>
          </Avatar>
        </div>
      )}

      {/* Fullscreen icon button */}
      <button
        onClick={toggleFullscreen}
        className="absolute top-3 right-3 bg-black/60 backdrop-blur-sm text-white p-2 rounded-lg cursor-pointer hover:bg-black/80 transition-all opacity-0 group-hover:opacity-100"
      >
        {isFullscreen ? <Minimize size={20} /> : <Maximize size={20} />}
      </button>

      {/* Overlay info */}
      <div className="absolute bottom-3 left-3 flex items-center gap-2">
        <div className="bg-black/60 backdrop-blur-sm text-white px-3 py-1.5 rounded-full text-sm flex items-center gap-2">
          <ParticipantName />
          {micPublication && (
            <TrackMutedIndicator
              trackRef={
                {
                  participant,
                  source: Track.Source.Microphone,
                  publication: micPublication,
                } as TrackReference
              }
              className="opacity-70"
            />
          )}
        </div>
      </div>
    </div>
  );
}

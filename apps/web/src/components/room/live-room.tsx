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
  useParticipants,
  type TrackReference,
} from "@livekit/components-react";
import { Track, RoomEvent } from "livekit-client";
import Image from "next/image";
import { Grid2X2, CircleHelp, ChartNoAxesColumn, GitFork, Clapperboard, Volume2, Moon, Sun } from "lucide-react";
import { DialogClose } from "@/components/ui/dialog";
import { useConference } from "@/hooks/v2/useConferences";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useCollaborationState } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import { useTheme } from "next-themes";
import type { SidebarPanel } from "./sidebar";
import { useMediaDeviceSelect } from "@livekit/components-react";
import Sidebar from "@/components/room/sidebar";
import FooterControls from "@/components/room/footer-controls";
import type { RoomJoinData } from "@/lib/prejoin";
import { useRouter } from "next/navigation";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Maximize, Minimize, UserPlus, UserMinus, MicOff } from "lucide-react";
import { storageStore } from "@/store";
import { conferencesService } from "@/services/v2/conferences.service";
import ConferenceControls from "./conference-controls";
import { useQueryClient } from "@tanstack/react-query";
import RealtimeRoomSync from "./RealtimeRoomSync";
import { participantKeys } from "@/hooks/v2/useParticipants";
import { ReactionOverlay } from "./RoomEngagementControls";

// Notification type for participant events
interface ParticipantNotification {
  id: string;
  name: string;
  type: "joined" | "left";
}

// Tile background colors for participants without video
const TILE_COLORS = [
  "bg-blue-100 text-blue-950 dark:bg-blue-900 dark:text-blue-50",
  "bg-emerald-100 text-emerald-950 dark:bg-emerald-900 dark:text-emerald-50",
  "bg-amber-100 text-amber-950 dark:bg-amber-900 dark:text-amber-50",
  "bg-violet-100 text-violet-950 dark:bg-violet-900 dark:text-violet-50",
  "bg-rose-100 text-rose-950 dark:bg-rose-900 dark:text-rose-50",
  "bg-cyan-100 text-cyan-950 dark:bg-cyan-900 dark:text-cyan-50",
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
  joinData: RoomJoinData;
  initialVideo?: boolean;
  initialAudio?: boolean;
}

/**
 * The live room of one conference (FE-017: keyed by the conference id). Media is LiveKit with
 * the backend-issued credential; chat, presence and controls use v2 conference endpoints only
 * (FE-014 / MTG-019: no legacy v1 meeting, chat or media writes).
 */
export default function LiveRoom({ joinData, initialVideo = true, initialAudio = true }: LiveRoomProps) {
  const router = useRouter();
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const isDisconnectingRef = useRef(false);
  const isRefreshingCredentialRef = useRef(false);
  const sessionClosed = useRef(false);
  const queryClient = useQueryClient();
  const conference = useConference(joinData.conferenceId);
  const closeSession = useCallback(async () => {
    if (sessionClosed.current) return;
    await conferencesService.leave(joinData.conferenceId, { participantSessionId: joinData.participantSessionId });
    sessionClosed.current = true;
    await queryClient.invalidateQueries({ queryKey: ["v2", "conferences"] });
  }, [joinData.conferenceId, joinData.participantSessionId, queryClient]);

  const leaveTo = joinData.isGuest ? "/" : "/dashboard";

  const handleDisconnect = useCallback(async () => {
    if (isDisconnectingRef.current) return;
    isDisconnectingRef.current = true;
    try {
      await closeSession();
    } finally {
      storageStore.clearParticipantToken();
      storageStore.clearV2JoinData();
      router.replace(leaveTo);
    }
  }, [router, leaveTo, closeSession]);

  // The conference ended for everyone (v2 status): leave the room.
  const status = conference.data?.status;
  useEffect(() => {
    if (isDisconnectingRef.current || !status || !["ENDED", "CANCELLED", "FAILED"].includes(status)) return;
    isDisconnectingRef.current = true;
    sessionClosed.current = true;
    storageStore.clearParticipantToken();
    storageStore.clearV2JoinData();
    router.replace(leaveTo);
  }, [status, router, leaveTo]);

  if (!joinData.rtcToken) {
    return (
      <div className="flex items-center justify-center h-screen">
        <p>Invalid Join Token</p>
      </div>
    );
  }

  return (
    <LayoutContextProvider>
      <LiveKitRoom
        video={initialVideo && (joinData.permissions?.videoPublish ?? true)}
        audio={initialAudio && (joinData.permissions?.audioPublish ?? true)}
        token={joinData.rtcToken}
        serverUrl={joinData.rtcUrl}
        data-lk-theme="default"
        style={{ height: "100vh" }}
        onDisconnected={() => {
          if (!isRefreshingCredentialRef.current) {
            void handleDisconnect().catch(() => { /* Session expiration also closes a disconnected session. */ });
          }
        }}
      >
        <RealtimeRoomSync conferenceId={joinData.conferenceId} />
        <RoomContent joinData={joinData} isSidebarOpen={isSidebarOpen} setIsSidebarOpen={setIsSidebarOpen} onLeave={closeSession} onCredentialRefreshStateChange={value => { isRefreshingCredentialRef.current = value; }} />
        <RoomAudioRenderer />
      </LiveKitRoom>
    </LayoutContextProvider>
  );
}

// Room content with participant notifications
interface RoomContentProps {
  joinData: RoomJoinData;
  isSidebarOpen: boolean;
  setIsSidebarOpen: (open: boolean) => void;
  onLeave?: () => Promise<void>;
  onCredentialRefreshStateChange: (refreshing: boolean) => void;
}

function RoomContent({ joinData, isSidebarOpen, setIsSidebarOpen, onLeave, onCredentialRefreshStateChange }: RoomContentProps) {
  const room = useRoomContext();
  const queryClient = useQueryClient();
  const participants = useParticipants();
  const [sidebarPanel, setSidebarPanel] = useState<SidebarPanel>("people");
  const [layout, setLayout] = useState<"grid" | "comfortable">("grid");
  const openPanel = (panel: SidebarPanel) => { setSidebarPanel(panel); setIsSidebarOpen(!(isSidebarOpen && sidebarPanel === panel)); };
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
      queryClient.invalidateQueries({ queryKey: participantKeys.list(joinData.conferenceId) });

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
      queryClient.invalidateQueries({ queryKey: participantKeys.list(joinData.conferenceId) });

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
  }, [room, queryClient, joinData.conferenceId]);

  return (
    <>
      <div className="room-space flex h-dvh bg-background text-foreground overflow-hidden">
        <div className="flex min-w-0 flex-col flex-1 overflow-hidden">
          <header className="flex h-16 shrink-0 items-center justify-between gap-3 border-b border-border bg-card/80 px-4 sm:px-6"><div className="flex min-w-0 items-center gap-3 sm:gap-5"><Image src="/CFLogo.png" alt="CF Meet" width={110} height={40} className="h-9 w-auto object-contain" priority /><span className="h-5 w-px bg-border" /><button onClick={() => openPanel("people")} className="flex items-center gap-2 text-xs text-muted-foreground sm:text-sm"><span className="size-1.5 rounded-full bg-emerald-500" />{participants.length}<span className="hidden min-[380px]:inline">participants</span></button></div><label className="flex items-center gap-2 rounded-full border border-border bg-muted/40 px-3 py-2 text-xs"><Grid2X2 className="size-4" /><select aria-label="Grid layout" value={layout} onChange={event => setLayout(event.target.value as typeof layout)} className="bg-transparent outline-none"><option className="bg-card" value="grid">Grid</option><option className="bg-card" value="comfortable">Comfortable</option></select></label></header>
          {/* Main Video Area */}
          <main className="flex-1 overflow-hidden transition-all duration-300 ease-in-out relative bg-background">
            <VideoGridContent layout={layout} />
            <ReactionOverlay />

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
            onOpenPanel={openPanel}
            sidebarPanel={sidebarPanel}
            moreControls={<ConferenceControls id={joinData.conferenceId} role={joinData.conferenceRole} sessionId={joinData.participantSessionId} initialMediaContext={joinData.mediaContext} onCredentialRefreshStateChange={onCredentialRefreshStateChange} tools={<RoomTools conferenceId={joinData.conferenceId} onOpenPanel={openPanel} />} />}
            isSidebarOpen={isSidebarOpen}
            conferenceId={joinData.conferenceId}
            permissions={joinData.permissions}
            onLeave={onLeave}
          />
        </div>

        {/* Conditional Sidebar */}
        {isSidebarOpen && (
          <aside className="fixed inset-y-0 right-0 z-50 w-[85%] sm:w-80 bg-card sm:bg-transparent shadow-2xl sm:shadow-none sm:relative sm:z-0 border-l sm:border-none border-border flex flex-col h-full transition-all duration-300 sm:p-4">
            <Sidebar
              key={sidebarPanel}
              initialPanel={sidebarPanel}
              conferenceId={joinData.conferenceId}
              participantId={joinData.participantId}
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
  if (count === 3) return isMobile ? { cols: 1, rows: 3 } : { cols: 2, rows: 2 };
  if (count === 4) return isMobile ? { cols: 1, rows: 4 } : { cols: 2, rows: 2 };
  if (count <= 6) return isMobile ? { cols: 2, rows: 3 } : { cols: 3, rows: 2 };
  if (count <= 9) return isMobile ? { cols: 2, rows: Math.ceil(count / 2) } : { cols: 3, rows: 3 };
  if (count <= 10) return isMobile ? { cols: 2, rows: 5 } : { cols: 5, rows: 2 };
  // For more participants, calculate optimal grid
  const cols = isMobile ? 2 : Math.min(5, Math.ceil(Math.sqrt(count)));
  const rows = Math.ceil(count / cols);
  return { cols, rows };
}

function VideoGridContent({ layout }: { layout: "grid" | "comfortable" }) {
  const [isMobile, setIsMobile] = useState(false);
  const gridContainer = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = gridContainer.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => setIsMobile(entry.contentRect.width < 640));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const cameraTracks = useTracks(
    [{ source: Track.Source.Camera, withPlaceholder: true }],
    { onlySubscribed: false },
  );

  const screenShareTracks = useTracks(
    [{ source: Track.Source.ScreenShare, withPlaceholder: false }],
    { onlySubscribed: false },
  );

  const hasScreenShare = (screenShareTracks ?? []).length > 0;
  const baseLayout = getGridLayout((cameraTracks ?? []).length, isMobile);
  const cols = layout === "comfortable" ? Math.min(2, baseLayout.cols) : baseLayout.cols;
  const rows = Math.ceil(cameraTracks.length / cols);

  if (hasScreenShare) {
    const screenShareParticipantLayout = getGridLayout(cameraTracks.length, isMobile);
    return (
      <div ref={gridContainer} className="flex flex-col md:flex-row h-full w-full overflow-hidden">
        {/* Screen share - takes half the screen */}
        <div className="h-1/2 md:h-full md:w-1/2 p-2 overflow-hidden">
          <GridLayout tracks={screenShareTracks} style={{ height: "100%" }} className="room-tile rounded-lg">
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
              gridTemplateRows: `repeat(${screenShareParticipantLayout.rows}, minmax(130px, 1fr))`,
            }}
          >
            {(cameraTracks ?? []).map((track) => (
              <ParticipantTile key={track.participant.sid} trackRef={track} className="rounded-lg overflow-hidden w-full h-full room-tile min-h-32">
                <CustomTileContent />
              </ParticipantTile>
            ))}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div ref={gridContainer} className="h-full w-full p-2 md:p-4 overflow-y-auto overflow-x-hidden">
      <div
        className="grid gap-2 md:gap-3 h-full w-full place-items-center"
        style={{
          gridTemplateColumns: `repeat(${cols}, 1fr)`,
          gridTemplateRows: `repeat(${rows}, minmax(150px, 1fr))`,
        }}
      >
        {(cameraTracks ?? []).map((track) => (
          <ParticipantTile key={track.participant.sid} trackRef={track} className="rounded-lg overflow-hidden w-full h-full room-tile">
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
          aria-label={isFullscreen ? "Exit fullscreen" : "View participant fullscreen"}
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
        <div className={`room-tile-backdrop w-full h-full flex items-center justify-center ${getParticipantColor(participant.identity)}`}>
          <Avatar className="size-16 sm:size-24 border-4 border-white/30 bg-transparent">
            <AvatarFallback className="text-2xl sm:text-3xl font-semibold bg-white/15 text-inherit">
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
        className="absolute top-3 right-3 bg-black/30 backdrop-blur-sm text-white p-2 rounded-full cursor-pointer hover:bg-black/50 transition-all"
      >
        {isFullscreen ? <Minimize size={20} /> : <Maximize size={20} />}
      </button>

      {/* Overlay info */}
      <div className="absolute bottom-3 left-3 flex items-center gap-2">
        <div className="bg-black/60 backdrop-blur-sm text-white px-3 py-1.5 rounded-full text-sm flex items-center gap-2">
          <ParticipantName />
          {!participant.isMicrophoneEnabled && <MicOff className="size-3.5 text-red-300" />}
          {micPublication && participant.isMicrophoneEnabled && (
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

function RoomTools({ onOpenPanel, conferenceId }: { conferenceId: string; onOpenPanel: (panel: SidebarPanel) => void }) {
 const conference = useConference(conferenceId);
 const me = useCurrentParticipant(conferenceId);
 const space = useSpace(conference.data?.meetingSpaceId || "");
 const collaboration = useCollaborationState(conferenceId);
 const role = effectiveConferenceRole(space.data?.viewerRole, me.data?.role, me.data?.admissionState);
 const canRecord = meetingAllows(role, "artifact.list", space.data?.rolePolicy) || (space.data?.recordingPolicy !== "DISABLED" && (meetingAllows(role, "recording.start", space.data?.rolePolicy) || meetingAllows(role, "recording.stop", space.data?.rolePolicy)));
 const { resolvedTheme, setTheme } = useTheme();
 const { devices, activeDeviceId, setActiveMediaDevice } = useMediaDeviceSelect({ kind: "audiooutput" });
 return <div className="space-y-4"><div className="grid grid-cols-2 gap-2 sm:grid-cols-3">{([{ panel: "breakouts", label: "Breakout rooms", icon: GitFork }, { panel: "polls", label: "Polls", icon: ChartNoAxesColumn }, { panel: "qa", label: "Q&A", icon: CircleHelp }, { panel: "recordings", label: "Recordings", icon: Clapperboard }] as const).filter(item => item.panel === "recordings" ? canRecord : item.panel === "polls" ? collaboration.data?.policy.pollsEnabled !== false : item.panel === "qa" ? collaboration.data?.policy.qnaEnabled !== false : collaboration.data?.policy.maxBreakouts !== 0).map(({ panel, label, icon: Icon }) => <DialogClose asChild key={panel}><button key={panel} onClick={() => onOpenPanel(panel)} className="flex items-center gap-2 rounded-xl border border-border p-3 text-sm hover:bg-muted"><Icon className="size-4 text-primary" />{label}</button></DialogClose>)}<button className="flex items-center gap-2 rounded-xl border border-border p-3 text-sm hover:bg-muted" onClick={() => setTheme(resolvedTheme === "dark" ? "light" : "dark")}>{resolvedTheme === "dark" ? <Sun className="size-4" /> : <Moon className="size-4" />}Appearance</button></div><label className="flex items-center gap-2 text-sm"><Volume2 className="size-4" />Speaker<select aria-label="Audio output" className="min-w-0 flex-1 rounded-lg border border-border bg-card p-2" value={activeDeviceId} onChange={event => void setActiveMediaDevice(event.target.value)}>{(devices ?? []).map(device => <option key={device.deviceId} value={device.deviceId}>{device.label || "Default speaker"}</option>)}</select></label></div>;
}

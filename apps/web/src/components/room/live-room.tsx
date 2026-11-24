"use client";

import { RoomMessage } from "./pre-join-screen";
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
  useSpeakingParticipants,
  useIsSpeaking,
  useIsMuted,
  useLocalParticipant,
  type TrackReference,
} from "@livekit/components-react";
import { Track, RoomEvent } from "livekit-client";
import Image from "next/image";
import { Grid2X2, CircleHelp, ChartNoAxesColumn, GitFork, Clapperboard, Volume2, LayoutPanelTop, Lock, ShieldCheck, Users, MessageCircle, Settings2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useConference, usePreJoin } from "@/hooks/v2/useConferences";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useCollaborationState } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import { MEETING_AI_READY, MEETING_RECORDING_READY } from "@/lib/meeting-capture";
import type { SidebarPanel } from "./sidebar";
import { useMediaDeviceSelect } from "@livekit/components-react";
import Sidebar from "@/components/room/sidebar";
import FooterControls from "@/components/room/footer-controls";
import type { RoomJoinData } from "@/lib/prejoin";
import { useRouter } from "next/navigation";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Maximize, Minimize, UserPlus, UserMinus, MicOff, Mic } from "lucide-react";
import { storageStore } from "@/store";
import { conferencesService } from "@/services/v2/conferences.service";
import ConferenceControls from "./conference-controls";
import LocalRecordingControls from "./LocalRecordingControls";
import { useQueryClient } from "@tanstack/react-query";
import RealtimeRoomSync from "./RealtimeRoomSync";
import { participantKeys } from "@/hooks/v2/useParticipants";
import { Emoji, ReactionOverlay } from "./RoomEngagementControls";
import { CaptionsOverlay, RoomNotices } from "./RoomLiveEvents";
import { createContext, useContext } from "react";
import { useStartTranscription, useStopTranscription } from "@/hooks/v2/useArtifacts";
import { Captions, CaptionsOff, Pin } from "lucide-react";
import { quietExpectedLiveKitTeardown } from "@/lib/calling/livekit-quiet";

quietExpectedLiveKitTeardown();

// Notification type for participant events
interface ParticipantNotification {
  id: string;
  name: string;
  type: "joined" | "left";
}

// Tile background colors for participants without video
// Deep tile colours for people without video (inline so they never depend on compiled classes).
const TILE_COLORS = ["#1d3a6e", "#14503f", "#5a4216", "#3d2c6b", "#5c2236", "#164a5a"] as const;

// Distinct color per person in the call: order by identity so everyone sees the same colors,
// and only wrap around once there are more people than colors.
function getParticipantColor(identity: string, identities: string[]): string {
  const order = [...identities].sort();
  const index = order.indexOf(identity);
  if (index >= 0) return TILE_COLORS[index % TILE_COLORS.length];
  let hash = 0;
  for (let i = 0; i < identity.length; i++) hash = (hash * 31 + identity.charCodeAt(i)) | 0;
  return TILE_COLORS[Math.abs(hash) % TILE_COLORS.length];
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
  const me = useCurrentParticipant(joinData.conferenceId);
  const terminal = useRef(false);
  const closeSession = useCallback(async () => {
    if (sessionClosed.current) return;
    await conferencesService.leave(joinData.conferenceId, { participantSessionId: joinData.participantSessionId });
    sessionClosed.current = true;
    await queryClient.invalidateQueries({ queryKey: ["v2", "conferences"] });
  }, [joinData.conferenceId, joinData.participantSessionId, queryClient]);

  const leaveTo = joinData.isGuest ? "/" : "/dashboard";
  // A refresh / tab close also disconnects the media room; that must not navigate away.
  const unloading = useRef(false);
  useEffect(() => {
    const mark = () => { unloading.current = true; };
    window.addEventListener("pagehide", mark);
    window.addEventListener("beforeunload", mark);
    return () => {
      window.removeEventListener("pagehide", mark);
      window.removeEventListener("beforeunload", mark);
    };
  }, []);
  const [endedStatus, setEndedStatus] = useState<string | null>(null);

  const handleDisconnect = useCallback(async () => {
    if (isDisconnectingRef.current) return;
    isDisconnectingRef.current = true;
    try {
      await closeSession();
    } finally {
      storageStore.clearParticipantToken();
      storageStore.clearV2JoinData();
      if (!unloading.current && !terminal.current) router.replace(leaveTo);
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
    void queryClient.invalidateQueries({ queryKey: ["v2"] });
    // Say what happened instead of silently sending people home.
    terminal.current = true;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setEndedStatus(status);
  }, [status, queryClient]);

  // Removed, banned or denied by a host: say so instead of dropping the person on Home.
  const admission = me.data?.admissionState;
  useEffect(() => {
    if (!admission || !["REMOVED", "BANNED", "DENIED"].includes(admission)) return;
    terminal.current = true;
    sessionClosed.current = true;
    storageStore.clearParticipantToken();
    storageStore.clearV2JoinData();
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setEndedStatus(admission);
  }, [admission]);

  if (endedStatus) {
    return (
      <RoomMessage
        icon={endedStatus === "REMOVED" || endedStatus === "BANNED" || endedStatus === "DENIED" ? "missing" : "ended"}
        title={{ CANCELLED: "Meeting cancelled", FAILED: "The call stopped unexpectedly", REMOVED: "You were removed from the call", BANNED: "You can't rejoin this meeting", DENIED: "The host didn't let you in" }[endedStatus] ?? "This meeting has ended"}
      >
        {{ FAILED: "Something went wrong with the call. You can try joining again.", REMOVED: "A host removed you from this meeting.", BANNED: "A host removed you and blocked you from joining again.", DENIED: "Your request to join was declined." }[endedStatus] ?? "Thanks for joining. The host has ended the meeting for everyone."}
      </RoomMessage>
    );
  }

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
/** Collaboration state the tiles need: raised hands (order) and spotlights, matched by name for remote tiles. */
interface StageInfo { handOrder: Map<string, number>; spotlit: Set<string>; myId?: string; myHand?: number; mySpot?: boolean }
const StageContext = createContext<StageInfo>({ handOrder: new Map(), spotlit: new Set() });

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
  const [layout, setLayout] = useState<"grid" | "speaker">("grid");
  const conference = useConference(joinData.conferenceId);
  const me = useCurrentParticipant(joinData.conferenceId);
  const space = useSpace(conference.data?.meetingSpaceId || "");
  const preJoin = usePreJoin(joinData.conferenceId);
  const role = effectiveConferenceRole(space.data?.viewerRole || joinData.conferenceRole, me.data?.role, me.data?.admissionState);
  const canEnd = meetingAllows(role, "conference.end", space.data?.rolePolicy);
  const title = space.data?.title || preJoin.data?.title || "Meeting";
  // Live recording flag on the session (updated by recording.* realtime events); pre-join notice as fallback.
  const recordingState = (conference.data as { recordingState?: string | null } | undefined)?.recordingState;
  const recording = recordingState ? recordingState === "RECORDING" || recordingState === "STOPPING" : !!preJoin.data?.notices.recordingActive;
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  const startedAt = conference.data?.startedAt ? Date.parse(conference.data.startedAt) : null;
  const elapsed = startedAt ? Math.max(0, Math.floor((nowMs - startedAt) / 1000)) : null;
  const elapsedText = elapsed === null ? "" : `${Math.floor(elapsed / 3600) ? `${Math.floor(elapsed / 3600)}:` : ""}${String(Math.floor((elapsed % 3600) / 60)).padStart(2, "0")}:${String(elapsed % 60).padStart(2, "0")}`;
  const openPanel = (panel: SidebarPanel) => { setSidebarPanel(panel); setIsSidebarOpen(!(isSidebarOpen && sidebarPanel === panel)); };
  const [notifications, setNotifications] = useState<ParticipantNotification[]>([]);
  const [captionsOn, setCaptionsOn] = useState(false);
  const collab = useCollaborationState(joinData.conferenceId);
  const hands = [...(collab.data?.raisedHands ?? [])].sort((a, b) => a.raisedAt.localeCompare(b.raisedAt));
  const myHandIndex = hands.findIndex((h) => h.participantId === me.data?.id);
  const stage: StageInfo = {
    handOrder: new Map(hands.map((h, i) => [h.displayName, i + 1])),
    spotlit: new Set((collab.data?.spotlights ?? []).map((spot) => spot.displayName)),
    myId: me.data?.id,
    myHand: myHandIndex < 0 ? undefined : myHandIndex + 1,
    mySpot: (collab.data?.spotlights ?? []).some((spot) => spot.participantId === me.data?.id),
  };

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
      <div className="room-space dark flex h-dvh overflow-hidden bg-[#202124] text-foreground">
        <div className="flex min-w-0 flex-col flex-1 overflow-hidden">
          <header className="flex h-14 shrink-0 items-center justify-between gap-3 px-3 sm:h-16 sm:px-5">
            <div className="flex min-w-0 items-center gap-3">
              <Image src="/CFLogo.png" alt="CF Meet" width={36} height={36} className="size-8 shrink-0 object-contain" priority />
              <div className="min-w-0">
                <p className="truncate text-sm font-semibold text-white sm:text-base">{title}</p>
                <p className="flex items-center gap-2 text-xs text-white/60">
                  {elapsedText && <span className="tabular-nums">{elapsedText}</span>}
                  {conference.data?.isLocked ? <span className="flex items-center gap-1"><Lock className="size-3" /> Locked</span> : space.data?.waitingRoomPolicy && space.data.waitingRoomPolicy !== "OFF" ? <span className="hidden items-center gap-1 sm:flex"><ShieldCheck className="size-3" /> Waiting room on</span> : null}
                </p>
              </div>
              {recording && (
                <span className="flex shrink-0 items-center gap-1.5 rounded-full bg-destructive px-2.5 py-1 text-xs font-semibold text-white">
                  <span className="size-1.5 animate-pulse rounded-full bg-white" /> REC
                </span>
              )}
            </div>
            <div className="hidden w-40 sm:block">
              <NiceSelect<"grid" | "speaker">
                value={layout}
                onChange={setLayout}
                options={[
                  { value: "grid", label: "Grid", icon: Grid2X2 },
                  { value: "speaker", label: "Speaker", icon: LayoutPanelTop },
                ]}
              />
            </div>
          </header>
          {/* Main Video Area */}
          <main className="relative flex-1 overflow-hidden transition-all duration-300 ease-in-out">
            <StageContext.Provider value={stage}>
              <VideoGridContent layout={layout} />
            </StageContext.Provider>
            <ReactionOverlay />
            <RoomNotices onOpenPanel={(panel) => { setSidebarPanel(panel); setIsSidebarOpen(true); }} onExtend={canEnd ? (minutes) => conferencesService.extend(joinData.conferenceId, minutes) : undefined} />
            <CaptionsOverlay conferenceId={joinData.conferenceId} on={captionsOn} />

            {/* Participant Notifications */}
            <div className="absolute top-4 left-1/2 -translate-x-1/2 z-50 flex flex-col gap-2">
              {notifications.map((notification) => (
                <div
                  key={notification.id}
                  className="flex items-center gap-2 rounded-full bg-slate-800/95 px-4 py-2 text-white shadow-xl ring-1 ring-white/10 backdrop-blur animate-fade-in"
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

          <LocalRecordingControls label="meeting" canStart={!!space.data && space.data.recordingPolicy !== "DISABLED" && meetingAllows(role, "recording.start", space.data.rolePolicy)} />

          {/* Custom Footer Controls */}
          <FooterControls
            onOpenPanel={openPanel}
            sidebarPanel={sidebarPanel}
            moreControls={<ConferenceControls id={joinData.conferenceId} role={joinData.conferenceRole} sessionId={joinData.participantSessionId} initialMediaContext={joinData.mediaContext} onCredentialRefreshStateChange={onCredentialRefreshStateChange} tools={(close, openAudio) => <RoomTools conferenceId={joinData.conferenceId} onOpenPanel={openPanel} close={close} openAudio={openAudio} captionsOn={captionsOn} setCaptionsOn={setCaptionsOn} />} audioSettings={<AudioSettings />} />}
            isSidebarOpen={isSidebarOpen}
            conferenceId={joinData.conferenceId}
            permissions={joinData.permissions}
            onLeave={onLeave}
            canEnd={canEnd}
            info={<span className="truncate text-sm text-white/70"><span className="tabular-nums">{new Date(nowMs).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</span><span className="mx-2 text-white/30">|</span>{space.data?.meetingCode ?? ""}</span>}
          />
        </div>

        {/* Conditional Sidebar */}
        {isSidebarOpen && (
          <aside className="fixed inset-0 z-50 flex h-full flex-col bg-card sm:relative sm:inset-auto sm:z-0 sm:w-[360px] sm:bg-transparent sm:py-3 sm:pr-3" style={{ ["--background" as string]: "#2D2E31", ["--muted" as string]: "#3C4043", ["--border" as string]: "rgba(255,255,255,0.08)" }}>
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

function VideoGridContent({ layout }: { layout: "grid" | "speaker" }) {
  const stage = useContext(StageContext);
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
  const cols = baseLayout.cols;
  const rows = Math.ceil(cameraTracks.length / cols);
  const speaking = useSpeakingParticipants();
  const lastSpeaker = useRef<string | null>(null);
  if (speaking[0]) lastSpeaker.current = speaking[0].identity;

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

  // Speaker view: the current (or last) speaker large, everyone else in a strip.
  // A host spotlight overrides the layout for everyone: spotlighted person large.
  const spotTrack = cameraTracks.find((t) => (t.participant.isLocal ? stage.mySpot : stage.spotlit.has(t.participant.name ?? "")));
  if ((layout === "speaker" || spotTrack) && cameraTracks.length > 1) {
    const mainTrack = spotTrack ?? cameraTracks.find((t) => t.participant.identity === lastSpeaker.current)
      ?? cameraTracks.find((t) => !t.participant.isLocal)
      ?? cameraTracks[0];
    const others = cameraTracks.filter((t) => t !== mainTrack);
    return (
      <div ref={gridContainer} className="flex h-full w-full flex-col gap-2 p-2 md:gap-3 md:p-3">
        <div className="min-h-0 flex-1">
          <ParticipantTile trackRef={mainTrack} className="room-tile size-full">
            <CustomTileContent />
          </ParticipantTile>
        </div>
        <div className="no-scrollbar flex h-24 shrink-0 gap-2 overflow-x-auto md:h-32 md:gap-3">
          {others.map((track) => (
            <ParticipantTile key={track.participant.sid} trackRef={track} className="room-tile aspect-video h-full shrink-0">
              <CustomTileContent compact />
            </ParticipantTile>
          ))}
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

/** True while the given (local) mic track carries speech-level sound. */
function useLocalMicSpeaking(track: MediaStreamTrack | undefined) {
  const [speaking, setSpeaking] = useState(false);
  useEffect(() => {
    if (!track) return;
    const ctx = new AudioContext();
    // Browsers start audio analysis suspended; resume it (the user already clicked Join).
    void ctx.resume().catch(() => undefined);
    const source = ctx.createMediaStreamSource(new MediaStream([track]));
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 256;
    analyser.smoothingTimeConstant = 0.5;
    source.connect(analyser);
    const data = new Uint8Array(analyser.fftSize);
    let quietSince = 0;
    const id = setInterval(() => {
      // Peak deviation of the waveform, as in the pre-join meter (useMicLevel).
      analyser.getByteTimeDomainData(data);
      let peak = 0;
      for (let i = 0; i < data.length; i++) peak = Math.max(peak, Math.abs(data[i] - 128));
      const level = peak / 128;
      if (level > 0.06 && track.enabled && track.readyState === "live") { quietSince = 0; setSpeaking(true); }
      else if (!quietSince) quietSince = Date.now();
      else if (Date.now() - quietSince > 600) setSpeaking(false);
    }, 120);
    return () => { clearInterval(id); source.disconnect(); void ctx.close(); };
  }, [track]);
  return speaking;
}

function CustomTileContent({ compact = false }: { compact?: boolean }) {
  const roomIdentities = useParticipants().map((p) => p.identity);
  const participant = useParticipantContext();
  const stage = useContext(StageContext);
  const handNumber = participant.isLocal ? stage.myHand : stage.handOrder.get(participant.name ?? "");
  const spotlit = participant.isLocal ? stage.mySpot : stage.spotlit.has(participant.name ?? "");
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

  // Live speaking / mic state. Remote people use the server's active-speaker signal; for your
  // own tile the mic level is measured in the browser (instant, like Meet).
  const serverSpeaking = useIsSpeaking(participant ?? undefined);
  // useLocalParticipant re-renders when the mic track is (re)published, unlike participant.getTrackPublication.
  const { microphoneTrack } = useLocalParticipant();
  const localMic = participant?.isLocal ? microphoneTrack?.track?.mediaStreamTrack : undefined;
  const localLevelSpeaking = useLocalMicSpeaking(localMic);
  const speakingNow = serverSpeaking || localLevelSpeaking;
  const micMuted = useIsMuted({ participant: participant!, source: Track.Source.Microphone });

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
      className="group relative h-full w-full transition-shadow"
      style={{ borderRadius: "inherit" }}
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
        <div className="room-tile-backdrop flex h-full w-full items-center justify-center text-white" style={{ backgroundColor: getParticipantColor(participant.identity, roomIdentities) }}>
          <span className="relative flex items-center justify-center">
          <Avatar className={cn("relative border-4 border-white/30 bg-transparent", compact ? "size-10" : "size-16 sm:size-24")}>
            <AvatarFallback className={cn("bg-white/15 font-semibold text-inherit", compact ? "text-sm" : "text-2xl sm:text-3xl")}>
              {(participant.name || participant.identity || "Guest")
                .slice(0, 2)
                .toUpperCase()}
            </AvatarFallback>
          </Avatar>
          </span>
        </div>
      )}

      {/* Fullscreen (shown on hover / focus, like Meet) */}
      {!compact && <button
        onClick={toggleFullscreen}
        aria-label={isFullscreen ? "Exit full screen" : "Full screen"}
        className="absolute right-3 top-3 rounded-full bg-black/40 p-2 text-white opacity-0 backdrop-blur-sm transition-all hover:bg-black/60 focus-visible:opacity-100 group-hover:opacity-100"
      >
        {isFullscreen ? <Minimize size={20} /> : <Maximize size={20} />}
      </button>}

      {/* Speaking border: its own layer ON TOP of video/backdrop, same rounded corners as the tile. */}
      {speakingNow && !micMuted && (
        <span aria-hidden className="pointer-events-none absolute inset-0 z-10" style={{ borderRadius: "inherit", border: "3px solid #4d9bff" }} />
      )}
      {/* Raised hand (with queue order) and spotlight, top-left like Meet */}
      {(handNumber || spotlit) && (
        <div className={cn("absolute z-20 flex items-center gap-1.5", compact ? "left-1.5 top-1.5" : "left-3 top-3")}>
          {handNumber && (
            <span className={cn("flex items-center gap-1 rounded-full bg-[#a8c7fa] font-semibold text-[#062e6f] shadow-lg", compact ? "py-0.5 pl-0.5 pr-2 text-xs" : "py-1 pl-1 pr-2.5 text-sm")} aria-label={`Hand raised, number ${handNumber}`}>
              <Emoji name="raised_hand" className={compact ? "size-4" : "size-5"} />{handNumber}
            </span>
          )}
          {spotlit && !compact && <span className="flex items-center gap-1 rounded-full bg-black/55 px-2.5 py-1 text-xs font-medium text-white backdrop-blur-sm"><Pin className="size-3" />Spotlight</span>}
        </div>
      )}
      {/* Name, speaking bars and mic state */}
      <div className={cn("absolute flex items-center gap-2", compact ? "bottom-1.5 left-1.5" : "bottom-3 left-3")}>
        <div className={cn("flex max-w-[14rem] items-center gap-2 rounded-full bg-black/55 text-white backdrop-blur-sm", compact ? "px-2 py-1 text-xs" : "px-3 py-1.5 text-sm")}>
          {speakingNow && !micMuted ? (
            <span className="flex size-4 shrink-0 items-center justify-center gap-[2px] rounded-full bg-primary" aria-label="Speaking">
              {[0, 1, 2].map((b) => <span key={b} className="cf-eq w-[2px] rounded-full bg-white" style={{ animationDelay: `${b * 0.15}s` }} />)}
            </span>
          ) : null}
          <span className="truncate"><ParticipantName /></span>
          {micMuted && <MicOff className="size-3.5 shrink-0 text-red-300" aria-label="Muted" />}
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

function AudioSettings() {
 const mics = useMediaDeviceSelect({ kind: "audioinput" });
 const speakers = useMediaDeviceSelect({ kind: "audiooutput" });
 return <div className="grid gap-4">
   <NiceSelect label="Microphone" value={mics.activeDeviceId} onChange={(id) => void mics.setActiveMediaDevice(id)} options={(mics.devices ?? []).map(device => ({ value: device.deviceId, label: device.label || "Microphone", icon: Mic }))} />
   {/* Phones (and some browsers) don't let a web page pick speaker vs earpiece: the system does. */}
   {typeof HTMLMediaElement !== "undefined" && "setSinkId" in HTMLMediaElement.prototype && (speakers.devices ?? []).length > 1
     ? <NiceSelect label="Speaker" value={speakers.activeDeviceId} onChange={(id) => void speakers.setActiveMediaDevice(id)} options={(speakers.devices ?? []).map(device => ({ value: device.deviceId, label: device.label || "Default speaker", icon: Volume2 }))} />
     : <p className="flex items-start gap-2 rounded-xl bg-white/5 p-3 text-sm text-white/70"><Volume2 className="mt-0.5 size-4 shrink-0" />Your device chooses the speaker, earpiece or headphones. On a phone, switch with its audio or Bluetooth controls.</p>}
 </div>;
}

function RoomTools({ onOpenPanel, conferenceId, close, openAudio, captionsOn, setCaptionsOn }: { conferenceId: string; onOpenPanel: (panel: SidebarPanel) => void; close: () => void; openAudio: () => void; captionsOn: boolean; setCaptionsOn: (on: boolean) => void }) {
 const conference = useConference(conferenceId);
 const me = useCurrentParticipant(conferenceId);
 const space = useSpace(conference.data?.meetingSpaceId || "");
 const collaboration = useCollaborationState(conferenceId);
 const role = effectiveConferenceRole(space.data?.viewerRole, me.data?.role, me.data?.admissionState);
 const canRecord = MEETING_RECORDING_READY && (meetingAllows(role, "artifact.list", space.data?.rolePolicy) || (space.data?.recordingPolicy !== "DISABLED" && (meetingAllows(role, "recording.start", space.data?.rolePolicy) || meetingAllows(role, "recording.stop", space.data?.rolePolicy))));
 const row = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground transition-colors hover:bg-white/10";
 const open = (panel: SidebarPanel) => { close(); onOpenPanel(panel); };
 const transcriptionState = (conference.data as { transcriptionState?: string } | undefined)?.transcriptionState;
 const transcribing = !!transcriptionState && transcriptionState !== "OFF";
 const canStartTranscription = MEETING_AI_READY && meetingAllows(role, "transcription.start", space.data?.rolePolicy) && space.data?.transcriptionPolicy !== "DISABLED";
 const canStopTranscription = meetingAllows(role, "transcription.stop", space.data?.rolePolicy);
 const startTranscription = useStartTranscription(conferenceId);
 const stopTranscription = useStopTranscription(conferenceId);
 const [liveJob, setLiveJob] = useState<string>();
 const [captionError, setCaptionError] = useState("");
 const toggleCaptions = async () => {
   setCaptionError("");
   if (captionsOn) { setCaptionsOn(false); close(); return; }
   if (!transcribing) {
     if (!canStartTranscription) { setCaptionError("Captions need transcription. Ask a host to turn it on."); return; }
     try {
       const job = await startTranscription.mutateAsync({ mode: "LIVE", language: "en" });
       setLiveJob(job.id);
     } catch (error) {
       setCaptionError(error instanceof Error ? error.message : "Captions could not start.");
       return;
     }
   }
   setCaptionsOn(true);
   close();
 };
 const items = ([
   { panel: "people", label: "People", icon: Users, phoneOnly: true },
   { panel: "chat", label: "Chat", icon: MessageCircle, phoneOnly: true },
   { panel: "polls", label: "Polls", icon: ChartNoAxesColumn, show: collaboration.data?.policy.pollsEnabled !== false },
   { panel: "qa", label: "Q&A", icon: CircleHelp, show: collaboration.data?.policy.qnaEnabled !== false },
   { panel: "breakouts", label: "Breakout rooms", icon: GitFork, show: collaboration.data?.policy.maxBreakouts !== 0 },
   { panel: "recordings", label: "Recordings", icon: Clapperboard, show: canRecord },
 ] as const).filter(item => !("show" in item) || item.show);
 return <>
   {items.map(({ panel, label, icon: Icon, ...rest }) => (
     <button key={panel} type="button" onClick={() => open(panel)} className={cn(row, "phoneOnly" in rest && rest.phoneOnly && "sm:hidden")}>
       <Icon className="size-4 text-white/70" /> {label}
     </button>
   ))}
   {MEETING_AI_READY ? <button type="button" onClick={() => void toggleCaptions()} disabled={startTranscription.isPending} className={row}>
     {captionsOn ? <CaptionsOff className="size-4 text-white/70" /> : <Captions className="size-4 text-white/70" />}
     <span className="flex-1">{startTranscription.isPending ? "Starting captions…" : captionsOn ? "Turn off captions" : "Turn on captions"}</span>
   </button> : null}
   {transcribing && canStopTranscription && liveJob && (
     <button type="button" onClick={() => { stopTranscription.mutate(liveJob); setCaptionsOn(false); close(); }} disabled={stopTranscription.isPending} className={row}><CaptionsOff className="size-4 text-white/70" /> Stop transcription for everyone</button>
   )}
   {captionError && <p className="px-3 pb-1 text-xs text-red-300">{captionError}</p>}
   <button type="button" onClick={openAudio} className={row}><Settings2 className="size-4 text-white/70" /> Audio settings</button>
 </>;
}

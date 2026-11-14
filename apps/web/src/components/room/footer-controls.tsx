"use client";

import { Mic, MicOff, Video, VideoOff, MonitorUp, MonitorX, PhoneOff, Users, MessageCircle, Loader2 } from "lucide-react";
import { useLocalParticipant, useMediaDeviceSelect, useParticipants, useRoomContext } from "@livekit/components-react";
import { useState, type ReactNode } from "react";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { useCollaborationState } from "@/hooks/v2/useCollaboration";
import { useEndConference } from "@/hooks/v2/useConferences";
import { schedulingError } from "@/lib/meeting-scheduling";
import type { SidebarPanel } from "./sidebar";
import RoomEngagementControls from "./RoomEngagementControls";
import MutedSpeechPrompt from "./MutedSpeechPrompt";

interface Props {
  onOpenPanel: (panel: SidebarPanel) => void;
  sidebarPanel: SidebarPanel;
  isSidebarOpen: boolean;
  conferenceId: string;
  permissions?: import("@/types/v2").JoinPermissions;
  onLeave?: () => Promise<void>;
  moreControls?: ReactNode;
  /** The viewer may end the meeting for everyone (host / co-host). */
  canEnd?: boolean;
  /** Left side of the bar (clock, meeting code) on wide screens. */
  info?: ReactNode;
}

/**
 * Call toolbar (Meet / Teams pattern): device controls in the middle, red when off;
 * people and chat on the right; a red Leave pill. Hosts leaving get "End for everyone",
 * and a host who is alone is asked whether to end the meeting so it never stays open empty.
 */
export default function FooterControls({ onOpenPanel, sidebarPanel, isSidebarOpen, conferenceId, permissions, onLeave, moreControls, canEnd = false, info }: Props) {
  const { localParticipant, isMicrophoneEnabled, isCameraEnabled, isScreenShareEnabled } = useLocalParticipant();
  const participants = useParticipants();
  const room = useRoomContext();
  const collaboration = useCollaborationState(conferenceId);
  const endConference = useEndConference();
  const allowed = collaboration.data?.me?.permissions || permissions;
  const [leaveOpen, setLeaveOpen] = useState(false);
  const [busy, setBusy] = useState<"leave" | "end" | null>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState<string>();
  const alone = participants.length <= 1;
  const mic = useMediaDeviceSelect({ kind: "audioinput" });

  const media = async (kind: "audio" | "video" | "screen") => {
    setError("");
    setPending(kind);
    try {
      if (kind === "audio") await localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled, { echoCancellation: true, noiseSuppression: true, autoGainControl: true });
      if (kind === "video") await localParticipant.setCameraEnabled(!isCameraEnabled);
      if (kind === "screen") await localParticipant.setScreenShareEnabled(!isScreenShareEnabled);
    } catch (cause) {
      setError(schedulingError(cause));
    } finally {
      setPending(undefined);
    }
  };

  const leave = async () => {
    setBusy("leave");
    setError("");
    try {
      await onLeave?.();
      await room.disconnect();
    } catch (cause) {
      setError(schedulingError(cause));
      setBusy(null);
    }
  };

  const endForEveryone = async () => {
    setBusy("end");
    setError("");
    try {
      await endConference.mutateAsync(conferenceId);
      await onLeave?.();
      await room.disconnect();
    } catch (cause) {
      setError(schedulingError(cause));
      setBusy(null);
    }
  };

  const panelButton = (panel: SidebarPanel, label: string, Icon: typeof Users, count?: number) => (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={isSidebarOpen && sidebarPanel === panel}
      onClick={() => onOpenPanel(panel)}
      className="room-control-button relative"
    >
      <Icon />
      {count ? <span className="absolute -right-1 -top-1 flex h-5 min-w-5 items-center justify-center rounded-full bg-primary px-1 text-[11px] font-semibold text-primary-foreground">{count}</span> : null}
    </button>
  );

  return (
    <footer className="room-footer">
      {error && !leaveOpen && <p role="alert" className="absolute bottom-full left-1/2 mb-3 w-[min(92vw,28rem)] -translate-x-1/2 rounded-xl bg-destructive px-4 py-3 text-sm text-white shadow-xl">{error}</p>}
      <MutedSpeechPrompt muted={!isMicrophoneEnabled && allowed?.audioPublish !== false} deviceId={mic.activeDeviceId} onUnmute={() => void media("audio")} />
      <div className="room-bar">
        <div className="room-bar-side hidden lg:flex">{info}</div>

        <div className="room-controls">
          <button type="button" className="room-control-button" data-off={!isMicrophoneEnabled || undefined} aria-label={isMicrophoneEnabled ? "Turn off microphone" : "Turn on microphone"} title={isMicrophoneEnabled ? "Turn off microphone" : "Turn on microphone"} disabled={!!pending || (allowed?.audioPublish === false && !isMicrophoneEnabled)} onClick={() => void media("audio")}>
            {isMicrophoneEnabled ? <Mic /> : <MicOff />}
          </button>
          <button type="button" className="room-control-button" data-off={!isCameraEnabled || undefined} aria-label={isCameraEnabled ? "Turn off camera" : "Turn on camera"} title={isCameraEnabled ? "Turn off camera" : "Turn on camera"} disabled={!!pending || (allowed?.videoPublish === false && !isCameraEnabled)} onClick={() => void media("video")}>
            {isCameraEnabled ? <Video /> : <VideoOff />}
          </button>
          <button type="button" className="room-control-button max-sm:hidden" data-active={isScreenShareEnabled || undefined} aria-label={isScreenShareEnabled ? "Stop presenting" : "Present your screen"} title={isScreenShareEnabled ? "Stop presenting" : "Present your screen"} aria-pressed={isScreenShareEnabled} disabled={!!pending || (allowed?.screenSharePublish === false && !isScreenShareEnabled)} onClick={() => void media("screen")}>
            {isScreenShareEnabled ? <MonitorX /> : <MonitorUp />}
          </button>
          <RoomEngagementControls conferenceId={conferenceId} />
          {moreControls}
          <button type="button" aria-label="Leave call" title="Leave call" className="room-leave-button" onClick={() => { setError(""); setLeaveOpen(true); }}>
            <PhoneOff />
          </button>
        </div>

        <div className="room-bar-side flex justify-end max-sm:hidden">
          {panelButton("people", "People", Users, participants.length)}
          {panelButton("chat", "Chat", MessageCircle)}
        </div>
      </div>

      <Dialog open={leaveOpen} onOpenChange={(v) => !busy && setLeaveOpen(v)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{canEnd && alone ? "You're the only one here" : "Leave the call?"}</DialogTitle>
            <DialogDescription>
              {canEnd && alone
                ? "End the meeting so it doesn't stay open, or leave it open so others can still join."
                : canEnd
                  ? "Everyone else can keep talking after you leave. As host, you can also end it for everyone."
                  : "Everyone else can keep talking after you leave."}
            </DialogDescription>
          </DialogHeader>
          {error && <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p>}
          <div className="grid gap-2">
            {canEnd && (
              <button type="button" disabled={!!busy} onClick={() => void endForEveryone()} className="inline-flex h-11 items-center justify-center gap-2 rounded-xl bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-70">
                {busy === "end" && <Loader2 className="size-4 animate-spin" />}
                {alone ? "End meeting" : "End for everyone"}
              </button>
            )}
            <button type="button" disabled={!!busy} onClick={() => void leave()} className={canEnd ? "inline-flex h-11 items-center justify-center gap-2 rounded-xl border border-border px-4 text-sm font-semibold text-foreground hover:bg-muted disabled:opacity-70" : "inline-flex h-11 items-center justify-center gap-2 rounded-xl bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-70"}>
              {busy === "leave" && <Loader2 className="size-4 animate-spin" />}
              {canEnd && alone ? "Leave it open" : "Leave call"}
            </button>
            <button type="button" disabled={!!busy} onClick={() => setLeaveOpen(false)} className="inline-flex h-11 items-center justify-center rounded-xl px-4 text-sm font-semibold text-muted-foreground hover:bg-muted">
              Stay in call
            </button>
          </div>
        </DialogContent>
      </Dialog>
    </footer>
  );
}

"use client";

import { Mic, MicOff, Video, VideoOff, MonitorUp, PhoneOff, Users, MessageCircle } from "lucide-react";
import { useLocalParticipant, useRoomContext } from "@livekit/components-react";
import { useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter, DialogTrigger } from "@/components/ui/dialog";
import { useMediaControl } from "@/hooks";
import { useCollaborationState } from "@/hooks/v2/useCollaboration";
import { schedulingError } from "@/lib/meeting-scheduling";
import type { SidebarPanel } from "./sidebar";
import RoomEngagementControls from "./RoomEngagementControls";
interface Props {
  onOpenPanel: (panel: SidebarPanel) => void;
  sidebarPanel: SidebarPanel;
  isSidebarOpen: boolean;
  meetingId: string;
  participantId: string;
  conferenceId?: string;
  permissions?: import("@/types/v2").JoinPermissions;
  onLeave?: () => Promise<void>;
  moreControls?: ReactNode;
}
export default function FooterControls({
  onOpenPanel,
  sidebarPanel,
  isSidebarOpen,
  meetingId,
  participantId,
  conferenceId,
  permissions,
  onLeave,
  moreControls
}: Props) {
  const {
    localParticipant,
    isMicrophoneEnabled,
    isCameraEnabled,
    isScreenShareEnabled
  } = useLocalParticipant();
  const room = useRoomContext();
  const collaboration = useCollaborationState(conferenceId || "");
  const allowed = collaboration.data?.me?.permissions || permissions;
  const {
    startScreenShare,
    stopScreenShare,
    updateAudio,
    updateVideo
  } = useMediaControl();
  const [leaveOpen, setLeaveOpen] = useState(false),
    [leaving, setLeaving] = useState(false);
  const [error, setError] = useState("");
  const [pending, setPending] = useState<string>();
  const media = async (kind: "audio" | "video" | "screen") => {
    setError("");
    setPending(kind);
    try {
      if (kind === "audio") {
        await localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled, {
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true
        });
        if (!conferenceId) await updateAudio.mutateAsync({
          participantId,
          isMuted: isMicrophoneEnabled
        });
      }
      if (kind === "video") {
        await localParticipant.setCameraEnabled(!isCameraEnabled);
        if (!conferenceId) await updateVideo.mutateAsync({
          participantId,
          isVideoOn: !isCameraEnabled
        });
      }
      if (kind === "screen") {
        await localParticipant.setScreenShareEnabled(!isScreenShareEnabled);
        if (!conferenceId) await (isScreenShareEnabled ? stopScreenShare : startScreenShare).mutateAsync(meetingId);
      }
    } catch (cause) {
      setError(schedulingError(cause));
    } finally {
      setPending(undefined);
    }
  };
  const leave = async () => {
    setLeaving(true);
    setError("");
    try {
      await onLeave?.();
      await room.disconnect();
    } catch (cause) {
      setError(schedulingError(cause));
      setLeaving(false);
    }
  };
  return <footer className="room-footer">
 {error && !leaveOpen && <p role="alert" className="absolute bottom-full left-2 right-2 rounded-xl border border-border bg-card p-3 text-sm text-destructive">{error}</p>}
 <div className="room-controls">
 <div className="room-control"><button className="room-control-button" aria-label={isMicrophoneEnabled ? "Mute microphone" : "Unmute microphone"} aria-pressed={isMicrophoneEnabled} disabled={!!pending || allowed?.audioPublish === false && !isMicrophoneEnabled} onClick={() => void media("audio")}>{isMicrophoneEnabled ? <Mic /> : <MicOff className="text-destructive" />}</button><span>{isMicrophoneEnabled ? "Mute" : "Unmute"}</span></div>
 <div className="room-control"><button className="room-control-button" aria-label={isCameraEnabled ? "Stop video" : "Start video"} aria-pressed={isCameraEnabled} disabled={!!pending || allowed?.videoPublish === false && !isCameraEnabled} onClick={() => void media("video")}>{isCameraEnabled ? <Video /> : <VideoOff className="text-destructive" />}</button><span>{isCameraEnabled ? "Stop video" : "Start video"}</span></div>
 <div className="room-control"><button className="room-control-button" aria-label="Share screen" aria-pressed={isScreenShareEnabled} disabled={!!pending || allowed?.screenSharePublish === false && !isScreenShareEnabled} onClick={() => void media("screen")}><MonitorUp /></button><span>Share</span></div>
 <Dialog open={leaveOpen} onOpenChange={value => {
        if (!leaving) setLeaveOpen(value);
      }}><div className="room-control"><DialogTrigger asChild><button aria-label="Leave conference" className="room-leave-button"><PhoneOff /></button></DialogTrigger><span>Leave</span></div><DialogContent><DialogHeader><DialogTitle>Leave this call?</DialogTitle><DialogDescription>Other participants can continue their conversation after you leave.</DialogDescription></DialogHeader>{error && <p role="alert" className="text-sm text-destructive">{error}</p>}<DialogFooter><Button variant="outline" disabled={leaving} onClick={() => setLeaveOpen(false)}>Stay in call</Button><Button variant="destructive" disabled={leaving} onClick={() => void leave()}>{leaving ? "Leaving?" : "Leave call"}</Button></DialogFooter></DialogContent></Dialog>
 <div className="room-control"><button className="room-control-button" aria-label="Open participants" aria-pressed={isSidebarOpen && sidebarPanel === "people"} onClick={() => onOpenPanel("people")}><Users /></button><span>People</span></div>
 <div className="room-control"><button className="room-control-button" aria-label="Open chat" aria-pressed={isSidebarOpen && sidebarPanel === "chat"} onClick={() => onOpenPanel("chat")}><MessageCircle /></button><span>Chat</span></div>
 {conferenceId && <RoomEngagementControls conferenceId={conferenceId} />}{moreControls}
 </div></footer>;
}

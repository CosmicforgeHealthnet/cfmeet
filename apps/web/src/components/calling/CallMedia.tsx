"use client";

import { useEffect, useState } from "react";
import { LiveKitRoom, RoomAudioRenderer, useAudioPlayback, useLocalParticipant, useRoomContext, useTracks, VideoTrack } from "@livekit/components-react";
import { DisconnectReason, RoomEvent, Track, type LocalVideoTrack } from "livekit-client";
import { Mic, MicOff, SwitchCamera, Video, VideoOff, Volume2 } from "lucide-react";
import { cn } from "@/lib/utils";
import type { MediaCredential } from "@/types/calling";
import { quietExpectedLiveKitTeardown } from "@/lib/calling/livekit-quiet";

quietExpectedLiveKitTeardown();

/**
 * Video for the full-screen call (Meet / FaceTime pattern): the other person fills the stage,
 * you sit in a small rounded window in the corner. Renders nothing until someone has video.
 */
export function CallVideoStage() {
  const tracks = useTracks([Track.Source.Camera]).filter((t) => t.publication?.track && !t.publication.isMuted);
  const remote = tracks.filter((t) => !t.participant.isLocal);
  const local = tracks.find((t) => t.participant.isLocal);
  if (!tracks.length) return null;
  return (
    <div className="absolute inset-0">
      {remote.length > 0 ? (
        <div className={cn("grid h-full w-full gap-1 bg-black", remote.length > 1 && "grid-cols-2")}>
          {remote.map((t) => <VideoTrack key={`${t.participant.identity}-${t.publication.trackSid}`} trackRef={t} className="h-full w-full object-cover" />)}
        </div>
      ) : null}
      {local ? (
        <div className={cn("overflow-hidden rounded-2xl bg-black shadow-2xl ring-1 ring-white/15", remote.length ? "absolute bottom-28 right-4 aspect-[3/4] w-28 sm:bottom-28 sm:right-6 sm:aspect-video sm:w-56" : "absolute inset-0 rounded-none ring-0")}>
          <VideoTrack trackRef={local} className="h-full w-full -scale-x-100 object-cover" />
        </div>
      ) : null}
    </div>
  );
}

/** True once anyone in the call is sending video (so the stage can switch from avatar to video). */
export function useAnyVideo() {
  return useTracks([Track.Source.Camera]).some((t) => t.publication?.track && !t.publication.isMuted);
}

/**
 * Turn the mic (and camera for video) on only once the server allows it. Calling first admits a
 * session with a join-only grant and upgrades it to publish after admission; publishing at
 * connect time fails with "insufficient permissions" and LiveKit never retries.
 */
function PublishWhenAllowed({ audio, video }: { audio: boolean; video: boolean }) {
  const room = useRoomContext();
  useEffect(() => {
    let done = false;
    const tryPublish = () => {
      const lp = room.localParticipant;
      const allowed = lp.permissions?.canPublish;
      if (done || !allowed || room.state !== "connected") return;
      done = true;
      if (audio) void lp.setMicrophoneEnabled(true).catch(() => { done = false; });
      if (video) void lp.setCameraEnabled(true).catch(() => undefined);
    };
    tryPublish();
    room.on(RoomEvent.Connected, tryPublish);
    room.on(RoomEvent.ParticipantPermissionsChanged, tryPublish);
    return () => { room.off(RoomEvent.Connected, tryPublish); room.off(RoomEvent.ParticipantPermissionsChanged, tryPublish); };
  }, [room, audio, video]);
  return null;
}

/**
 * Browsers block sound that starts long after the last click (the media server joins seconds
 * after Accept). Retry on the next tap anywhere, and show a clear button until sound is on.
 */
function SoundUnlock() {
  const { canPlayAudio, startAudio } = useAudioPlayback();
  useEffect(() => {
    if (canPlayAudio) return;
    const unlock = () => void startAudio().catch(() => undefined);
    window.addEventListener("pointerdown", unlock, { once: true });
    window.addEventListener("keydown", unlock, { once: true });
    return () => { window.removeEventListener("pointerdown", unlock); window.removeEventListener("keydown", unlock); };
  }, [canPlayAudio, startAudio]);
  if (canPlayAudio) return null;
  return (
    <button type="button" onClick={() => void startAudio()}
      className="absolute left-1/2 top-20 z-50 inline-flex h-10 -translate-x-1/2 items-center gap-2 rounded-full bg-white px-4 text-sm font-semibold text-[#202124] shadow-2xl hover:bg-white/90">
      <Volume2 className="size-4" />Tap to turn on sound
    </button>
  );
}

/** LiveKit's own retry (before it gives up) surfaced to the reconnect state machine (CAL-015). */
function ProviderReconnectEvents({ onReconnecting, onReconnected }: { onReconnecting: () => void; onReconnected: () => void }) {
  const room = useRoomContext();
  useEffect(() => {
    room.on(RoomEvent.Reconnecting, onReconnecting);
    room.on(RoomEvent.Reconnected, onReconnected);
    return () => { room.off(RoomEvent.Reconnecting, onReconnecting); room.off(RoomEvent.Reconnected, onReconnected); };
  }, [room, onReconnecting, onReconnected]);
  return null;
}

/**
 * One labelled round call button (WhatsApp call-screen pattern): icon in a circle, label below.
 * `off` paints it white with a dark icon, the way WhatsApp shows "mic off" / "camera off".
 */
export function CallButton({ icon, label, onClick, off, disabled, tone }: { icon: React.ReactNode; label: string; onClick?: () => void; off?: boolean; disabled?: boolean; tone?: "danger" | "warn" }) {
  return (
    <button type="button" aria-label={label} title={label} aria-pressed={off} disabled={disabled} onClick={onClick}
      className="group flex w-20 flex-col items-center gap-2 disabled:opacity-40">
      <span className={cn("flex size-16 items-center justify-center rounded-full transition-colors [&_svg]:size-6",
        tone === "danger" ? "bg-[#EA4335] text-white group-hover:bg-[#D93025]"
          : tone === "warn" ? "bg-[#FDD663] text-[#3A2A00] group-hover:bg-[#FDE293]"
            : off ? "bg-white text-[#202124]" : "bg-white/12 text-white group-hover:bg-white/20")}>{icon}</span>
      <span className="text-sm text-white/85">{label}</span>
    </button>
  );
}

/** Mute / unmute. Must be rendered inside <CallMedia>. */
export function CallMicButton({ allowed }: { allowed: boolean }) {
  const { localParticipant, isMicrophoneEnabled } = useLocalParticipant();
  if (!allowed) return null;
  return <CallButton icon={isMicrophoneEnabled ? <Mic /> : <MicOff />} label={isMicrophoneEnabled ? "Mute" : "Unmute"} off={!isMicrophoneEnabled}
    onClick={() => void localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled)} />;
}

/** Small mute toggle for the minimised strip. */
export function CallMicToggle() {
  const { localParticipant, isMicrophoneEnabled } = useLocalParticipant();
  return (
    <button type="button" aria-label={isMicrophoneEnabled ? "Mute" : "Unmute"} title={isMicrophoneEnabled ? "Mute" : "Unmute"} aria-pressed={!isMicrophoneEnabled}
      onClick={() => void localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled)}
      className={cn("flex size-10 items-center justify-center rounded-full [&_svg]:size-5", isMicrophoneEnabled ? "bg-white/12 text-white hover:bg-white/20" : "bg-white text-[#202124]")}>
      {isMicrophoneEnabled ? <Mic /> : <MicOff />}
    </button>
  );
}

/** Camera on / off. Must be rendered inside <CallMedia>. */
export function CallCameraButton({ allowed }: { allowed: boolean }) {
  const { localParticipant, isCameraEnabled } = useLocalParticipant();
  if (!allowed || !localParticipant.permissions?.canPublish) return null;
  return <CallButton icon={isCameraEnabled ? <Video /> : <VideoOff />} label={isCameraEnabled ? "Camera" : "Camera off"} off={!isCameraEnabled}
    onClick={() => void localParticipant.setCameraEnabled(!isCameraEnabled)} />;
}

/** Switch between front and back camera (phones). Hidden while the camera is off. */
export function CallFlipButton() {
  const { localParticipant, isCameraEnabled } = useLocalParticipant();
  const [front, setFront] = useState(true);
  const [many, setMany] = useState(false);
  useEffect(() => {
    void navigator.mediaDevices?.enumerateDevices().then((d) => setMany(d.filter((x) => x.kind === "videoinput").length > 1)).catch(() => undefined);
  }, [isCameraEnabled]);
  if (!isCameraEnabled || !many) return null;
  const flip = async () => {
    const track = [...localParticipant.videoTrackPublications.values()].find((t) => t.source === Track.Source.Camera)?.track as LocalVideoTrack | undefined;
    if (!track) return;
    await track.restartTrack({ facingMode: front ? "environment" : "user" });
    setFront(!front);
  };
  return <CallButton icon={<SwitchCamera />} label="Flip" onClick={() => void flip().catch(() => undefined)} />;
}

/** Choose where call audio plays (speaker, headset, …). Only where the browser lets a page pick the output. */
export function CallSpeakerButton() {
  const room = useRoomContext();
  const [outputs, setOutputs] = useState<MediaDeviceInfo[]>([]);
  const [index, setIndex] = useState(0);
  useEffect(() => {
    if (typeof HTMLMediaElement === "undefined" || !("setSinkId" in HTMLMediaElement.prototype)) return;
    void navigator.mediaDevices?.enumerateDevices().then((d) => setOutputs(d.filter((x) => x.kind === "audiooutput" && x.deviceId !== "communications"))).catch(() => undefined);
  }, []);
  if (outputs.length < 2) return null;
  const next = (index + 1) % outputs.length;
  const label = outputs[index]?.label?.split("(")[0]?.trim() || "Speaker";
  return <CallButton icon={<Volume2 />} label={label.length > 10 ? "Speaker" : label}
    onClick={() => void room.switchActiveDevice("audiooutput", outputs[next].deviceId).then(() => setIndex(next)).catch(() => undefined)} />;
}

/** "Muted" tag for the stage. Must be rendered inside <CallMedia>. */
export function MutedTag() {
  const { isMicrophoneEnabled } = useLocalParticipant();
  if (isMicrophoneEnabled) return null;
  return <span className="inline-flex items-center gap-1.5 rounded-full bg-white/10 px-3 py-1 text-xs font-medium text-white/80"><MicOff className="size-3.5" />You&apos;re muted</span>;
}

/**
 * The media connection of one call session. It reports every connection transition; the
 * reconnect policy (fresh credential + bounded backoff) lives in the calling provider.
 * `children` render inside the room (so they can use the buttons and stage above).
 */
export default function CallMedia({ credential, video, onConnected, onDisconnected, onProviderReconnecting, onProviderReconnected, onError, className, children }: {
  credential: MediaCredential;
  video: boolean;
  onConnected: () => void;
  /** `userInitiated` is true for this client's own disconnect (leave), false when the connection was lost. */
  onDisconnected: (userInitiated: boolean) => void;
  onProviderReconnecting: () => void;
  onProviderReconnected: () => void;
  onError: (error: Error) => void;
  className?: string;
  children?: React.ReactNode;
}) {
  return (
    <LiveKitRoom serverUrl={credential.url} token={credential.token} connect
      audio={false} video={false}
      onConnected={onConnected}
      onDisconnected={(reason) => onDisconnected(reason === DisconnectReason.CLIENT_INITIATED)}
      onError={onError} className={className}>
      <RoomAudioRenderer />
      <SoundUnlock />
      <ProviderReconnectEvents onReconnecting={onProviderReconnecting} onReconnected={onProviderReconnected} />
      <PublishWhenAllowed audio={credential.capabilities.canPublishAudio} video={video && credential.capabilities.canPublishVideo} />
      {children}
    </LiveKitRoom>
  );
}

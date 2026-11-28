"use client";

import { useEffect, useRef, useState } from "react";
import { LiveKitRoom, RoomAudioRenderer, useAudioPlayback, useLocalParticipant, useRoomContext, useTracks, VideoTrack } from "@livekit/components-react";
import { DisconnectReason, RoomEvent, Track, type LocalVideoTrack, type Room } from "livekit-client";
import { Mic, MicOff, MonitorUp, MonitorX, SwitchCamera, Video, VideoOff, Volume2 } from "lucide-react";
import { cn } from "@/lib/utils";
import type { MediaCredential } from "@/types/calling";
import { quietExpectedLiveKitTeardown } from "@/lib/calling/livekit-quiet";
import { DeviceRecordingProvider } from "@/components/recording/DeviceRecording";

quietExpectedLiveKitTeardown();

/**
 * Video for the full-screen call (Meet / FaceTime pattern): the other person fills the stage,
 * you sit in a small rounded window in the corner. Renders nothing until someone has video.
 */
export function CallVideoStage() {
  const tracks = useTracks([Track.Source.Camera]).filter((t) => t.publication?.track && !t.publication.isMuted);
  // C6: a remote participant's shared screen takes the stage (fitted, never cropped).
  const screen = useTracks([Track.Source.ScreenShare]).find((t) => t.publication?.track && !t.publication.isMuted && !t.participant.isLocal);
  const remote = tracks.filter((t) => !t.participant.isLocal);
  const local = tracks.find((t) => t.participant.isLocal);
  if (!tracks.length && !screen) return null;
  return (
    <div className="absolute inset-0">
      {screen ? (
        <div className="h-full w-full bg-black">
          <VideoTrack trackRef={screen} className="h-full w-full object-contain" />
        </div>
      ) : remote.length > 0 ? (
        <div className={cn("grid h-full w-full gap-1 bg-black", remote.length > 1 && "grid-cols-2")}>
          {remote.map((t) => <VideoTrack key={`${t.participant.identity}-${t.publication.trackSid}`} trackRef={t} className="h-full w-full object-cover" />)}
        </div>
      ) : null}
      {local ? (
        <div className={cn("overflow-hidden rounded-2xl bg-black shadow-2xl ring-1 ring-white/15", remote.length ? "absolute bottom-28 right-4 aspect-[3/4] w-28 sm:bottom-28 sm:right-6 sm:aspect-video sm:w-56" : "absolute inset-0 rounded-none ring-0")}>
          <VideoTrack trackRef={local} className={cn("h-full w-full object-cover", (local.publication?.track?.mediaStreamTrack.getSettings().facingMode ?? "user") === "user" && "-scale-x-100")} />
        </div>
      ) : null}
    </div>
  );
}

/** True once anyone in the call is sending video (so the stage can switch from avatar to video). */
export function useAnyVideo() {
  return useTracks([Track.Source.Camera, Track.Source.ScreenShare])
    .some((t) => t.publication?.track && !t.publication.isMuted && (t.source === Track.Source.Camera || !t.participant.isLocal));
}

/**
 * Turn the mic (and camera for video) on only once the server allows it. Calling first admits a
 * session with a join-only grant and upgrades it to publish after admission; publishing at
 * connect time fails with "insufficient permissions" and LiveKit never retries.
 */
/**
 * The camera opened for the video-call preview, handed to the call when it connects. Opening the
 * camera a second time fails on many laptops (Windows webcams allow one user), so the preview's
 * track is the one that gets sent.
 */
let preview: { stream: Promise<MediaStream | null>; taken: boolean } | null = null;

export function openPreviewCamera(): Promise<MediaStream | null> {
  if (!preview) {
    const stream = navigator.mediaDevices?.getUserMedia({ video: { facingMode: "user" } }).catch(() => null) ?? Promise.resolve(null);
    preview = { stream, taken: false };
  }
  return preview.stream;
}

/** The preview is no longer shown: stop the camera unless the call took it. */
export function closePreviewCamera() {
  const p = preview;
  preview = null;
  if (p && !p.taken) void p.stream.then((s) => s?.getTracks().forEach((t) => t.stop()));
}

async function takePreviewTrack(): Promise<MediaStreamTrack | null> {
  if (!preview) return null;
  const s = await preview.stream;
  const track = s?.getVideoTracks().find((t) => t.readyState === "live") ?? null;
  if (track) preview.taken = true;
  return track;
}

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
      if (video) {
        void (async () => {
          const track = await takePreviewTrack();
          if (track) await lp.publishTrack(track, { source: Track.Source.Camera, name: "camera" });
          else await lp.setCameraEnabled(true);
        })().catch(() => void lp.setCameraEnabled(true).catch(() => undefined));
      }
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
export function CallButton({ icon, label, onClick, off, active, disabled, tone }: { icon: React.ReactNode; label: string; onClick?: () => void; off?: boolean; active?: boolean; disabled?: boolean; tone?: "danger" | "warn" }) {
  return (
    <button type="button" aria-label={label} title={label} aria-pressed={off} disabled={disabled} onClick={onClick}
      className="group flex w-20 flex-col items-center gap-2 disabled:opacity-40">
      <span className={cn("flex size-16 items-center justify-center rounded-full transition-colors [&_svg]:size-6",
        tone === "danger" ? "bg-[#EA4335] text-white group-hover:bg-[#D93025]"
          : tone === "warn" ? "bg-[#FDD663] text-[#3A2A00] group-hover:bg-[#FDE293]"
            : off ? "bg-[#EA4335] text-white group-hover:bg-[#D93025]" : active ? "bg-[#8AB4FF] text-[#0A2A5E] group-hover:bg-[#A8C7FA]" : "bg-white/12 text-white group-hover:bg-white/20")}>{icon}</span>
      <span className="text-sm text-white/85">{label}</span>
    </button>
  );
}

/** Mute / unmute. Must be rendered inside <CallMedia>. */
export function CallMicButton({ allowed }: { allowed: boolean }) {
  const { localParticipant, isMicrophoneEnabled } = useLocalParticipant();
  if (!allowed) return null;
  // Until the mic has been published once, show a calm "Mute" (it's switching on), never a red "Unmute".
  if (!localParticipant.getTrackPublication(Track.Source.Microphone)) return <CallButton icon={<Mic />} label="Mute" disabled />;
  return <CallButton icon={isMicrophoneEnabled ? <Mic /> : <MicOff />} label={isMicrophoneEnabled ? "Mute" : "Unmute"} off={!isMicrophoneEnabled}
    onClick={() => void localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled)} />;
}

/** Small mute toggle for the minimised strip. */
export function CallMicToggle() {
  const { localParticipant, isMicrophoneEnabled } = useLocalParticipant();
  return (
    <button type="button" aria-label={isMicrophoneEnabled ? "Mute" : "Unmute"} title={isMicrophoneEnabled ? "Mute" : "Unmute"} aria-pressed={!isMicrophoneEnabled}
      onClick={() => void localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled)}
      className={cn("flex size-10 items-center justify-center rounded-full [&_svg]:size-5", isMicrophoneEnabled ? "bg-white/12 text-white hover:bg-white/20" : "bg-[#EA4335] text-white hover:bg-[#D93025]")}>
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

/** Whether the server's grant (pushed after the screen-share API call) includes the screen. */
function screenGranted(room: Room): boolean {
  const permissions = room.localParticipant.permissions;
  if (!permissions?.canPublish) return false;
  const sources = (permissions.canPublishSources ?? []) as unknown[];
  // An empty list with canPublish means every source; 3 = SCREEN_SHARE in the protocol enum.
  return sources.length === 0 || sources.some((s) => s === 3 || s === "SCREEN_SHARE");
}

/** Resolves once the screen grant arrived (or after `ms`, whichever is first). */
function waitForScreenGrant(room: Room, ms: number): Promise<boolean> {
  if (screenGranted(room)) return Promise.resolve(true);
  return new Promise((resolve) => {
    const done = (value: boolean) => { room.off(RoomEvent.ParticipantPermissionsChanged, check); clearTimeout(timer); resolve(value); };
    const check = () => { if (screenGranted(room)) done(true); };
    const timer = setTimeout(() => done(screenGranted(room)), ms);
    room.on(RoomEvent.ParticipantPermissionsChanged, check);
  });
}

/**
 * Share screen (C6). The server decides: `onStart` asks Calling to make this user the call's
 * presenter (one per call); only then is the screen published, once the server's grant has
 * arrived. Stopping (here, the browser's "Stop sharing" bar, or the server — another stop, the
 * call ending) ends the local share and tells Calling. Must be rendered inside <CallMedia>.
 */
export function CallScreenShareButton({ presenting, onStart, onStop, onError }: {
  presenting: boolean;
  onStart: () => Promise<unknown>;
  onStop: () => Promise<unknown>;
  onError: (error: unknown) => void;
}) {
  const room = useRoomContext();
  const { localParticipant, isScreenShareEnabled } = useLocalParticipant();
  const [busy, setBusy] = useState(false);
  const wasSharing = useRef(false);
  // The server no longer lists us as presenter: stop publishing the screen.
  useEffect(() => {
    if (!presenting && isScreenShareEnabled && !busy) void localParticipant.setScreenShareEnabled(false).catch(() => undefined);
  }, [presenting, isScreenShareEnabled, busy, localParticipant]);
  // The browser's own "Stop sharing" ended the track: release the presenter role.
  useEffect(() => {
    if (wasSharing.current && !isScreenShareEnabled && presenting && !busy) void onStop().catch(() => undefined);
    wasSharing.current = isScreenShareEnabled;
  }, [isScreenShareEnabled, presenting, busy, onStop]);
  if (typeof navigator !== "undefined" && !navigator.mediaDevices?.getDisplayMedia) return null;
  const start = async () => {
    setBusy(true);
    try {
      await onStart();
      await waitForScreenGrant(room, 5000);
      try {
        await localParticipant.setScreenShareEnabled(true, { audio: true });
      } catch (e) {
        // Picker dismissed or refused: give the presenter role back.
        await onStop().catch(() => undefined);
        if (!(e instanceof Error && e.name === "NotAllowedError")) throw e;
      }
    } catch (e) { onError(e); } finally { setBusy(false); }
  };
  const stop = async () => {
    setBusy(true);
    try {
      await localParticipant.setScreenShareEnabled(false).catch(() => undefined);
      await onStop();
    } catch (e) { onError(e); } finally { setBusy(false); }
  };
  return presenting
    ? <CallButton icon={<MonitorX />} label="Stop sharing" tone="warn" disabled={busy} onClick={() => void stop()} />
    : <CallButton icon={<MonitorUp />} label="Share screen" disabled={busy} onClick={() => void start()} />;
}

/** Switch between front and back camera (phones). Hidden while the camera is off. */
/**
 * "Video" on a voice call (C5, WhatsApp): asks the server to make it a video call, then turns
 * your camera on as soon as the server allows it. Must be rendered inside <CallMedia>.
 */
export function CallUpgradeButton({ upgrade, onError }: { upgrade: () => Promise<unknown>; onError: (message: string) => void }) {
  const room = useRoomContext();
  const [busy, setBusy] = useState(false);
  const go = async () => {
    setBusy(true);
    try {
      await upgrade();
      const lp = room.localParticipant;
      const allowed = () => !!lp.permissions?.canPublish && (lp.permissions.canPublishSources.length === 0 || lp.permissions.canPublishSources.includes(1 /* CAMERA */));
      if (!allowed()) {
        await new Promise<void>((resolve, reject) => {
          const timer = window.setTimeout(() => { room.off(RoomEvent.ParticipantPermissionsChanged, check); reject(new Error("timeout")); }, 10_000);
          function check() { if (allowed()) { window.clearTimeout(timer); room.off(RoomEvent.ParticipantPermissionsChanged, check); resolve(); } }
          room.on(RoomEvent.ParticipantPermissionsChanged, check);
        });
      }
      await lp.setCameraEnabled(true);
    } catch {
      onError("Couldn't turn on your camera. Tap Camera to try again.");
    } finally {
      setBusy(false);
    }
  };
  return <CallButton icon={<Video />} label="Video" disabled={busy} onClick={() => void go()} />;
}

/** Phones switch front ⇄ back (like WhatsApp); computers step through their cameras. */
const isPhone = () => typeof navigator !== "undefined" && /Android|iPhone|iPad|iPod/i.test(navigator.userAgent);

export function CallFlipButton({ onError }: { onError?: (message: string) => void } = {}) {
  const { localParticipant, isCameraEnabled } = useLocalParticipant();
  const [cameras, setCameras] = useState<MediaDeviceInfo[]>([]);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const load = () => void navigator.mediaDevices?.enumerateDevices().then((d) => setCameras(d.filter((x) => x.kind === "videoinput" && x.deviceId))).catch(() => undefined);
    load();
    navigator.mediaDevices?.addEventListener?.("devicechange", load);
    return () => navigator.mediaDevices?.removeEventListener?.("devicechange", load);
  }, [isCameraEnabled]);
  if (!isCameraEnabled || cameras.length < 2) return null;
  const flip = async () => {
    const track = [...localParticipant.videoTrackPublications.values()].find((t) => t.source === Track.Source.Camera)?.track as LocalVideoTrack | undefined;
    if (!track) return;
    const settings = track.mediaStreamTrack.getSettings();
    setBusy(true);
    try {
      if (isPhone()) {
        await track.restartTrack({ facingMode: settings.facingMode === "environment" ? "user" : "environment" });
      } else {
        const next = cameras[(cameras.findIndex((c) => c.deviceId === settings.deviceId) + 1) % cameras.length];
        await track.restartTrack({ deviceId: next.deviceId });
      }
    } catch {
      onError?.("Couldn't switch camera. Another app may be using it.");
    } finally {
      setBusy(false);
    }
  };
  return <CallButton icon={<SwitchCamera />} label="Flip" disabled={busy} onClick={() => void flip()} />;
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
  // Kept in place (dimmed) where the browser can't switch output, so the buttons never jump.
  if (outputs.length < 2) return <CallButton icon={<Volume2 />} label="Speaker" disabled />;
  const next = (index + 1) % outputs.length;
  const label = outputs[index]?.label?.split("(")[0]?.trim() || "Speaker";
  return <CallButton icon={<Volume2 />} label={label.length > 10 ? "Speaker" : label} active={index !== 0}
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
  // The call is over (or this media connection is replaced): the camera it took from the
  // preview is stopped and forgotten, so the next call's preview opens the camera afresh
  // instead of showing this call's stopped stream, and the camera light goes off.
  useEffect(() => () => {
    const p = preview;
    if (!p?.taken) return;
    preview = null;
    void p.stream.then((s) => s?.getTracks().forEach((t) => t.stop()));
  }, []);
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
      {/* Old calls (APP_MEDIA_V1) have no canPublishData, so they can't record. */}
      <DeviceRecordingProvider label="call" canStart={credential.capabilities.canPublishAudio && credential.capabilities.canPublishData}>{children}</DeviceRecordingProvider>
    </LiveKitRoom>
  );
}

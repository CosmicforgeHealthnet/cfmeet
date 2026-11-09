"use client";

import { useEffect } from "react";
import { LiveKitRoom, RoomAudioRenderer, TrackToggle, useRoomContext, useTracks, VideoTrack } from "@livekit/components-react";
import { DisconnectReason, RoomEvent, Track } from "livekit-client";
import type { MediaCredential } from "@/types/calling";

function Cameras() {
  const tracks = useTracks([Track.Source.Camera]);
  if (!tracks.length) return null;
  return <div className="grid gap-2 sm:grid-cols-2">{tracks.map((track) =>
    <VideoTrack key={`${track.participant.identity}-${track.publication.trackSid}`} trackRef={track} className="aspect-video w-full rounded-lg bg-muted object-cover" />,
  )}</div>;
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
 * The media connection of one call session. It reports every connection transition; the
 * reconnect policy (fresh credential + bounded backoff) lives in the calling provider.
 */
export default function CallMedia({ credential, video, onConnected, onDisconnected, onProviderReconnecting, onProviderReconnected, onError }: {
  credential: MediaCredential;
  video: boolean;
  onConnected: () => void;
  /** `userInitiated` is true for this client's own disconnect (leave), false when the connection was lost. */
  onDisconnected: (userInitiated: boolean) => void;
  onProviderReconnecting: () => void;
  onProviderReconnected: () => void;
  onError: (error: Error) => void;
}) {
  return <LiveKitRoom serverUrl={credential.url} token={credential.token} connect
    audio={credential.capabilities.canPublishAudio} video={video && credential.capabilities.canPublishVideo}
    onConnected={onConnected}
    onDisconnected={(reason) => onDisconnected(reason === DisconnectReason.CLIENT_INITIATED)}
    onError={onError} className="space-y-2">
    <RoomAudioRenderer />
    <ProviderReconnectEvents onReconnecting={onProviderReconnecting} onReconnected={onProviderReconnected} />
    {video ? <Cameras /> : null}
    <div className="flex flex-wrap gap-2">
      {credential.capabilities.canPublishAudio ? <TrackToggle source={Track.Source.Microphone} className="rounded-md border border-input px-3 py-1.5 text-sm">Microphone</TrackToggle> : null}
      {video && credential.capabilities.canPublishVideo ? <TrackToggle source={Track.Source.Camera} className="rounded-md border border-input px-3 py-1.5 text-sm">Camera</TrackToggle> : null}
    </div>
  </LiveKitRoom>;
}

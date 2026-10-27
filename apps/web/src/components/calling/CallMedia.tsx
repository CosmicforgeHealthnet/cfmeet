"use client";

import { LiveKitRoom, RoomAudioRenderer, TrackToggle, useTracks, VideoTrack } from "@livekit/components-react";
import { Track } from "livekit-client";
import { Button } from "@/components/ui/button";
import type { MediaCredential } from "@/types/calling";

function Cameras() {
  const tracks = useTracks([Track.Source.Camera]);
  return <div className="grid gap-3 sm:grid-cols-2">{tracks.map((track) =>
    <VideoTrack key={`${track.participant.identity}-${track.publication.trackSid}`} trackRef={track} className="aspect-video w-full rounded-xl bg-muted object-cover" />,
  )}</div>;
}

export default function CallMedia({ credential, video, onLeave, onError }: {
  credential: MediaCredential; video: boolean; onLeave: () => void; onError: (error: Error) => void;
}) {
  return <LiveKitRoom serverUrl={credential.url} token={credential.token} connect
    audio={credential.capabilities.canPublishAudio} video={video && credential.capabilities.canPublishVideo}
    onDisconnected={onLeave} onError={onError} className="space-y-3 rounded-2xl border border-border bg-card p-4">
    <RoomAudioRenderer />
    <Cameras />
    <p className="text-sm text-muted-foreground">Call audio is connected. Allow microphone access when your browser asks.</p>
    <div className="flex flex-wrap gap-2">
      {credential.capabilities.canPublishAudio ? <TrackToggle source={Track.Source.Microphone} className="rounded-md border border-input px-3 py-2 text-sm">Microphone</TrackToggle> : null}
      {video && credential.capabilities.canPublishVideo ? <TrackToggle source={Track.Source.Camera} className="rounded-md border border-input px-3 py-2 text-sm">Camera</TrackToggle> : null}
      <Button variant="outline" onClick={onLeave}>Disconnect this device</Button>
    </div>
  </LiveKitRoom>;
}

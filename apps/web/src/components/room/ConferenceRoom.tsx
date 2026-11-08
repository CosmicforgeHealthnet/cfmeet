"use client";

import { useState } from "react";
import LiveRoom from "./live-room";
import PreJoinScreen, { type MediaPreferences } from "./pre-join-screen";
import type { RoomJoinData } from "@/lib/prejoin";

/** Pre-join → (waiting) → live for one conference id (FE-017). */
export default function ConferenceRoom({ conferenceId, initialInvitationToken }: { conferenceId: string; initialInvitationToken?: string }) {
  const [joined, setJoined] = useState<{ data: RoomJoinData; prefs: MediaPreferences } | null>(null);
  if (joined && joined.data.conferenceId === conferenceId) {
    return <LiveRoom joinData={joined.data} initialVideo={joined.prefs.isCameraOn} initialAudio={joined.prefs.isMicOn} />;
  }
  return <PreJoinScreen conferenceId={conferenceId} initialInvitationToken={initialInvitationToken} onJoin={(data, prefs) => setJoined({ data, prefs })} />;
}

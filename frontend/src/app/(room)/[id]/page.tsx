"use client";

import { useState, useEffect } from "react";
import { useParams } from "next/navigation";
import LiveRoom from "@/components/room/live-room";
import PreJoinScreen from "@/components/room/pre-join-screen";
import type { JoinMeetingData } from "@/types/meeting";
import { storageStore } from "@/store";

interface MediaPreferences {
  isCameraOn: boolean;
  isMicOn: boolean;
}

export default function RoomPage() {
  const params = useParams();
  const id = params?.id as string;
  const [joinData, setJoinData] = useState<JoinMeetingData | null>(null);
  const [mediaPrefs, setMediaPrefs] = useState<MediaPreferences>({ isCameraOn: true, isMicOn: true });
  const [isCheckingInstant, setIsCheckingInstant] = useState(true);

  // Check for instant meeting join data on mount
  useEffect(() => {
    const instantData = storageStore.getInstantJoinData();
    if (instantData && instantData.room_name === id) {
      setJoinData(instantData);
      storageStore.clearInstantJoinData();
    }
    setIsCheckingInstant(false);
  }, [id]);

  const handleJoin = (data: JoinMeetingData, prefs: MediaPreferences) => {
    setMediaPrefs(prefs);
    setJoinData(data);
  };

  if (!id) return null;

  // Show nothing while checking for instant join data
  if (isCheckingInstant) return null;

  if (!joinData) {
    return <PreJoinScreen roomId={id} onJoin={handleJoin} />;
  }

  return <LiveRoom joinData={joinData} initialVideo={mediaPrefs.isCameraOn} initialAudio={mediaPrefs.isMicOn} />;
}

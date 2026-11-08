/**
 * Live room (FE-004 / MTG-002): real LiveKit media with the conference's RTC credential from
 * `POST /conferences/{id}/join`, plus Meeting Service v2 participants, waiting room, chat and
 * hand raise. The credential lives only in memory (features/room/live-session.ts).
 */
import { useCallback, useEffect, useState } from "react";
import { Alert, Modal, Pressable, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router, useLocalSearchParams } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { AudioSession, LiveKitRoom, useConnectionState, useLocalParticipant } from "@livekit/react-native";
import { ConnectionState } from "livekit-client";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { ScreenHeader } from "@/components/shared/screen-header";
import { useLiveSession } from "@/features/room/live-session";
import { VideoGrid } from "@/features/room/components/video-grid";
import { ChatPanel } from "@/features/room/components/chat-panel";
import { ParticipantsPanel } from "@/features/room/components/participants-panel";
import { useHands } from "@/features/room/hooks";
import { meetingsApi } from "@/features/meetings/api";
import { decodeApiError } from "@/lib/api/errors";
import { classifyJoin, nextWaitingStep } from "@/lib/meeting/admission";
import { effectiveRole, meetingAllows } from "@/lib/meeting/authority";

type Panel = "chat" | "people" | null;

export default function LiveRoomScreen() {
  const params = useLocalSearchParams<{ id: string; mic?: string; cam?: string }>();
  const media = useLiveSession((s) => s.media);
  const isGuest = useLiveSession((s) => s.isGuest);
  const [ended, setEnded] = useState<string | null>(null);

  useEffect(() => {
    void AudioSession.startAudioSession();
    return () => {
      void AudioSession.stopAudioSession();
    };
  }, []);

  if (!media || media.conferenceId !== params.id) {
    return (
      <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
        <ScreenHeader title="Meeting" />
        <View className="flex-1 items-center justify-center p-8">
          <Text className="text-lg font-bold text-center">{ended ?? "You're not in this meeting"}</Text>
          <Button
            className="mt-6"
            onPress={() => router.replace({ pathname: "/(room)/[id]", params: { id: params.id } })}
          >
            Join again
          </Button>
          <Button variant="ghost" className="mt-2" onPress={() => router.replace(isGuest ? "/join" : "/(tabs)")}>
            Close
          </Button>
        </View>
      </SafeAreaView>
    );
  }

  return (
    <LiveKitRoom
      serverUrl={media.serverUrl}
      token={media.token}
      connect
      audio={params.mic !== "0" && media.permissions.audioPublish}
      video={params.cam !== "0" && media.permissions.videoPublish}
      options={{ adaptiveStream: { pixelDensity: "screen" } }}
    >
      <RoomBody onEnded={setEnded} />
    </LiveKitRoom>
  );
}

function RoomBody({ onEnded }: { onEnded: (reason: string) => void }) {
  const media = useLiveSession((s) => s.media)!;
  const spaceRole = useLiveSession((s) => s.spaceRole);
  const title = useLiveSession((s) => s.title);
  const isGuest = useLiveSession((s) => s.isGuest);
  const replaceMedia = useLiveSession((s) => s.replaceMedia);
  const clear = useLiveSession((s) => s.clear);
  const connection = useConnectionState();
  const { localParticipant, isMicrophoneEnabled, isCameraEnabled } = useLocalParticipant();
  const [panel, setPanel] = useState<Panel>(null);
  const [busy, setBusy] = useState(false);
  const role = effectiveRole(spaceRole, media.role);
  const hands = useHands(media.conferenceId, true);
  const handRaised = (hands.query.data?.hands ?? []).some((h) => h.participantId === media.participantId);

  const exit = useCallback(
    async (reason?: string) => {
      await clear();
      if (reason) onEnded(reason);
      router.replace(isGuest ? "/join" : "/(tabs)");
    },
    [clear, isGuest, onEnded]
  );

  const leave = useCallback(async () => {
    setBusy(true);
    try {
      await meetingsApi.leave(media.conferenceId, { participantSessionId: media.participantSessionId });
    } catch {
      // Leaving locally still disconnects media; the server session expires.
    }
    await exit();
  }, [media, exit]);

  const endForAll = useCallback(() => {
    Alert.alert("End meeting for everyone?", "Everyone will be disconnected.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "End meeting",
        style: "destructive",
        onPress: async () => {
          setBusy(true);
          try {
            await meetingsApi.endConference(media.conferenceId);
            await exit();
          } catch (e) {
            setBusy(false);
            Alert.alert("Could not end the meeting", decodeApiError(e).message);
          }
        },
      },
    ]);
  }, [media.conferenceId, exit]);

  /** After an unexpected disconnect: refused → stop; otherwise reconnect as the same participant. */
  const reconnect = useCallback(async () => {
    setBusy(true);
    try {
      const me = await meetingsApi.me(media.conferenceId);
      const step = nextWaitingStep(me);
      if (step.kind === "refused") {
        await exit(step.reason);
        return;
      }
      const outcome = classifyJoin(
        await meetingsApi.join(media.conferenceId, {
          displayName: media.displayName,
          previousSessionId: media.participantSessionId,
        })
      );
      if (outcome.kind === "live") replaceMedia(outcome.media);
      else await exit(outcome.kind === "refused" || outcome.kind === "media_unavailable" ? outcome.reason : "You left the meeting.");
    } catch (e) {
      const decoded = decodeApiError(e);
      if (decoded.code === "CONFERENCE_NOT_LIVE") await exit("The meeting has ended.");
      else Alert.alert("Could not reconnect", decoded.message);
    } finally {
      setBusy(false);
    }
  }, [media, replaceMedia, exit]);

  const toggleMic = async () => {
    try {
      await localParticipant.setMicrophoneEnabled(!isMicrophoneEnabled);
    } catch (e) {
      Alert.alert("Microphone", e instanceof Error ? e.message : "Unavailable");
    }
  };
  const toggleCam = async () => {
    try {
      await localParticipant.setCameraEnabled(!isCameraEnabled);
    } catch (e) {
      Alert.alert("Camera", e instanceof Error ? e.message : "Unavailable");
    }
  };

  const disconnected = connection === ConnectionState.Disconnected;

  return (
    <SafeAreaView className="flex-1 bg-slate-900">
      <View className="flex-row items-center justify-between px-4 py-3">
        <View className="flex-1">
          <Text className="text-white font-bold" numberOfLines={1}>
            {title ?? "Meeting"}
          </Text>
          <Text className="text-slate-300 text-xs">
            {connection === ConnectionState.Connected
              ? "Connected"
              : connection === ConnectionState.Reconnecting
                ? "Reconnecting…"
                : connection === ConnectionState.Connecting
                  ? "Connecting…"
                  : "Disconnected"}
            {media.recordingActive ? " · Recording" : ""}
            {media.transcriptionActive ? " · Transcribing" : ""}
          </Text>
        </View>
        {meetingAllows(role, "conference.end") && (
          <Button size="sm" variant="destructive" onPress={endForAll} disabled={busy}>
            End
          </Button>
        )}
      </View>

      {disconnected ? (
        <View className="flex-1 items-center justify-center p-8">
          <Text className="text-white text-lg font-bold">Disconnected</Text>
          <Text className="text-slate-300 text-center mt-2">The media connection was lost.</Text>
          <Button className="mt-6" isLoading={busy} onPress={reconnect}>
            Reconnect
          </Button>
          <Button variant="ghost" className="mt-2" onPress={leave}>
            <Text className="text-white">Leave</Text>
          </Button>
        </View>
      ) : (
        <View className="flex-1">
          <VideoGrid />
        </View>
      )}

      <View className="flex-row items-center justify-around px-2 py-3 bg-slate-800">
        <ControlButton
          icon={isMicrophoneEnabled ? "mic" : "mic-off"}
          label="Mic"
          onPress={toggleMic}
          disabled={!media.permissions.audioPublish}
          active={isMicrophoneEnabled}
        />
        <ControlButton
          icon={isCameraEnabled ? "videocam" : "videocam-off"}
          label="Camera"
          onPress={toggleCam}
          disabled={!media.permissions.videoPublish}
          active={isCameraEnabled}
        />
        <ControlButton
          icon="hand-left"
          label={handRaised ? "Lower" : "Raise"}
          onPress={() => hands.toggle.mutate(!handRaised)}
          active={handRaised}
        />
        <ControlButton icon="chatbubble-ellipses" label="Chat" onPress={() => setPanel("chat")} />
        <ControlButton icon="people" label="People" onPress={() => setPanel("people")} />
        <ControlButton icon="call" label="Leave" onPress={leave} danger disabled={busy} />
      </View>

      <Modal visible={panel !== null} animationType="slide" presentationStyle="pageSheet" onRequestClose={() => setPanel(null)}>
        <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
          <ScreenHeader title={panel === "chat" ? "Chat" : "People"} onBack={() => setPanel(null)} />
          {panel === "chat" ? (
            <ChatPanel
              conferenceId={media.conferenceId}
              participantId={media.participantId}
              canSend={media.permissions.dataPublish}
            />
          ) : panel === "people" ? (
            <ParticipantsPanel conferenceId={media.conferenceId} participantId={media.participantId} role={role} />
          ) : null}
        </SafeAreaView>
      </Modal>
    </SafeAreaView>
  );
}

function ControlButton({
  icon,
  label,
  onPress,
  disabled,
  active,
  danger,
}: {
  icon: React.ComponentProps<typeof Ionicons>["name"];
  label: string;
  onPress: () => void;
  disabled?: boolean;
  active?: boolean;
  danger?: boolean;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={label}
      onPress={onPress}
      disabled={disabled}
      className={`items-center ${disabled ? "opacity-40" : ""}`}
    >
      <View
        className={`h-12 w-12 rounded-full items-center justify-center ${
          danger ? "bg-red-600" : active ? "bg-slate-600" : "bg-slate-700"
        }`}
      >
        <Ionicons name={icon} size={22} color="#fff" />
      </View>
      <Text className="text-xs text-slate-200 mt-1">{label}</Text>
    </Pressable>
  );
}

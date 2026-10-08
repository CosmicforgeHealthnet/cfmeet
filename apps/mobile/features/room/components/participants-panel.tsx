import { View, FlatList, Pressable } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { useRoom } from "../providers/room-provider";
import {
  useParticipants,
  useWaitingParticipants,
  useAdmitParticipant,
  useDenyParticipant,
  useAdmitAllParticipants,
} from "../hooks/use-participants";
import { useToast } from "@/components/ui/toast";
import type { Participant, WaitingParticipant } from "@/features/meetings/types";

export function ParticipantsPanel() {
  const { joinData } = useRoom();
  const meetingId = joinData?.id ?? "";
  const isHost = joinData?.role === "host";
  const currentParticipantId = joinData?.participant_id;

  const { data: participantsData, isLoading: isLoadingParticipants } = useParticipants(meetingId);
  const { data: waitingData, isLoading: isLoadingWaiting } = useWaitingParticipants(meetingId, isHost);

  const { toast, success } = useToast();
  const admitParticipant = useAdmitParticipant(meetingId);
  const denyParticipant = useDenyParticipant(meetingId);
  const admitAll = useAdmitAllParticipants(meetingId);

  const handleAdmit = (id: string) => {
    admitParticipant.mutate(id, {
      onSuccess: () => success("Participant admitted"),
      onError: (error) => toast(error.message, "destructive"),
    });
  };

  const handleDeny = (id: string) => {
    denyParticipant.mutate(id, {
      onSuccess: () => success("Participant denied"),
      onError: (error) => toast(error.message, "destructive"),
    });
  };

  const handleAdmitAll = () => {
    if (!waitingData?.waiting) return;
    const ids = waitingData.waiting.map((p) => p.id);
    admitAll.mutate(ids, {
      onSuccess: () => success("All participants admitted"),
      onError: (error) => toast(error.message, "destructive"),
    });
  };

  const renderParticipant = ({ item }: { item: Participant }) => {
    const isCurrentUser = item.id === currentParticipantId;

    return (
      <View className="flex-row items-center justify-between py-3">
        <View className="flex-row items-center gap-3">
          <Avatar fallback={item.display_name} size="default" />
          <View>
            <Text className="font-semibold text-foreground dark:text-foreground-dark">
              {item.display_name}
              {isCurrentUser && " (You)"}
            </Text>
            {item.role === "host" && (
              <Text className="text-xs text-primary dark:text-primary-dark">Host</Text>
            )}
          </View>
        </View>
        <View className="flex-row gap-3">
          <Ionicons
            name={item.is_audio_enabled ? "mic" : "mic-off"}
            size={18}
            color={item.is_audio_enabled ? "#22c55e" : "#737373"}
          />
          <Ionicons
            name={item.is_video_enabled ? "videocam" : "videocam-off"}
            size={18}
            color={item.is_video_enabled ? "#22c55e" : "#737373"}
          />
        </View>
      </View>
    );
  };

  const renderWaitingParticipant = ({ item }: { item: WaitingParticipant }) => (
    <View className="flex-row items-center justify-between py-3">
      <View className="flex-row items-center gap-3">
        <Avatar fallback={item.display_name} size="default" />
        <Text className="font-semibold text-foreground dark:text-foreground-dark">{item.display_name}</Text>
      </View>
      <View className="flex-row gap-2">
        <Pressable
          onPress={() => handleAdmit(item.id)}
          disabled={admitParticipant.isPending}
          className="rounded-lg bg-success dark:bg-success-dark px-3 py-1.5"
        >
          <Text className="text-sm font-bold text-white">Admit</Text>
        </Pressable>
        <Pressable
          onPress={() => handleDeny(item.id)}
          disabled={denyParticipant.isPending}
          className="rounded-lg bg-destructive dark:bg-destructive-dark px-3 py-1.5"
        >
          <Text className="text-sm font-bold text-white">Deny</Text>
        </Pressable>
      </View>
    </View>
  );

  if (isLoadingParticipants) {
    return <Loading message="Loading participants..." />;
  }

  const participants = participantsData?.participants ?? [];
  const waitingParticipants = waitingData?.waiting ?? [];

  return (
    <View className="flex-1">
      {isHost && waitingParticipants.length > 0 && (
        <View className="mb-4">
          <View className="mb-2 flex-row items-center justify-between">
            <View className="flex-row items-center gap-2">
              <Text className="font-bold text-foreground dark:text-foreground-dark">Waiting Room</Text>
              <Badge variant="secondary">{waitingParticipants.length.toString()}</Badge>
            </View>
            <Button
              variant="outline"
              size="sm"
              onPress={handleAdmitAll}
              isLoading={admitAll.isPending}
            >
              Admit All
            </Button>
          </View>
          <FlatList
            data={waitingParticipants}
            keyExtractor={(item) => item.id}
            renderItem={renderWaitingParticipant}
            scrollEnabled={false}
          />
        </View>
      )}

      <View className="flex-1">
        <View className="mb-2 flex-row items-center gap-2">
          <Text className="font-bold text-foreground dark:text-foreground-dark">Participants</Text>
          <Badge variant="secondary">{participants.length.toString()}</Badge>
        </View>
        <FlatList
          data={participants}
          keyExtractor={(item) => item.id}
          renderItem={renderParticipant}
          showsVerticalScrollIndicator={false}
        />
      </View>
    </View>
  );
}

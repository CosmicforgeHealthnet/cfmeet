import { View, Dimensions } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Avatar } from "@/components/ui/avatar";
import { useRoom } from "../providers/room-provider";
import { useParticipants } from "../hooks/use-participants";
import type { Participant } from "@/features/meetings/types";

const { width: SCREEN_WIDTH } = Dimensions.get("window");

function ParticipantTile({ participant, size }: { participant: Participant; size: number }) {
  const { joinData } = useRoom();
  const isCurrentUser = participant.id === joinData?.participant_id;

  return (
    <View
      style={{ width: size, height: size }}
      className="items-center justify-center rounded-xl bg-muted dark:bg-muted-dark p-1"
    >
      <View className="flex-1 w-full items-center justify-center rounded-lg bg-card dark:bg-card-dark">
        <Avatar fallback={participant.display_name} size="xl" />
        <View className="absolute bottom-2 left-2 right-2 flex-row items-center justify-between">
          <View className="rounded-md bg-black/60 px-2 py-1">
            <Text className="text-xs text-white font-semibold" numberOfLines={1}>
              {participant.display_name}
              {isCurrentUser && " (You)"}
            </Text>
          </View>
          <View className="flex-row gap-1">
            {!participant.is_audio_enabled && (
              <View className="rounded-full bg-destructive dark:bg-destructive-dark p-1">
                <Ionicons name="mic-off" size={12} color="#ffffff" />
              </View>
            )}
          </View>
        </View>
        {participant.role === "host" && (
          <View className="absolute right-2 top-2 rounded-md bg-primary dark:bg-primary-dark px-2 py-0.5">
            <Text className="text-xs font-bold text-primary-foreground dark:text-primary-foreground-dark">Host</Text>
          </View>
        )}
      </View>
    </View>
  );
}

export function VideoGrid() {
  const { joinData } = useRoom();
  const meetingId = joinData?.id ?? "";
  const { data } = useParticipants(meetingId);

  const participants = data?.participants ?? [];
  const count = participants.length;

  const calculateTileSize = () => {
    const padding = 16;
    const gap = 8;
    const availableWidth = SCREEN_WIDTH - padding * 2;

    if (count <= 1) {
      return availableWidth;
    } else if (count <= 4) {
      return (availableWidth - gap) / 2;
    } else {
      return (availableWidth - gap * 2) / 3;
    }
  };

  const tileSize = calculateTileSize();

  if (count === 0) {
    return (
      <View className="flex-1 items-center justify-center">
        <Ionicons name="people-outline" size={48} color="#029CD4" style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-muted-foreground dark:text-muted-foreground-dark">Waiting for participants...</Text>
      </View>
    );
  }

  return (
    <View className="flex-1 p-4">
      <View className="flex-1 flex-row flex-wrap justify-center gap-2 content-center">
        {participants.map((participant) => (
          <ParticipantTile
            key={participant.id}
            participant={participant}
            size={tileSize}
          />
        ))}
      </View>
    </View>
  );
}

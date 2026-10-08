import { View, Pressable } from "react-native";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { useRoom } from "../providers/room-provider";
import { useEndMeeting } from "../hooks/use-media-controls";
import { useToast } from "@/components/ui/toast";

export function RoomControls() {
  const {
    joinData,
    isAudioEnabled,
    isVideoEnabled,
    toggleAudio,
    toggleVideo,
    toggleSidebar,
  } = useRoom();

  const { toast } = useToast();
  const endMeeting = useEndMeeting(joinData?.id ?? "");

  const handleLeave = () => {
    router.replace("/(tabs)");
  };

  const handleEndMeeting = () => {
    endMeeting.mutate(undefined, {
      onSuccess: () => {
        router.replace("/(tabs)");
      },
      onError: (error) => {
        toast(error.message, "destructive");
      },
    });
  };

  const isHost = joinData?.role === "host";

  return (
    <View className="flex-row items-center justify-center gap-3 bg-card dark:bg-card-dark px-4 py-4">
      <Pressable
        onPress={toggleAudio}
        className={`h-12 w-12 items-center justify-center rounded-full ${
          isAudioEnabled ? "bg-secondary dark:bg-secondary-dark" : "bg-destructive dark:bg-destructive-dark"
        }`}
      >
        <Ionicons
          name={isAudioEnabled ? "mic" : "mic-off"}
          size={22}
          color={isAudioEnabled ? "#029CD4" : "#ffffff"}
        />
      </Pressable>

      <Pressable
        onPress={toggleVideo}
        className={`h-12 w-12 items-center justify-center rounded-full ${
          isVideoEnabled ? "bg-secondary dark:bg-secondary-dark" : "bg-destructive dark:bg-destructive-dark"
        }`}
      >
        <Ionicons
          name={isVideoEnabled ? "videocam" : "videocam-off"}
          size={22}
          color={isVideoEnabled ? "#029CD4" : "#ffffff"}
        />
      </Pressable>

      <Pressable
        onPress={toggleSidebar}
        className="h-12 w-12 items-center justify-center rounded-full bg-secondary dark:bg-secondary-dark"
      >
        <Ionicons name="chatbubble-ellipses" size={22} color="#029CD4" />
      </Pressable>

      {isHost ? (
        <Pressable
          onPress={handleEndMeeting}
          disabled={endMeeting.isPending}
          className="h-12 items-center justify-center rounded-full bg-destructive dark:bg-destructive-dark px-6"
        >
          <Text className="font-bold text-white">End</Text>
        </Pressable>
      ) : (
        <Pressable
          onPress={handleLeave}
          className="h-12 items-center justify-center rounded-full bg-destructive dark:bg-destructive-dark px-6"
        >
          <Text className="font-bold text-white">Leave</Text>
        </Pressable>
      )}
    </View>
  );
}

import { useLocalSearchParams } from "expo-router";
import { View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { RoomProvider } from "@/features/room/providers/room-provider";
import { VideoGrid } from "@/features/room/components/video-grid";
import { RoomControls } from "@/features/room/components/room-controls";
import { RoomSidebar } from "@/features/room/components/room-sidebar";
import { ErrorState } from "@/components/shared/error-state";
import type { JoinMeetingData } from "@/features/meetings/types";

export default function LiveRoomScreen() {
  const { joinData: joinDataString } = useLocalSearchParams<{ joinData: string }>();

  let joinData: JoinMeetingData | null = null;

  try {
    if (joinDataString) {
      joinData = JSON.parse(joinDataString) as JoinMeetingData;
    }
  } catch {
    joinData = null;
  }

  if (!joinData) {
    return (
      <SafeAreaView className="flex-1 bg-background">
        <ErrorState
          title="Invalid Room"
          message="Unable to join this meeting. Please try again."
        />
      </SafeAreaView>
    );
  }

  return (
    <RoomProvider initialJoinData={joinData}>
      <SafeAreaView edges={["top"]} className="flex-1 bg-background">
        <View className="flex-1">
          <VideoGrid />
        </View>
        <RoomControls />
        <RoomSidebar />
      </SafeAreaView>
    </RoomProvider>
  );
}

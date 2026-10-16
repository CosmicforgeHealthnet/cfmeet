import { SafeAreaView } from "react-native-safe-area-context";
import { View } from "react-native";
import { Text } from "@/components/ui/text";
import { MeetingList } from "@/features/meetings/components/meeting-list";

export default function MeetingsScreen() {
  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <View className="border-b border-border dark:border-border-dark px-6 py-4">
        <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">Meetings</Text>
        <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark mt-1">
          View and manage your meetings
        </Text>
      </View>
      <MeetingList />
    </SafeAreaView>
  );
}

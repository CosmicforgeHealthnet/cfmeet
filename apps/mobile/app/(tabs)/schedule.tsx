import { router } from "expo-router";
import { ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Text } from "@/components/ui/text";
import { ScheduleForm } from "@/features/meetings/components/schedule-form";

export default function ScheduleScreen() {
  const handleSuccess = () => {
    router.push("/(tabs)/meetings");
  };

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <View className="border-b border-border dark:border-border-dark px-6 py-4">
        <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">Schedule Meeting</Text>
        <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark mt-1">
          Plan your next meeting
        </Text>
      </View>
      <ScrollView
        className="flex-1"
        contentContainerStyle={{ padding: 24, paddingBottom: 120 }}
        showsVerticalScrollIndicator={false}
        keyboardShouldPersistTaps="handled"
      >
        <ScheduleForm onSuccess={handleSuccess} />
      </ScrollView>
    </SafeAreaView>
  );
}

import { View, ScrollView, Pressable } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { useToast } from "@/components/ui/toast";
import { JoinMeetingInput } from "@/features/meetings/components/join-meeting-input";
import { useInstantMeeting, useMeetings } from "@/features/meetings/hooks/use-meetings";
import { useAuthStore } from "@/store/auth.store";
import type { Meeting } from "@/features/meetings/types";
import * as Clipboard from "expo-clipboard";

function MeetingHistoryCard({ meeting }: { meeting: Meeting }) {
  const { success } = useToast();

  const formatDate = (dateString: string) => {
    const date = new Date(dateString);
    return date.toLocaleDateString("en-US", { month: "short", day: "numeric" });
  };

  const handleCopy = async () => {
    await Clipboard.setStringAsync(meeting.meeting_identifier);
    success("Code copied!");
  };

  const statusVariant = {
    live: "success" as const,
    scheduled: "default" as const,
    ended: "secondary" as const,
    cancelled: "destructive" as const,
  };

  return (
    <Card className="mr-3 w-64 border border-border dark:border-border-dark">
      <View className="flex-row justify-between items-start">
        <Text className="font-bold text-primary dark:text-primary-dark flex-1" numberOfLines={1}>
          {meeting.title}
        </Text>
        <Pressable onPress={handleCopy} className="p-1">
          <Ionicons name="copy-outline" size={16} color="#737373" />
        </Pressable>
      </View>
      <Text className="mt-2 text-sm text-muted-foreground dark:text-muted-foreground-dark">
        {formatDate(meeting.scheduled_at)}
      </Text>
      <View className="mt-2 flex-row items-center justify-between">
        <Badge variant={statusVariant[meeting.status]}>{meeting.status}</Badge>
        <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark">
          {meeting.meeting_identifier}
        </Text>
      </View>
    </Card>
  );
}

export default function HomeScreen() {
  const { user } = useAuthStore();
  const { data: meetingsData } = useMeetings(1, 5);
  const instantMeeting = useInstantMeeting();
  const { toast } = useToast();

  const handleNewMeeting = () => {
    instantMeeting.mutate(undefined, {
      onSuccess: (response) => {
        router.push({
          pathname: "/(room)/[id]/live",
          params: {
            id: response.meeting.id,
            joinData: JSON.stringify(response.join_data),
          },
        });
      },
      onError: (error) => {
        toast(error.message, "destructive");
      },
    });
  };

  const recentMeetings = meetingsData?.meetings ?? [];

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <ScrollView className="flex-1" showsVerticalScrollIndicator={false} contentContainerStyle={{ paddingBottom: 100 }}>
        <View className="px-6 py-4">
          <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">
            Welcome, {user?.username ?? "User"}
          </Text>
          <Text className="mt-1 text-muted-foreground dark:text-muted-foreground-dark">
            Start or join a meeting
          </Text>
        </View>

        {/* Action Buttons - matching web design */}
        <View className="flex-row justify-center gap-8 mt-6 px-6">
          <Pressable
            onPress={handleNewMeeting}
            disabled={instantMeeting.isPending}
            className="items-center"
          >
            <View className="w-20 h-20 rounded-2xl bg-primary dark:bg-primary-dark items-center justify-center shadow-lg">
              {instantMeeting.isPending ? (
                <Ionicons name="sync" size={28} color="#ffffff" />
              ) : (
                <Ionicons name="videocam" size={28} color="#ffffff" />
              )}
            </View>
            <Text className="mt-3 font-bold text-foreground dark:text-foreground-dark">
              {instantMeeting.isPending ? "Starting..." : "New Meeting"}
            </Text>
          </Pressable>

          <Pressable
            onPress={() => router.push("/(tabs)/schedule")}
            className="items-center"
          >
            <View className="w-20 h-20 rounded-2xl bg-secondary dark:bg-secondary-dark items-center justify-center shadow">
              <Ionicons name="calendar" size={28} color="#006BFF" />
            </View>
            <Text className="mt-3 font-bold text-foreground dark:text-foreground-dark">
              Schedule
            </Text>
          </Pressable>
        </View>

        {/* Join Meeting Input */}
        <View className="mt-8 px-6">
          <Text className="mb-3 font-bold text-foreground dark:text-foreground-dark">Join a Meeting</Text>
          <JoinMeetingInput />
        </View>

        {/* Meeting History */}
        {recentMeetings.length > 0 && (
          <View className="mt-10">
            <View className="mb-4 flex-row items-center justify-between px-6">
              <Text className="text-lg font-bold text-foreground dark:text-foreground-dark">
                Meeting History
              </Text>
              <Pressable onPress={() => router.push("/(tabs)/meetings")} className="flex-row items-center gap-1">
                <Text className="text-sm font-semibold text-primary dark:text-primary-dark">See all</Text>
                <Ionicons name="chevron-forward" size={16} color="#006BFF" />
              </Pressable>
            </View>
            <ScrollView
              horizontal
              showsHorizontalScrollIndicator={false}
              contentContainerStyle={{ paddingHorizontal: 24 }}
            >
              {/* Add New Meeting Card */}
              <Pressable
                onPress={() => router.push("/(tabs)/schedule")}
                className="w-12 h-28 bg-accent dark:bg-accent-dark rounded-lg items-center justify-center mr-3"
              >
                <Ionicons name="add" size={24} color="#006BFF" />
              </Pressable>
              {recentMeetings.map((meeting) => (
                <MeetingHistoryCard key={meeting.id} meeting={meeting} />
              ))}
            </ScrollView>
          </View>
        )}

        {recentMeetings.length === 0 && (
          <View className="mt-10 px-6">
            <Text className="text-lg font-bold text-foreground dark:text-foreground-dark mb-4">
              Meeting History
            </Text>
            <Card className="items-center py-8">
              <Ionicons name="calendar-outline" size={48} color="#006BFF" style={{ opacity: 0.5 }} />
              <Text className="mt-4 text-muted-foreground dark:text-muted-foreground-dark">
                No meetings yet
              </Text>
              <Pressable
                onPress={() => router.push("/(tabs)/schedule")}
                className="mt-4 flex-row items-center gap-2"
              >
                <Ionicons name="add-circle" size={20} color="#006BFF" />
                <Text className="font-semibold text-primary dark:text-primary-dark">
                  Schedule your first meeting
                </Text>
              </Pressable>
            </Card>
          </View>
        )}
      </ScrollView>
    </SafeAreaView>
  );
}

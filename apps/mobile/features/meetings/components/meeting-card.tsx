import { View, Pressable } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import * as Clipboard from "expo-clipboard";
import { Text } from "@/components/ui/text";
import { Card } from "@/components/ui/card";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import type { Meeting } from "../types";

interface MeetingCardProps {
  meeting: Meeting;
  onPress?: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
}

export function MeetingCard({ meeting, onPress, onEdit, onDelete }: MeetingCardProps) {
  const { success } = useToast();
  const colors = useColors();
  const isScheduled = meeting.status === "scheduled";
  const canJoin = meeting.status === "scheduled" || meeting.status === "live";

  const formatDate = (dateString: string) => {
    const date = new Date(dateString);
    return date.toLocaleDateString("en-US", {
      month: "short",
      day: "numeric",
    });
  };

  const formatTime = (dateString: string) => {
    const date = new Date(dateString);
    return date.toLocaleTimeString("en-US", {
      hour: "numeric",
      minute: "2-digit",
      hour12: true,
    });
  };

  const handleCopyCode = async () => {
    await Clipboard.setStringAsync(meeting.meeting_identifier);
    success("Meeting code copied!");
  };

  return (
    <Card className="mb-3 border border-border dark:border-border-dark">
      <View className="mb-2">
        <Text className="text-base font-bold text-foreground dark:text-foreground-dark" numberOfLines={1}>
          {meeting.title}
        </Text>
      </View>

      <View className="flex-row flex-wrap items-center gap-3 mb-3">
        <View className="flex-row items-center gap-1">
          <Ionicons name="calendar-outline" size={14} color={colors.mutedForeground} />
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
            {formatDate(meeting.scheduled_at)}
          </Text>
        </View>
        <View className="flex-row items-center gap-1">
          <Ionicons name="time-outline" size={14} color={colors.mutedForeground} />
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
            {formatTime(meeting.scheduled_at)}
          </Text>
        </View>
        <View className="flex-row items-center gap-1">
          <Ionicons name={meeting.is_private ? "lock-closed-outline" : "globe-outline"} size={14} color={colors.mutedForeground} />
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
            {meeting.is_private ? "Private" : "Public"}
          </Text>
        </View>
      </View>

      <Pressable
        onPress={handleCopyCode}
        className="flex-row items-center justify-between rounded-lg bg-muted dark:bg-muted-dark px-3 py-2 mb-3"
      >
        <Text className="text-sm font-mono text-foreground dark:text-foreground-dark">
          {meeting.meeting_identifier}
        </Text>
        <Ionicons name="copy-outline" size={16} color={colors.primary} />
      </Pressable>

      <View className="flex-row gap-2">
        {isScheduled && onEdit && (
          <Pressable
            onPress={onEdit}
            className="flex-1 flex-row items-center justify-center gap-1.5 rounded-lg bg-secondary dark:bg-secondary-dark py-2.5"
          >
            <Ionicons name="pencil-outline" size={16} color={colors.primary} />
            <Text className="text-sm font-bold text-primary dark:text-primary-dark">Edit</Text>
          </Pressable>
        )}
        {isScheduled && onDelete && (
          <Pressable
            onPress={onDelete}
            className="flex-1 flex-row items-center justify-center gap-1.5 rounded-lg bg-destructive/10 dark:bg-destructive-dark/10 py-2.5"
          >
            <Ionicons name="trash-outline" size={16} color={colors.destructive} />
            <Text className="text-sm font-bold text-destructive dark:text-destructive-dark">Delete</Text>
          </Pressable>
        )}
        {canJoin && (
          <Pressable
            onPress={onPress}
            className="flex-1 flex-row items-center justify-center gap-1.5 rounded-lg bg-primary dark:bg-primary-dark py-2.5"
          >
            <Ionicons name="enter-outline" size={16} color="#ffffff" />
            <Text className="text-sm font-bold text-primary-foreground dark:text-primary-foreground-dark">
              {meeting.status === "live" ? "Join Now" : "Join"}
            </Text>
          </Pressable>
        )}
      </View>
    </Card>
  );
}

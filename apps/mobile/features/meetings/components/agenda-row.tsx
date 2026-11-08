import { Pressable, View } from "react-native";
import { router } from "expo-router";
import { Text } from "@/components/ui/text";
import { Badge } from "@/components/ui/badge";
import { conferenceAction, describeTime, statusLabel } from "@/lib/meeting/agenda";
import { goToPreJoin } from "@/features/meetings/open-meeting";
import type { AgendaItem } from "@/lib/api/generated/meeting-service";

export function AgendaRow({ item, deviceTz }: { item: AgendaItem; deviceTz: string }) {
  const time = describeTime(item.scheduledStartAt, item.scheduledEndAt, item.meetingSpace.timezone, deviceTz);
  const action = conferenceAction(item.status, item.viewerRole);
  const title = item.meetingSpace.title || item.meetingSpace.meetingCode;
  const open = () => {
    if (item.conferenceId && action !== "none") {
      goToPreJoin(item.conferenceId, { title: item.meetingSpace.title ?? null, meetingCode: item.meetingSpace.meetingCode });
    } else {
      router.push({ pathname: "/(meetings)/[id]", params: { id: item.meetingSpaceId } });
    }
  };
  return (
    <Pressable onPress={open} className="flex-row gap-3 rounded-xl border border-border dark:border-border-dark p-4 mb-2">
      <View className="w-24">
        <Text className="font-semibold">{time.local}</Text>
        {time.spaceLocal && (
          <Text variant="muted" size="xs">
            {time.spaceLocal}
          </Text>
        )}
      </View>
      <View className="flex-1">
        <Text className="font-semibold" numberOfLines={1}>
          {title}
        </Text>
        <Text variant="muted" size="xs">
          {item.meetingSpace.meetingCode}
          {item.materialized ? "" : " · recurring"}
        </Text>
      </View>
      <Badge variant={item.status === "LIVE" ? "success" : item.status === "CANCELLED" ? "destructive" : "secondary"}>
        {statusLabel(item.status)}
      </Badge>
    </Pressable>
  );
}

import { View, Pressable } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import * as Clipboard from "expo-clipboard";
import { Text } from "@/components/ui/text";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import type { MeetingSpaceResponse, AccessPolicy, ConferenceStatus } from "../types";

interface SpaceCardProps {
  space: MeetingSpaceResponse;
  onPress?: () => void;
  onEdit?: () => void;
  onArchive?: () => void;
}

function getConferenceStatusInfo(status: ConferenceStatus): { label: string; variant: "success" | "default" | "destructive" | "secondary" | "outline" } {
  switch (status) {
    case "LIVE":
      return { label: "Live", variant: "success" };
    case "WAITING":
    case "PREPARING":
      return { label: "Starting", variant: "default" };
    case "CREATED":
      return { label: "Scheduled", variant: "outline" };
    case "ENDING":
      return { label: "Ending", variant: "default" };
    case "ENDED":
      return { label: "Ended", variant: "secondary" };
    case "CANCELLED":
      return { label: "Cancelled", variant: "destructive" };
    case "FAILED":
      return { label: "Failed", variant: "destructive" };
    default:
      return { label: status, variant: "secondary" };
  }
}

const ACCESS_POLICY_ICONS: Record<AccessPolicy, string> = {
  PUBLIC: "globe-outline",
  PUBLIC_PASSCODE: "key-outline",
  REGISTERED_USERS: "person-circle-outline",
  ORGANIZATION_ONLY: "business-outline",
  DOMAIN_RESTRICTED: "at-outline",
  EXTERNAL_ONLY: "people-outline",
  MEMBERS_ONLY: "people-circle-outline",
  INVITATION_ONLY: "mail-outline",
  CUSTOM: "settings-outline",
  HEALTHCARE_APPOINTMENT: "medical-outline",
};

const ACCESS_POLICY_LABELS: Record<AccessPolicy, string> = {
  PUBLIC: "Public",
  PUBLIC_PASSCODE: "Passcode",
  REGISTERED_USERS: "Registered",
  ORGANIZATION_ONLY: "Organization",
  DOMAIN_RESTRICTED: "Domain",
  EXTERNAL_ONLY: "External",
  MEMBERS_ONLY: "Members Only",
  INVITATION_ONLY: "Invite Only",
  CUSTOM: "Custom",
  HEALTHCARE_APPOINTMENT: "Healthcare",
};

export function SpaceCard({ space, onPress, onEdit, onArchive }: SpaceCardProps) {
  const { success } = useToast();
  const colors = useColors();
  const isActive = space.status === "ACTIVE";

  const formatDate = (dateString?: string) => {
    if (!dateString) return "";
    const date = new Date(dateString);
    return date.toLocaleDateString("en-US", {
      month: "short",
      day: "numeric",
    });
  };

  const handleCopyCode = async () => {
    await Clipboard.setStringAsync(space.meetingCode);
    success("Meeting code copied!");
  };

  const conferenceStatus = space.latestConference?.status;
  const statusInfo = conferenceStatus ? getConferenceStatusInfo(conferenceStatus) : null;

  return (
    <Card className="mb-3 border border-border dark:border-border-dark">
      <View className="flex-row items-start justify-between mb-2">
        <View className="flex-1 mr-2">
          <Text className="text-base font-bold text-foreground dark:text-foreground-dark" numberOfLines={1}>
            {space.title || "Untitled Meeting"}
          </Text>
        </View>
        {statusInfo && (
          <Badge variant={statusInfo.variant}>
            {statusInfo.label}
          </Badge>
        )}
      </View>

      <View className="flex-row flex-wrap items-center gap-3 mb-3">
        <View className="flex-row items-center gap-1">
          <Ionicons
            name={ACCESS_POLICY_ICONS[space.accessPolicy] as any}
            size={14}
            color={colors.mutedForeground}
          />
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
            {ACCESS_POLICY_LABELS[space.accessPolicy]}
          </Text>
        </View>
        {space.requiresPasscode && (
          <View className="flex-row items-center gap-1">
            <Ionicons name="lock-closed-outline" size={14} color={colors.mutedForeground} />
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              Passcode
            </Text>
          </View>
        )}
        {space.createdAt && (
          <View className="flex-row items-center gap-1">
            <Ionicons name="calendar-outline" size={14} color={colors.mutedForeground} />
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              {formatDate(space.createdAt)}
            </Text>
          </View>
        )}
      </View>

      <Pressable
        onPress={handleCopyCode}
        className="flex-row items-center justify-between rounded-lg bg-muted dark:bg-muted-dark px-3 py-2 mb-3"
      >
        <Text className="text-sm font-mono text-foreground dark:text-foreground-dark">
          {space.meetingCode}
        </Text>
        <Ionicons name="copy-outline" size={16} color={colors.primary} />
      </Pressable>

      <View className="flex-row gap-2">
        {isActive && onEdit && (
          <Pressable
            onPress={onEdit}
            className="flex-1 flex-row items-center justify-center gap-1.5 rounded-lg bg-secondary dark:bg-secondary-dark py-2.5"
          >
            <Ionicons name="pencil-outline" size={16} color={colors.primary} />
            <Text className="text-sm font-bold text-primary dark:text-primary-dark">Edit</Text>
          </Pressable>
        )}
        {isActive && onArchive && (
          <Pressable
            onPress={onArchive}
            className="flex-1 flex-row items-center justify-center gap-1.5 rounded-lg bg-destructive/10 dark:bg-destructive-dark/10 py-2.5"
          >
            <Ionicons name="archive-outline" size={16} color={colors.destructive} />
            <Text className="text-sm font-bold text-destructive dark:text-destructive-dark">Archive</Text>
          </Pressable>
        )}
        {isActive && (
          <Pressable
            onPress={onPress}
            className="flex-1 flex-row items-center justify-center gap-1.5 rounded-lg bg-primary dark:bg-primary-dark py-2.5"
          >
            <Ionicons name="enter-outline" size={16} color="#ffffff" />
            <Text className="text-sm font-bold text-primary-foreground dark:text-primary-foreground-dark">
              Join
            </Text>
          </Pressable>
        )}
      </View>
    </Card>
  );
}

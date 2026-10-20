import { useState } from "react";
import { View, Pressable, Platform, ScrollView } from "react-native";
import DateTimePicker, { DateTimePickerEvent } from "@react-native-community/datetimepicker";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import { useCreateSpace } from "../hooks";
import { conferencesApi } from "../api";
import { getErrorMessage } from "@/lib/utils/error";
import type { AccessPolicy, WaitingRoomPolicy, MeetingType } from "../types";

interface ScheduleFormProps {
  onSuccess?: () => void;
}

const MEETING_TYPES: { label: string; value: MeetingType; icon: string; description: string }[] = [
  { label: "Standard", value: "STANDARD", icon: "videocam-outline", description: "Regular meetings for teams" },
  { label: "Webinar", value: "WEBINAR", icon: "easel-outline", description: "One-to-many broadcasts" },
  { label: "Town Hall", value: "TOWN_HALL", icon: "megaphone-outline", description: "Large company-wide sessions" },
  { label: "Training", value: "TRAINING", icon: "school-outline", description: "Educational workshops" },
];

const ACCESS_POLICIES: { label: string; value: AccessPolicy; icon: string; description: string }[] = [
  { label: "Public", value: "PUBLIC", icon: "globe-outline", description: "Anyone with the link can join" },
  { label: "Passcode", value: "PUBLIC_PASSCODE", icon: "key-outline", description: "Requires a passcode to join" },
  { label: "Members Only", value: "MEMBERS_ONLY", icon: "people-circle-outline", description: "Only invited members" },
];

const WAITING_ROOM_POLICIES: { label: string; value: WaitingRoomPolicy; description: string }[] = [
  { label: "Off", value: "OFF", description: "No waiting room" },
  { label: "Guests Only", value: "GUESTS_ONLY", description: "Only guests wait" },
  { label: "Everyone", value: "EVERYONE", description: "All participants wait" },
];

const DURATION_OPTIONS = [
  { label: "15 min", value: 15 },
  { label: "30 min", value: 30 },
  { label: "45 min", value: 45 },
  { label: "1 hour", value: 60 },
  { label: "1.5 hours", value: 90 },
  { label: "2 hours", value: 120 },
];

export function ScheduleFormV2({ onSuccess }: ScheduleFormProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [meetingType, setMeetingType] = useState<MeetingType>("STANDARD");
  const [accessPolicy, setAccessPolicy] = useState<AccessPolicy>("PUBLIC");
  const [waitingRoom, setWaitingRoom] = useState<WaitingRoomPolicy>("OFF");
  const [passcode, setPasscode] = useState("");
  const [durationMinutes, setDurationMinutes] = useState(60);
  const [selectedDate, setSelectedDate] = useState(new Date());
  const [showDatePicker, setShowDatePicker] = useState(false);
  const [showTimePicker, setShowTimePicker] = useState(false);
  const [isCreatingConference, setIsCreatingConference] = useState(false);

  const { toast, success } = useToast();
  const colors = useColors();
  const createSpace = useCreateSpace();

  const handleDateChange = (_event: DateTimePickerEvent, date?: Date) => {
    setShowDatePicker(false);
    if (date) {
      const newDate = new Date(selectedDate);
      newDate.setFullYear(date.getFullYear(), date.getMonth(), date.getDate());
      setSelectedDate(newDate);
    }
  };

  const handleTimeChange = (_event: DateTimePickerEvent, date?: Date) => {
    setShowTimePicker(false);
    if (date) {
      const newDate = new Date(selectedDate);
      newDate.setHours(date.getHours(), date.getMinutes());
      setSelectedDate(newDate);
    }
  };

  const formatDate = (date: Date) => {
    return date.toLocaleDateString("en-US", {
      weekday: "short",
      month: "short",
      day: "numeric",
      year: "numeric",
    });
  };

  const formatTime = (date: Date) => {
    return date.toLocaleTimeString("en-US", {
      hour: "numeric",
      minute: "2-digit",
      hour12: true,
    });
  };

  const handleSubmit = async () => {
    if (!title.trim()) {
      toast("Please enter a meeting title", "destructive");
      return;
    }

    if (accessPolicy === "PUBLIC_PASSCODE") {
      const trimmedPasscode = passcode.trim();
      if (!trimmedPasscode) {
        toast("Please enter a passcode", "destructive");
        return;
      }
      if (trimmedPasscode.length < 4 || trimmedPasscode.length > 64) {
        toast("Passcode must be between 4 and 64 characters", "destructive");
        return;
      }
    }

    try {
      // Create meeting space
      const space = await createSpace.mutateAsync({
        title: title.trim(),
        description: description.trim() || undefined,
        meetingType,
        accessPolicy,
        waitingRoomPolicy: waitingRoom,
        passcode: accessPolicy === "PUBLIC_PASSCODE" ? passcode.trim() : undefined,
      });

      // Create scheduled conference
      setIsCreatingConference(true);
      const scheduledStartAt = selectedDate.toISOString();
      const scheduledEndAt = new Date(selectedDate.getTime() + durationMinutes * 60 * 1000).toISOString();

      await conferencesApi.create(space.id, {
        scheduledStartAt,
        scheduledEndAt,
      });

      success("Meeting scheduled successfully!");
      onSuccess?.();
    } catch (error) {
      toast(getErrorMessage(error, "Failed to schedule meeting"), "destructive");
    } finally {
      setIsCreatingConference(false);
    }
  };

  const isLoading = createSpace.isPending || isCreatingConference;

  return (
    <View className="gap-4">
      <Input
        label="Meeting Title"
        placeholder="Enter meeting title"
        value={title}
        onChangeText={setTitle}
      />

      <Input
        label="Description (Optional)"
        placeholder="Add a description"
        value={description}
        onChangeText={setDescription}
        multiline
        numberOfLines={3}
      />

      {/* Meeting Type */}
      <View>
        <Text className="mb-2 text-sm font-medium text-foreground dark:text-foreground-dark">
          Meeting Type
        </Text>
        <ScrollView horizontal showsHorizontalScrollIndicator={false}>
          <View className="flex-row gap-2">
            {MEETING_TYPES.map((type) => (
              <Pressable
                key={type.value}
                onPress={() => setMeetingType(type.value)}
                className={`flex-row items-center gap-2 px-4 py-2.5 rounded-lg border ${
                  meetingType === type.value
                    ? "border-primary dark:border-primary-dark bg-primary/10 dark:bg-primary-dark/10"
                    : "border-input dark:border-input-dark"
                }`}
              >
                <Ionicons
                  name={type.icon as any}
                  size={18}
                  color={meetingType === type.value ? colors.primary : colors.mutedForeground}
                />
                <Text
                  className={`text-sm font-medium ${
                    meetingType === type.value
                      ? "text-primary dark:text-primary-dark"
                      : "text-foreground dark:text-foreground-dark"
                  }`}
                >
                  {type.label}
                </Text>
              </Pressable>
            ))}
          </View>
        </ScrollView>
      </View>

      {/* Access Policy */}
      <View>
        <Text className="mb-2 text-sm font-medium text-foreground dark:text-foreground-dark">
          Access Policy
        </Text>
        <View className="gap-2">
          {ACCESS_POLICIES.map((policy) => (
            <Pressable
              key={policy.value}
              onPress={() => setAccessPolicy(policy.value)}
              className={`flex-row items-center gap-3 p-3 rounded-lg border ${
                accessPolicy === policy.value
                  ? "border-primary dark:border-primary-dark bg-primary/5 dark:bg-primary-dark/5"
                  : "border-input dark:border-input-dark"
              }`}
            >
              <View
                className={`w-10 h-10 rounded-lg items-center justify-center ${
                  accessPolicy === policy.value
                    ? "bg-primary/10 dark:bg-primary-dark/10"
                    : "bg-muted dark:bg-muted-dark"
                }`}
              >
                <Ionicons
                  name={policy.icon as any}
                  size={20}
                  color={accessPolicy === policy.value ? colors.primary : colors.mutedForeground}
                />
              </View>
              <View className="flex-1">
                <Text
                  className={`text-sm font-medium ${
                    accessPolicy === policy.value
                      ? "text-primary dark:text-primary-dark"
                      : "text-foreground dark:text-foreground-dark"
                  }`}
                >
                  {policy.label}
                </Text>
                <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark">
                  {policy.description}
                </Text>
              </View>
              {accessPolicy === policy.value && (
                <Ionicons name="checkmark-circle" size={22} color={colors.primary} />
              )}
            </Pressable>
          ))}
        </View>
      </View>

      {/* Passcode Input (if passcode policy) */}
      {accessPolicy === "PUBLIC_PASSCODE" && (
        <View>
          <Input
            label="Meeting Passcode"
            placeholder="Enter passcode (4-64 characters)"
            value={passcode}
            onChangeText={setPasscode}
            maxLength={64}
          />
          <Text className="mt-1 text-xs text-muted-foreground dark:text-muted-foreground-dark">
            Passcode must be 4-64 characters
          </Text>
        </View>
      )}

      {/* Waiting Room */}
      <View>
        <Text className="mb-2 text-sm font-medium text-foreground dark:text-foreground-dark">
          Waiting Room
        </Text>
        <View className="flex-row flex-wrap gap-2">
          {WAITING_ROOM_POLICIES.map((policy) => (
            <Pressable
              key={policy.value}
              onPress={() => setWaitingRoom(policy.value)}
              className={`px-4 py-2 rounded-lg border ${
                waitingRoom === policy.value
                  ? "border-primary dark:border-primary-dark bg-primary dark:bg-primary-dark"
                  : "border-input dark:border-input-dark"
              }`}
            >
              <Text
                className={`text-sm font-medium ${
                  waitingRoom === policy.value
                    ? "text-primary-foreground dark:text-primary-foreground-dark"
                    : "text-foreground dark:text-foreground-dark"
                }`}
              >
                {policy.label}
              </Text>
            </Pressable>
          ))}
        </View>
      </View>

      {/* Date & Time */}
      <View>
        <Text className="mb-2 text-sm font-medium text-foreground dark:text-foreground-dark">
          Date & Time
        </Text>
        <View className="flex-row gap-2">
          <Pressable
            onPress={() => setShowDatePicker(true)}
            className="flex-1 flex-row items-center gap-2 rounded-lg border border-input dark:border-input-dark bg-background dark:bg-card-dark px-3 py-3"
          >
            <Ionicons name="calendar-outline" size={18} color={colors.mutedForeground} />
            <Text className="text-foreground dark:text-foreground-dark flex-1">
              {formatDate(selectedDate)}
            </Text>
          </Pressable>
          <Pressable
            onPress={() => setShowTimePicker(true)}
            className="flex-1 flex-row items-center gap-2 rounded-lg border border-input dark:border-input-dark bg-background dark:bg-card-dark px-3 py-3"
          >
            <Ionicons name="time-outline" size={18} color={colors.mutedForeground} />
            <Text className="text-foreground dark:text-foreground-dark flex-1">
              {formatTime(selectedDate)}
            </Text>
          </Pressable>
        </View>
      </View>

      {(showDatePicker || showTimePicker) && (
        <DateTimePicker
          value={selectedDate}
          mode={showDatePicker ? "date" : "time"}
          display={Platform.OS === "ios" ? "spinner" : "default"}
          onChange={showDatePicker ? handleDateChange : handleTimeChange}
          minimumDate={new Date()}
        />
      )}

      {/* Duration */}
      <View>
        <Text className="mb-2 text-sm font-medium text-foreground dark:text-foreground-dark">
          Duration
        </Text>
        <View className="flex-row flex-wrap gap-2">
          {DURATION_OPTIONS.map((option) => (
            <Pressable
              key={option.value}
              onPress={() => setDurationMinutes(option.value)}
              className={`rounded-lg border px-3 py-2 ${
                durationMinutes === option.value
                  ? "border-primary dark:border-primary-dark bg-primary dark:bg-primary-dark"
                  : "border-input dark:border-input-dark bg-background dark:bg-card-dark"
              }`}
            >
              <Text
                className={`text-sm font-medium ${
                  durationMinutes === option.value
                    ? "text-primary-foreground dark:text-primary-foreground-dark"
                    : "text-foreground dark:text-foreground-dark"
                }`}
              >
                {option.label}
              </Text>
            </Pressable>
          ))}
        </View>
      </View>

      <Button
        onPress={handleSubmit}
        isLoading={isLoading}
        className="mt-4"
      >
        Schedule Meeting
      </Button>
    </View>
  );
}

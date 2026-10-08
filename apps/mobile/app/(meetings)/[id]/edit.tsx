import { useState, useEffect } from "react";
import { View, ScrollView, Pressable, Platform } from "react-native";
import { useLocalSearchParams, router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import DateTimePicker, { DateTimePickerEvent } from "@react-native-community/datetimepicker";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useMeeting, useUpdateMeeting } from "@/features/meetings/hooks/use-meetings";

const DURATION_OPTIONS = [
  { label: "15 min", value: 15 },
  { label: "30 min", value: 30 },
  { label: "45 min", value: 45 },
  { label: "1 hour", value: 60 },
  { label: "1.5 hours", value: 90 },
  { label: "2 hours", value: 120 },
  { label: "3 hours", value: 180 },
];

function findClosestDuration(minutes: number): number {
  const option = DURATION_OPTIONS.find(opt => opt.value === minutes);
  if (option) return option.value;
  // Find closest duration option
  return DURATION_OPTIONS.reduce((prev, curr) =>
    Math.abs(curr.value - minutes) < Math.abs(prev.value - minutes) ? curr : prev
  ).value;
}

export default function EditMeetingScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const { data: meeting, isLoading, isError } = useMeeting(id);
  const updateMeeting = useUpdateMeeting();
  const { toast, success } = useToast();

  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [selectedDate, setSelectedDate] = useState(new Date());
  const [duration, setDuration] = useState(60);
  const [isPrivate, setIsPrivate] = useState(false);
  const [showDatePicker, setShowDatePicker] = useState(false);
  const [showTimePicker, setShowTimePicker] = useState(false);
  const [isInitialized, setIsInitialized] = useState(false);

  useEffect(() => {
    if (meeting && !isInitialized) {
      setTitle(meeting.title || "");
      setDescription(meeting.description || "");
      setSelectedDate(new Date(meeting.scheduled_at));
      setDuration(findClosestDuration(meeting.duration_minutes));
      setIsPrivate(meeting.is_private);
      setIsInitialized(true);
    }
  }, [meeting, isInitialized]);

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

  const handleSubmit = () => {
    if (!title.trim()) {
      toast("Title is required", "destructive");
      return;
    }

    updateMeeting.mutate(
      {
        id,
        data: {
          title: title.trim(),
          description: description.trim() || undefined,
          scheduled_at: selectedDate.toISOString(),
          duration_minutes: duration,
          is_private: isPrivate,
        },
      },
      {
        onSuccess: () => {
          success("Meeting updated");
          router.back();
        },
        onError: (error) => {
          toast(error.message, "destructive");
        },
      }
    );
  };

  if (isLoading) {
    return <Loading fullScreen message="Loading meeting..." />;
  }

  if (isError || !meeting) {
    return (
      <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark items-center justify-center px-6">
        <Ionicons name="alert-circle-outline" size={48} color="#dc2626" style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
          Failed to load meeting
        </Text>
        <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
          The meeting may not exist or you don't have access.
        </Text>
        <Button onPress={() => router.back()} className="mt-4" variant="outline">
          Go Back
        </Button>
      </SafeAreaView>
    );
  }

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      {/* Header */}
      <View className="flex-row items-center px-4 py-3 border-b border-border dark:border-border-dark">
        <Pressable onPress={() => router.back()} className="flex-row items-center gap-2">
          <Ionicons name="arrow-back" size={24} color="#029CD4" />
          <Text className="text-sm font-bold text-primary dark:text-primary-dark">Back</Text>
        </Pressable>
      </View>

      <ScrollView
        className="flex-1"
        contentContainerStyle={{ padding: 24, paddingBottom: 100 }}
        showsVerticalScrollIndicator={false}
        keyboardShouldPersistTaps="handled"
      >
        <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark mb-1">
          Edit Meeting
        </Text>
        <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark mb-6">
          Update your meeting details
        </Text>

        <View className="gap-4">
          {/* Title */}
          <Input
            label="Meeting Title"
            placeholder="e.g. Weekly Team Standup"
            value={title}
            onChangeText={setTitle}
          />

          {/* Description */}
          <Input
            label="Description (Optional)"
            placeholder="Add a description"
            value={description}
            onChangeText={setDescription}
            multiline
            numberOfLines={3}
          />

          {/* Date & Time */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Date & Time
            </Text>
            <View className="flex-row gap-2">
              <Pressable
                onPress={() => setShowDatePicker(true)}
                className="flex-1 rounded-lg border border-input dark:border-input-dark bg-background dark:bg-card-dark px-3 py-3"
              >
                <Text className="text-foreground dark:text-foreground-dark">
                  {formatDate(selectedDate)}
                </Text>
              </Pressable>
              <Pressable
                onPress={() => setShowTimePicker(true)}
                className="flex-1 rounded-lg border border-input dark:border-input-dark bg-background dark:bg-card-dark px-3 py-3"
              >
                <Text className="text-foreground dark:text-foreground-dark">
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
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Duration
            </Text>
            <View className="flex-row flex-wrap gap-2">
              {DURATION_OPTIONS.map((option) => (
                <Pressable
                  key={option.value}
                  onPress={() => setDuration(option.value)}
                  className={`rounded-lg border px-3 py-2 ${
                    duration === option.value
                      ? "border-primary dark:border-primary-dark bg-primary dark:bg-primary-dark"
                      : "border-input dark:border-input-dark bg-background dark:bg-card-dark"
                  }`}
                >
                  <Text
                    className={`text-sm font-medium ${
                      duration === option.value
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

          {/* Privacy Toggle */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Meeting Type
            </Text>
            <View className="flex-row gap-2">
              <Pressable
                onPress={() => setIsPrivate(true)}
                className={`flex-1 flex-row items-center justify-center gap-2 rounded-lg px-4 py-3 ${
                  isPrivate
                    ? "bg-primary dark:bg-primary-dark"
                    : "bg-secondary dark:bg-secondary-dark"
                }`}
              >
                <Ionicons name="lock-closed" size={18} color={isPrivate ? "#ffffff" : "#737373"} />
                <Text
                  className={`font-bold ${
                    isPrivate
                      ? "text-primary-foreground dark:text-primary-foreground-dark"
                      : "text-muted-foreground dark:text-muted-foreground-dark"
                  }`}
                >
                  Private
                </Text>
              </Pressable>
              <Pressable
                onPress={() => setIsPrivate(false)}
                className={`flex-1 flex-row items-center justify-center gap-2 rounded-lg px-4 py-3 ${
                  !isPrivate
                    ? "bg-primary dark:bg-primary-dark"
                    : "bg-secondary dark:bg-secondary-dark"
                }`}
              >
                <Ionicons name="globe" size={18} color={!isPrivate ? "#ffffff" : "#737373"} />
                <Text
                  className={`font-bold ${
                    !isPrivate
                      ? "text-primary-foreground dark:text-primary-foreground-dark"
                      : "text-muted-foreground dark:text-muted-foreground-dark"
                  }`}
                >
                  Public
                </Text>
              </Pressable>
            </View>
            <Text className="mt-2 text-xs text-muted-foreground dark:text-muted-foreground-dark">
              {isPrivate
                ? "Participants need approval to join"
                : "Anyone with the code can join instantly"}
            </Text>
          </View>

          {/* Actions */}
          <View className="flex-row gap-3 mt-4">
            <Button
              onPress={handleSubmit}
              isLoading={updateMeeting.isPending}
              className="flex-1"
            >
              Save Changes
            </Button>
            <Button variant="outline" onPress={() => router.back()}>
              Cancel
            </Button>
          </View>
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}

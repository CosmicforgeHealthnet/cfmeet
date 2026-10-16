import { useState } from "react";
import { View, Pressable, Platform } from "react-native";
import DateTimePicker, { DateTimePickerEvent } from "@react-native-community/datetimepicker";
import { useForm, Controller } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { useCreateMeeting } from "../hooks/use-meetings";
import { createMeetingSchema, type CreateMeetingFormData } from "../schemas/meeting.schema";

interface ScheduleFormProps {
  onSuccess?: () => void;
}

const DURATION_OPTIONS = [
  { label: "15 min", value: 15 },
  { label: "30 min", value: 30 },
  { label: "45 min", value: 45 },
  { label: "1 hour", value: 60 },
  { label: "1.5 hours", value: 90 },
  { label: "2 hours", value: 120 },
  { label: "3 hours", value: 180 },
];

export function ScheduleForm({ onSuccess }: ScheduleFormProps) {
  const [showDatePicker, setShowDatePicker] = useState(false);
  const [showTimePicker, setShowTimePicker] = useState(false);
  const [selectedDate, setSelectedDate] = useState(new Date());
  const { toast, success } = useToast();

  const createMeeting = useCreateMeeting();

  const {
    control,
    handleSubmit,
    setValue,
    watch,
    formState: { errors },
  } = useForm<CreateMeetingFormData>({
    resolver: zodResolver(createMeetingSchema),
    defaultValues: {
      title: "",
      description: "",
      scheduled_at: "",
      duration_minutes: 60,
      is_private: false,
    },
  });

  const isPrivate = watch("is_private");
  const durationMinutes = watch("duration_minutes");

  const handleDateChange = (_event: DateTimePickerEvent, date?: Date) => {
    setShowDatePicker(false);
    if (date) {
      setSelectedDate(date);
      setValue("scheduled_at", date.toISOString());
    }
  };

  const handleTimeChange = (_event: DateTimePickerEvent, date?: Date) => {
    setShowTimePicker(false);
    if (date) {
      const newDate = new Date(selectedDate);
      newDate.setHours(date.getHours(), date.getMinutes());
      setSelectedDate(newDate);
      setValue("scheduled_at", newDate.toISOString());
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

  const onSubmit = (data: CreateMeetingFormData) => {
    createMeeting.mutate(data, {
      onSuccess: () => {
        success("Meeting scheduled successfully!");
        onSuccess?.();
      },
      onError: (error) => {
        toast(error.message, "destructive");
      },
    });
  };

  return (
    <View className="gap-4">
      <Controller
        control={control}
        name="title"
        render={({ field: { onChange, onBlur, value } }) => (
          <Input
            label="Meeting Title"
            placeholder="Enter meeting title"
            value={value}
            onChangeText={onChange}
            onBlur={onBlur}
            error={errors.title?.message}
          />
        )}
      />

      <Controller
        control={control}
        name="description"
        render={({ field: { onChange, onBlur, value } }) => (
          <Input
            label="Description (Optional)"
            placeholder="Add a description"
            value={value}
            onChangeText={onChange}
            onBlur={onBlur}
            multiline
            numberOfLines={3}
            error={errors.description?.message}
          />
        )}
      />

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
              {watch("scheduled_at") ? formatDate(selectedDate) : "Select date"}
            </Text>
          </Pressable>
          <Pressable
            onPress={() => setShowTimePicker(true)}
            className="flex-1 rounded-lg border border-input dark:border-input-dark bg-background dark:bg-card-dark px-3 py-3"
          >
            <Text className="text-foreground dark:text-foreground-dark">
              {watch("scheduled_at") ? formatTime(selectedDate) : "Select time"}
            </Text>
          </Pressable>
        </View>
        {errors.scheduled_at && (
          <Text className="mt-1 text-sm text-destructive dark:text-destructive-dark">
            {errors.scheduled_at.message}
          </Text>
        )}
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

      <View>
        <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
          Duration
        </Text>
        <View className="flex-row flex-wrap gap-2">
          {DURATION_OPTIONS.map((option) => (
            <Pressable
              key={option.value}
              onPress={() => setValue("duration_minutes", option.value)}
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

      <View>
        <Pressable
          onPress={() => setValue("is_private", !isPrivate)}
          className="flex-row items-center justify-between rounded-lg border border-input dark:border-input-dark bg-background dark:bg-card-dark p-4"
        >
          <View className="flex-1 mr-4">
            <Text className="font-medium text-foreground dark:text-foreground-dark">Private Meeting</Text>
            <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark">
              {isPrivate
                ? "Participants need approval to join"
                : "Anyone with the code can join"}
            </Text>
          </View>
          <View
            className={`h-6 w-11 rounded-full p-0.5 ${
              isPrivate ? "bg-primary dark:bg-primary-dark" : "bg-muted dark:bg-muted-dark"
            }`}
          >
            <View
              className={`h-5 w-5 rounded-full bg-background dark:bg-background-dark transition-all ${
                isPrivate ? "ml-5" : "ml-0"
              }`}
            />
          </View>
        </Pressable>
      </View>

      <Button
        onPress={handleSubmit(onSubmit)}
        isLoading={createMeeting.isPending}
        className="mt-4"
      >
        Schedule Meeting
      </Button>
    </View>
  );
}

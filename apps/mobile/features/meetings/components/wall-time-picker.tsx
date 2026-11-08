/**
 * Date + time picker for a wall-clock time in the meeting's time zone (not the phone's).
 * The native picker works in device time; only its calendar fields are used, and the caller
 * converts them with `zonedToUtc(wall, meetingTimeZone)`.
 */
import { useState } from "react";
import { Platform, Pressable, View } from "react-native";
import DateTimePicker, { type DateTimePickerEvent } from "@react-native-community/datetimepicker";
import { Text } from "@/components/ui/text";
import type { WallTime } from "@/lib/time/zoned";

function pad(n: number) {
  return String(n).padStart(2, "0");
}

export function wallToDeviceDate(w: WallTime): Date {
  return new Date(w.year, w.month - 1, w.day, w.hour, w.minute);
}

export function deviceDateToWall(d: Date): WallTime {
  return { year: d.getFullYear(), month: d.getMonth() + 1, day: d.getDate(), hour: d.getHours(), minute: d.getMinutes() };
}

export function WallTimePicker({ label, value, onChange }: { label: string; value: WallTime; onChange: (w: WallTime) => void }) {
  const [mode, setMode] = useState<"date" | "time" | null>(null);
  const onPicked = (event: DateTimePickerEvent, date?: Date) => {
    if (Platform.OS !== "ios") setMode(null);
    if (event.type === "set" && date) onChange(deviceDateToWall(date));
  };
  return (
    <View>
      <Text className="mb-1.5 text-sm font-medium">{label}</Text>
      <View className="flex-row gap-3">
        <Pressable onPress={() => setMode("date")} className="flex-1 rounded-lg border border-border dark:border-border-dark px-3 py-3">
          <Text>{`${value.year}-${pad(value.month)}-${pad(value.day)}`}</Text>
        </Pressable>
        <Pressable onPress={() => setMode("time")} className="w-28 rounded-lg border border-border dark:border-border-dark px-3 py-3">
          <Text>{`${pad(value.hour)}:${pad(value.minute)}`}</Text>
        </Pressable>
      </View>
      {mode && (
        <DateTimePicker
          value={wallToDeviceDate(value)}
          mode={mode}
          is24Hour
          display={Platform.OS === "ios" ? "spinner" : "default"}
          onChange={onPicked}
        />
      )}
      {mode && Platform.OS === "ios" && (
        <Pressable onPress={() => setMode(null)} className="self-end py-2">
          <Text className="font-semibold text-primary">Done</Text>
        </Pressable>
      )}
    </View>
  );
}

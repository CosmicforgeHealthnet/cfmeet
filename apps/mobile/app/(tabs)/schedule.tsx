/**
 * Schedule a meeting (MTG-001): `POST /api/v2/meeting-spaces` then
 * `POST /api/v2/meeting-spaces/{id}/conferences`, in the selected workspace (Personal or an
 * organization). Date and time are a wall-clock time in the meeting's time zone.
 */
import { useMemo, useState } from "react";
import { Pressable, ScrollView, Switch, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router } from "expo-router";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Notice } from "@/components/shared/screen-header";
import { WorkspaceSwitcher, useWorkspace } from "@/features/workspace/workspace";
import { WallTimePicker } from "@/features/meetings/components/wall-time-picker";
import { useScheduleMeeting } from "@/features/meetings/hooks";
import { decodeApiError } from "@/lib/api/errors";
import { deviceTimeZone, formatInZone, isValidTimeZone, scheduleProblem, wallTimeAt, zonedToUtc, type WallTime } from "@/lib/time/zoned";
import type { AccessPolicy } from "@/lib/api/generated/meeting-service";

const DURATIONS = [15, 30, 45, 60, 90, 120];

function nextHalfHour(tz: string): WallTime {
  const now = Date.now();
  const rounded = Math.ceil(now / 1_800_000) * 1_800_000;
  return wallTimeAt(rounded, tz);
}

export default function ScheduleScreen() {
  const workspace = useWorkspace();
  const schedule = useScheduleMeeting();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [timeZone, setTimeZone] = useState(deviceTimeZone());
  const [start, setStart] = useState<WallTime>(() => nextHalfHour(deviceTimeZone()));
  const [duration, setDuration] = useState(30);
  const [access, setAccess] = useState<AccessPolicy>("PUBLIC");
  const [passcode, setPasscode] = useState("");
  const [waitingRoom, setWaitingRoom] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const tzValid = isValidTimeZone(timeZone.trim());
  const startMs = useMemo(() => (tzValid ? zonedToUtc(start, timeZone.trim()) : NaN), [start, timeZone, tzValid]);
  const endMs = startMs + duration * 60_000;
  const deviceTz = deviceTimeZone();

  const accessOptions: { value: AccessPolicy; label: string }[] = [
    { value: "PUBLIC", label: "Anyone with the link" },
    { value: "AUTHENTICATED", label: "Signed-in CF Meet users" },
    ...(workspace.organizationId ? [{ value: "ORGANIZATION_ONLY" as AccessPolicy, label: "Organization members" }] : []),
  ];

  const submit = () => {
    setError(null);
    if (!title.trim()) return setError("Add a title.");
    if (!tzValid) return setError("Enter a valid time zone, e.g. Europe/London.");
    const problem = scheduleProblem(startMs, endMs, Date.now());
    if (problem) return setError(problem);
    const policy: AccessPolicy = access === "PUBLIC" && passcode.trim() ? "PUBLIC_PASSCODE" : access;
    schedule.mutate(
      {
        space: {
          title: title.trim(),
          description: description.trim() || null,
          timezone: timeZone.trim(),
          accessPolicy: policy,
          passcode: passcode.trim() || null,
          waitingRoomPolicy: waitingRoom ? "EVERYONE" : "OFF",
          organizationId: workspace.organizationId ?? null,
        },
        startMs,
        endMs,
      },
      {
        onSuccess: ({ space }) => {
          setTitle("");
          setDescription("");
          setPasscode("");
          router.push({ pathname: "/(meetings)/[id]", params: { id: space.id } });
        },
        onError: (e) => setError(decodeApiError(e).message),
      }
    );
  };

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <View className="flex-row items-center justify-between border-b border-border dark:border-border-dark px-6 py-4">
        <Text className="text-2xl font-bold">Schedule</Text>
        <WorkspaceSwitcher />
      </View>
      <ScrollView contentContainerStyle={{ padding: 24, paddingBottom: 120, gap: 16 }}>
        {workspace.suspended && (
          <Notice tone="warning" title="Organization suspended" body="This organization cannot schedule meetings right now." />
        )}
        <Input label="Title" value={title} onChangeText={setTitle} maxLength={200} placeholder="Weekly sync" />
        <Input label="Description (optional)" value={description} onChangeText={setDescription} multiline maxLength={2000} />
        <Input
          label="Time zone"
          value={timeZone}
          onChangeText={setTimeZone}
          autoCapitalize="none"
          autoCorrect={false}
          error={tzValid ? undefined : "Unknown time zone"}
        />
        <WallTimePicker label={`Starts (${tzValid ? timeZone.trim() : "meeting time"})`} value={start} onChange={setStart} />
        {tzValid && Number.isFinite(startMs) && timeZone.trim() !== deviceTz && (
          <Text variant="muted" size="sm">
            On this phone: {formatInZone(startMs, deviceTz)}
          </Text>
        )}
        <View>
          <Text className="mb-1.5 text-sm font-medium">Duration</Text>
          <View className="flex-row flex-wrap gap-2">
            {DURATIONS.map((d) => (
              <Pressable
                key={d}
                onPress={() => setDuration(d)}
                className={`rounded-full px-4 py-2 border ${d === duration ? "bg-primary border-primary" : "border-border dark:border-border-dark"}`}
              >
                <Text className={d === duration ? "text-white font-semibold" : ""}>{d < 60 ? `${d} min` : `${d / 60} h`}</Text>
              </Pressable>
            ))}
          </View>
        </View>
        <Card>
          <Text className="font-semibold mb-2">Who can join</Text>
          {accessOptions.map((o) => (
            <Pressable key={o.value} onPress={() => setAccess(o.value)} className="flex-row items-center justify-between py-2">
              <Text>{o.label}</Text>
              <View className={`h-5 w-5 rounded-full border-2 ${access === o.value ? "border-primary bg-primary" : "border-border"}`} />
            </Pressable>
          ))}
          <View className="flex-row items-center justify-between py-2">
            <Text>Waiting room</Text>
            <Switch value={waitingRoom} onValueChange={setWaitingRoom} />
          </View>
        </Card>
        <Input label="Passcode (optional)" value={passcode} onChangeText={setPasscode} secureTextEntry autoCapitalize="none" />
        {error && (
          <Text variant="destructive" size="sm">
            {error}
          </Text>
        )}
        <Button size="lg" isLoading={schedule.isPending} disabled={workspace.suspended} onPress={submit}>
          Schedule meeting
        </Button>
      </ScrollView>
    </SafeAreaView>
  );
}

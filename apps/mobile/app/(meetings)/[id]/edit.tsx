/** Edit a meeting room: `PATCH /api/v2/meeting-spaces/{id}` with optimistic-concurrency `version`. */
import { useEffect, useState } from "react";
import { ScrollView, Switch, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router, useLocalSearchParams } from "expo-router";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { ScreenHeader } from "@/components/shared/screen-header";
import { useSpace, useUpdateSpace } from "@/features/meetings/hooks";
import { decodeApiError } from "@/lib/api/errors";
import { isValidTimeZone } from "@/lib/time/zoned";

export default function EditSpaceScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const space = useSpace(id);
  const update = useUpdateSpace(id);
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [timeZone, setTimeZone] = useState("");
  const [passcode, setPasscode] = useState("");
  const [clearPasscode, setClearPasscode] = useState(false);
  const [waitingRoom, setWaitingRoom] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (space.data) {
      setTitle(space.data.title ?? "");
      setDescription(space.data.description ?? "");
      setTimeZone(space.data.timezone ?? "");
      setWaitingRoom((space.data.waitingRoomPolicy ?? "OFF") !== "OFF");
    }
  }, [space.data]);

  if (space.isLoading || !space.data) return <Loading fullScreen />;
  const s = space.data;

  const save = () => {
    setError(null);
    if (!title.trim()) return setError("Add a title.");
    if (timeZone.trim() && !isValidTimeZone(timeZone.trim())) return setError("Unknown time zone.");
    if (s.version === null || s.version === undefined) return setError("This meeting room cannot be edited from mobile.");
    update.mutate(
      {
        version: s.version,
        title: title.trim(),
        description: description.trim() || null,
        timezone: timeZone.trim() || null,
        waitingRoomPolicy: waitingRoom ? (s.waitingRoomPolicy && s.waitingRoomPolicy !== "OFF" ? s.waitingRoomPolicy : "EVERYONE") : "OFF",
        ...(passcode.trim() ? { passcode: passcode.trim() } : {}),
        ...(clearPasscode && !passcode.trim() ? { clearPasscode: true } : {}),
      },
      {
        onSuccess: () => router.back(),
        onError: (e) => {
          const d = decodeApiError(e);
          setError(d.kind === "conflict" ? "Someone else changed this meeting. Go back and try again." : d.message);
        },
      }
    );
  };

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="Edit meeting room" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 16 }}>
        <Input label="Title" value={title} onChangeText={setTitle} maxLength={200} />
        <Input label="Description" value={description} onChangeText={setDescription} multiline maxLength={2000} />
        <Input label="Time zone" value={timeZone} onChangeText={setTimeZone} autoCapitalize="none" autoCorrect={false} />
        <View className="flex-row items-center justify-between">
          <Text>Waiting room</Text>
          <Switch value={waitingRoom} onValueChange={setWaitingRoom} />
        </View>
        <Input
          label={s.requiresPasscode ? "New passcode (leave empty to keep)" : "Passcode (optional)"}
          value={passcode}
          onChangeText={setPasscode}
          secureTextEntry
          autoCapitalize="none"
        />
        {s.requiresPasscode && (
          <View className="flex-row items-center justify-between">
            <Text>Remove passcode</Text>
            <Switch value={clearPasscode} onValueChange={setClearPasscode} />
          </View>
        )}
        <Text variant="muted" size="sm">
          Access rules, roles, recording and other advanced settings are managed in CF Meet on the web.
        </Text>
        {error && (
          <Text variant="destructive" size="sm">
            {error}
          </Text>
        )}
        <Button isLoading={update.isPending} onPress={save}>
          Save changes
        </Button>
      </ScrollView>
    </SafeAreaView>
  );
}

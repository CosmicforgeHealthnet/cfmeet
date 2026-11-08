/**
 * Home: workspace switcher, instant meeting (`POST /api/v2/instant-meetings`), join by code
 * (`GET /api/v2/resolve/{identifier}`), and the next meetings of the selected workspace
 * (`GET /api/v2/me/conferences`).
 */
import { useState } from "react";
import { Pressable, ScrollView, View } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Logo } from "@/components/ui/logo";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import { useAuthStore } from "@/store/auth.store";
import { WorkspaceSwitcher, useWorkspace } from "@/features/workspace/workspace";
import { useAgenda, useInstantMeeting } from "@/features/meetings/hooks";
import { AgendaRow } from "@/features/meetings/components/agenda-row";
import { goToPreJoin, useOpenMeeting } from "@/features/meetings/open-meeting";
import { decodeApiError } from "@/lib/api/errors";
import { deviceTimeZone } from "@/lib/time/zoned";

export default function HomeScreen() {
  const { user } = useAuthStore();
  const colors = useColors();
  const { error: toastError } = useToast();
  const workspace = useWorkspace();
  const instant = useInstantMeeting();
  const { open, pending, error } = useOpenMeeting();
  const [code, setCode] = useState("");
  const deviceTz = deviceTimeZone();
  const [range] = useState(() => {
    const now = Math.floor(Date.now() / 3_600_000) * 3_600_000;
    return { from: now - 3_600_000 * 2, to: now + 7 * 86_400_000 };
  });
  const agenda = useAgenda(workspace.workspaceId, workspace.organizationId, range.from, range.to);
  const next = (agenda.data ?? [])
    .filter((i) => !["ENDED", "CANCELLED", "FAILED"].includes(i.status))
    .sort((a, b) => Date.parse(a.scheduledStartAt) - Date.parse(b.scheduledStartAt))
    .slice(0, 5);

  const startInstant = () =>
    instant.mutate(
      { timezone: deviceTz },
      {
        onSuccess: (res) => goToPreJoin(res.conference.id, res.meetingSpace),
        onError: (e) => toastError(decodeApiError(e).message),
      }
    );

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <ScrollView contentContainerStyle={{ paddingBottom: 120 }}>
        <View className="px-6 py-4">
          <View className="flex-row items-center justify-between mb-3">
            <Logo variant="medium" />
            <WorkspaceSwitcher />
          </View>
          <Text className="text-2xl font-bold">Welcome, {user?.username ?? "there"}</Text>
          <Text variant="muted" className="mt-1">
            {workspace.isPersonal ? "Your personal meetings" : `Working in ${workspace.current?.displayName}`}
          </Text>
        </View>

        <View className="flex-row justify-center gap-8 mt-4 px-6">
          <Pressable onPress={startInstant} disabled={instant.isPending} className="items-center">
            <View className="w-20 h-20 rounded-2xl bg-primary items-center justify-center">
              <Ionicons name={instant.isPending ? "sync" : "videocam"} size={28} color="#ffffff" />
            </View>
            <Text className="mt-3 font-bold">{instant.isPending ? "Starting…" : "New meeting"}</Text>
          </Pressable>
          <Pressable onPress={() => router.push("/(tabs)/schedule")} className="items-center">
            <View className="w-20 h-20 rounded-2xl bg-secondary dark:bg-secondary-dark items-center justify-center">
              <Ionicons name="calendar" size={28} color={colors.primary} />
            </View>
            <Text className="mt-3 font-bold">Schedule</Text>
          </Pressable>
        </View>
        {!workspace.isPersonal && (
          <Text variant="muted" size="xs" className="text-center mt-3 px-8">
            Instant meetings are created in your personal workspace. Schedule a meeting to create it in this organization.
          </Text>
        )}

        <View className="mt-8 px-6">
          <Text className="mb-3 font-bold">Join a meeting</Text>
          <Input
            placeholder="Meeting code or link"
            value={code}
            onChangeText={setCode}
            autoCapitalize="none"
            autoCorrect={false}
            returnKeyType="go"
            onSubmitEditing={() => void open(code)}
            error={error ?? undefined}
          />
          <Button className="mt-3" isLoading={pending} disabled={!code.trim()} onPress={() => void open(code)}>
            Join
          </Button>
        </View>

        <View className="mt-10 px-6">
          <View className="mb-3 flex-row items-center justify-between">
            <Text className="text-lg font-bold">Coming up</Text>
            <Pressable onPress={() => router.push("/(tabs)/meetings")}>
              <Text className="text-sm font-semibold text-primary">Calendar</Text>
            </Pressable>
          </View>
          {agenda.isError ? (
            <Text variant="destructive" size="sm">
              {decodeApiError(agenda.error).message}
            </Text>
          ) : next.length === 0 ? (
            <Card className="items-center py-8">
              <Ionicons name="calendar-outline" size={40} color={colors.primary} style={{ opacity: 0.5 }} />
              <Text variant="muted" className="mt-3">
                {agenda.isLoading ? "Loading…" : "Nothing in the next 7 days"}
              </Text>
            </Card>
          ) : (
            next.map((item) => <AgendaRow key={`${item.meetingSpaceId}:${item.occurrenceKey}`} item={item} deviceTz={deviceTz} />)
          )}
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}

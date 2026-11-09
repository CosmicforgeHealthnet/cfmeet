/**
 * Calendar (agenda from `GET /api/v2/me/conferences`) and meeting rooms
 * (`GET /api/v2/meeting-spaces`) of the selected workspace (FE-021 / FE-022 / ORG-016).
 */
import { useMemo, useState } from "react";
import { Pressable, RefreshControl, ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router } from "expo-router";
import { Text } from "@/components/ui/text";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { EmptyState } from "@/components/shared/empty-state";
import { WorkspaceSwitcher, useWorkspace } from "@/features/workspace/workspace";
import { useAgenda, useSpaces } from "@/features/meetings/hooks";
import { AgendaRow } from "@/features/meetings/components/agenda-row";
import { dayLabel, groupByDay } from "@/lib/meeting/agenda";
import { decodeApiError } from "@/lib/api/errors";
import { deviceTimeZone } from "@/lib/time/zoned";

const DAY = 86_400_000;

export default function MeetingsScreen() {
  const [tab, setTab] = useState<"agenda" | "rooms">("agenda");
  const workspace = useWorkspace();
  const deviceTz = deviceTimeZone();
  // Stable window: from the start of the current hour − 1 day to +30 days.
  const [range] = useState(() => {
    const now = Math.floor(Date.now() / 3_600_000) * 3_600_000;
    return { from: now - DAY, to: now + 30 * DAY };
  });
  const agenda = useAgenda(workspace.workspaceId, workspace.organizationId, range.from, range.to);
  const spaces = useSpaces(workspace.workspaceId, workspace.organizationId);
  const groups = useMemo(() => groupByDay(agenda.data ?? [], deviceTz), [agenda.data, deviceTz]);
  const active = tab === "agenda" ? agenda : spaces;

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <View className="flex-row items-center justify-between px-6 py-4">
        <Text className="text-2xl font-bold">Meetings</Text>
        <WorkspaceSwitcher />
      </View>
      <View className="mx-6 mb-2 flex-row rounded-xl bg-muted dark:bg-muted-dark p-1">
        {(["agenda", "rooms"] as const).map((t) => (
          <Pressable key={t} onPress={() => setTab(t)} className={`flex-1 rounded-lg py-2 ${tab === t ? "bg-card dark:bg-card-dark" : ""}`}>
            <Text className="text-center font-semibold">{t === "agenda" ? "Calendar" : "Meeting rooms"}</Text>
          </Pressable>
        ))}
      </View>
      <ScrollView
        contentContainerStyle={{ padding: 24, paddingBottom: 120 }}
        refreshControl={<RefreshControl refreshing={active.isRefetching} onRefresh={() => void active.refetch()} />}
      >
        {active.isLoading ? (
          <Loading />
        ) : active.isError ? (
          <ErrorState message={decodeApiError(active.error).message} onRetry={() => void active.refetch()} />
        ) : tab === "agenda" ? (
          groups.length === 0 ? (
            <EmptyState
              title="Nothing scheduled"
              description="Meetings you host, are invited to or registered for appear here."
              actionLabel="Schedule a meeting"
              onAction={() => router.push("/(tabs)/schedule")}
            />
          ) : (
            groups.map((g) => (
              <View key={g.day} className="mb-4">
                <Text className="mb-2 font-bold">{dayLabel(g.day, Date.now(), deviceTz)}</Text>
                {g.items.map((item) => (
                  <AgendaRow key={`${item.meetingSpaceId}:${item.occurrenceKey}`} item={item} deviceTz={deviceTz} />
                ))}
              </View>
            ))
          )
        ) : (spaces.data ?? []).length === 0 ? (
          <EmptyState title="No meeting rooms" description="Rooms you own or manage in this workspace appear here." />
        ) : (
          (spaces.data ?? []).map((s) => (
            <Pressable
              key={s.id}
              onPress={() => router.push({ pathname: "/(meetings)/[id]", params: { id: s.id } })}
              className="rounded-xl border border-border dark:border-border-dark p-4 mb-2"
            >
              <Text className="font-semibold">{s.title || s.meetingCode}</Text>
              <Text variant="muted" size="xs">
                {s.meetingCode}
                {s.latestConference ? ` · ${s.latestConference.status.toLowerCase()}` : ""}
                {s.timezone ? ` · ${s.timezone}` : ""}
              </Text>
            </Pressable>
          ))
        )}
      </ScrollView>
    </SafeAreaView>
  );
}

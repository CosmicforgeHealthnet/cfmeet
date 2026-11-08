/**
 * Meeting room (space) detail: sessions (conferences) with join/start, reschedule and cancel;
 * edit and archive for hosts. All Meeting Service v2.
 */
import { useState } from "react";
import { Alert, Modal, Pressable, ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router, useLocalSearchParams } from "expo-router";
import * as Clipboard from "expo-clipboard";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { ScreenHeader } from "@/components/shared/screen-header";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import { WallTimePicker } from "@/features/meetings/components/wall-time-picker";
import {
  useAddConference,
  useArchiveSpace,
  useCancelConference,
  useReschedule,
  useSpace,
  useSpaceConferences,
} from "@/features/meetings/hooks";
import { canHost, goToPreJoin, openSpace } from "@/features/meetings/open-meeting";
import { conferenceAction, describeTime, statusLabel } from "@/lib/meeting/agenda";
import { decodeApiError } from "@/lib/api/errors";
import { deviceTimeZone, isValidTimeZone, scheduleProblem, toIsoUtc, wallTimeAt, zonedToUtc, type WallTime } from "@/lib/time/zoned";
import type { ConferenceResponse } from "@/lib/api/generated/meeting-service";

const TERMINAL = ["ENDED", "CANCELLED", "FAILED"];

export default function SpaceDetailScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const colors = useColors();
  const { success, error: toastError } = useToast();
  const space = useSpace(id);
  const conferences = useSpaceConferences(id);
  const cancel = useCancelConference(id);
  const archive = useArchiveSpace(id);
  const addConference = useAddConference(id);
  const reschedule = useReschedule(id);
  const [editing, setEditing] = useState<ConferenceResponse | "new" | null>(null);
  const deviceTz = deviceTimeZone();

  if (space.isLoading) return <Loading fullScreen />;
  if (space.isError || !space.data) {
    return (
      <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
        <ScreenHeader title="Meeting" />
        <ErrorState message={decodeApiError(space.error).message} onRetry={() => void space.refetch()} />
      </SafeAreaView>
    );
  }
  const s = space.data;
  const host = canHost(s);
  const tz = s.timezone && isValidTimeZone(s.timezone) ? s.timezone : deviceTz;
  const list = (conferences.data?.items ?? [])
    .slice()
    .sort((a, b) => Date.parse(a.scheduledStartAt ?? a.startedAt ?? "") - Date.parse(b.scheduledStartAt ?? b.startedAt ?? ""));
  const upcoming = list.filter((c) => !TERMINAL.includes(c.status));
  const past = list.filter((c) => TERMINAL.includes(c.status)).slice(-5).reverse();

  const onError = (e: unknown) => toastError(decodeApiError(e).message);

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader
        title={s.title || s.meetingCode}
        subtitle={s.organizationId ? "Organization meeting" : "Personal meeting"}
        right={
          host ? (
            <Pressable accessibilityLabel="Edit meeting" onPress={() => router.push({ pathname: "/(meetings)/[id]/edit", params: { id } })}>
              <Ionicons name="create-outline" size={22} color={colors.primary} />
            </Pressable>
          ) : undefined
        }
      />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 16, paddingBottom: 64 }}>
        <Card>
          <Pressable
            onPress={async () => {
              await Clipboard.setStringAsync(s.meetingCode);
              success("Meeting code copied");
            }}
            className="flex-row items-center justify-between"
          >
            <View>
              <Text variant="muted" size="xs">
                Meeting code
              </Text>
              <Text className="text-lg font-bold">{s.meetingCode}</Text>
            </View>
            <Ionicons name="copy-outline" size={20} color={colors.mutedForeground} />
          </Pressable>
          {s.description ? <Text className="mt-3">{s.description}</Text> : null}
          <Text variant="muted" size="xs" className="mt-3">
            Time zone {tz} · {s.requiresPasscode ? "passcode required" : "no passcode"} · access {s.accessPolicy.toLowerCase().replace(/_/g, " ")}
          </Text>
        </Card>

        <View className="flex-row gap-3">
          <Button
            className="flex-1"
            onPress={() => openSpace(s, upcoming.find((c) => c.status === "LIVE")?.id ?? upcoming[0]?.id).catch(onError)}
          >
            {upcoming.some((c) => c.status === "LIVE") ? "Join now" : host ? "Start now" : "Join"}
          </Button>
          {host && (
            <Button variant="outline" className="flex-1" onPress={() => setEditing("new")}>
              Add session
            </Button>
          )}
        </View>

        <Text className="font-bold">Upcoming sessions</Text>
        {conferences.isError ? (
          <Text variant="destructive" size="sm">
            {decodeApiError(conferences.error).message}
          </Text>
        ) : upcoming.length === 0 ? (
          <Text variant="muted">No upcoming sessions.</Text>
        ) : (
          upcoming.map((c) => {
            const start = c.scheduledStartAt ?? c.startedAt;
            const time = start ? describeTime(start, c.scheduledEndAt, tz, deviceTz) : null;
            const action = conferenceAction(c.status, s.viewerRole);
            return (
              <Card key={c.id}>
                <View className="flex-row items-center justify-between">
                  <View className="flex-1">
                    <Text className="font-semibold">{time ? time.local : "Not scheduled"}</Text>
                    {time?.spaceLocal ? (
                      <Text variant="muted" size="xs">
                        {time.spaceLocal}
                      </Text>
                    ) : null}
                    {start ? (
                      <Text variant="muted" size="xs">
                        {new Date(start).toLocaleDateString()}
                      </Text>
                    ) : null}
                  </View>
                  <Badge variant={c.status === "LIVE" ? "success" : "secondary"}>{statusLabel(c.status)}</Badge>
                </View>
                <View className="flex-row gap-2 mt-3">
                  {action !== "none" && (
                    <Button size="sm" onPress={() => goToPreJoin(c.id, s)}>
                      {action === "start" ? "Start" : "Join"}
                    </Button>
                  )}
                  {host && c.status !== "LIVE" && (
                    <>
                      <Button size="sm" variant="outline" onPress={() => setEditing(c)}>
                        Reschedule
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onPress={() =>
                          Alert.alert("Cancel this session?", "Invitees will no longer be able to join it.", [
                            { text: "Keep", style: "cancel" },
                            { text: "Cancel session", style: "destructive", onPress: () => cancel.mutate(c.id, { onError }) },
                          ])
                        }
                      >
                        Cancel
                      </Button>
                    </>
                  )}
                </View>
              </Card>
            );
          })
        )}

        {past.length > 0 && (
          <>
            <Text className="font-bold">Past sessions</Text>
            {past.map((c) => (
              <Text key={c.id} variant="muted" size="sm">
                {(c.startedAt ?? c.scheduledStartAt) ? new Date((c.startedAt ?? c.scheduledStartAt)!).toLocaleString() : "—"} ·{" "}
                {statusLabel(c.status)}
              </Text>
            ))}
          </>
        )}

        {host && (
          <Button
            variant="destructive"
            className="mt-6"
            isLoading={archive.isPending}
            onPress={() =>
              Alert.alert("Archive this meeting room?", "It will no longer accept new sessions.", [
                { text: "Keep", style: "cancel" },
                {
                  text: "Archive",
                  style: "destructive",
                  onPress: () =>
                    archive.mutate(s.version, {
                      onSuccess: () => router.back(),
                      onError,
                    }),
                },
              ])
            }
          >
            Archive meeting room
          </Button>
        )}
      </ScrollView>

      <SessionTimeModal
        visible={editing !== null}
        timeZone={tz}
        initial={editing && editing !== "new" ? editing : null}
        busy={addConference.isPending || reschedule.isPending}
        onClose={() => setEditing(null)}
        onSave={(startMs, endMs) => {
          const done = { onSuccess: () => setEditing(null), onError };
          if (editing === "new") addConference.mutate({ startMs, endMs }, done);
          else if (editing)
            reschedule.mutate(
              {
                conferenceId: editing.id,
                data: {
                  scheduledStartAt: toIsoUtc(startMs),
                  scheduledEndAt: toIsoUtc(endMs),
                  version: editing.version ?? null,
                },
              },
              done
            );
        }}
      />
    </SafeAreaView>
  );
}

function SessionTimeModal({
  visible,
  timeZone,
  initial,
  busy,
  onClose,
  onSave,
}: {
  visible: boolean;
  timeZone: string;
  initial: ConferenceResponse | null;
  busy: boolean;
  onClose: () => void;
  onSave: (startMs: number, endMs: number) => void;
}) {
  const initialStart = initial?.scheduledStartAt ? Date.parse(initial.scheduledStartAt) : Math.ceil(Date.now() / 1_800_000) * 1_800_000;
  const initialDuration =
    initial?.scheduledStartAt && initial.scheduledEndAt
      ? Math.round((Date.parse(initial.scheduledEndAt) - Date.parse(initial.scheduledStartAt)) / 60_000)
      : 30;
  const [start, setStart] = useState<WallTime>(() => wallTimeAt(initialStart, timeZone));
  const [duration, setDuration] = useState(initialDuration);
  const [problem, setProblem] = useState<string | null>(null);
  const key = `${initial?.id ?? "new"}:${visible}`;
  const [lastKey, setLastKey] = useState(key);
  if (key !== lastKey) {
    setLastKey(key);
    setStart(wallTimeAt(initialStart, timeZone));
    setDuration(initialDuration);
    setProblem(null);
  }

  return (
    <Modal transparent visible={visible} animationType="fade" onRequestClose={onClose}>
      <Pressable className="flex-1 bg-black/40 justify-end" onPress={onClose}>
        <Pressable className="bg-card dark:bg-card-dark rounded-t-3xl p-6 gap-4">
          <Text className="text-lg font-bold">{initial ? "Reschedule session" : "Add a session"}</Text>
          <WallTimePicker label={`Starts (${timeZone})`} value={start} onChange={setStart} />
          <View className="flex-row flex-wrap gap-2">
            {[15, 30, 45, 60, 90, 120].map((d) => (
              <Pressable
                key={d}
                onPress={() => setDuration(d)}
                className={`rounded-full px-4 py-2 border ${d === duration ? "bg-primary border-primary" : "border-border dark:border-border-dark"}`}
              >
                <Text className={d === duration ? "text-white font-semibold" : ""}>{d < 60 ? `${d} min` : `${d / 60} h`}</Text>
              </Pressable>
            ))}
          </View>
          {problem && (
            <Text variant="destructive" size="sm">
              {problem}
            </Text>
          )}
          <Button
            isLoading={busy}
            onPress={() => {
              const startMs = zonedToUtc(start, timeZone);
              const endMs = startMs + duration * 60_000;
              const p = scheduleProblem(startMs, endMs, Date.now());
              if (p) setProblem(p);
              else onSave(startMs, endMs);
            }}
          >
            Save
          </Button>
        </Pressable>
      </Pressable>
    </Modal>
  );
}

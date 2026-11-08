/** Participants and (for admitting roles) the waiting room, from Meeting Service v2. */
import { Alert, Pressable, ScrollView, View } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { useColors } from "@/hooks/useColors";
import { useAdmission, useHands, useParticipants, useWaitingRoom } from "@/features/room/hooks";
import { decodeApiError } from "@/lib/api/errors";
import { mayTarget, meetingAllows, roleLabel } from "@/lib/meeting/authority";
import type { Role } from "@/lib/api/generated/meeting-service";

export function ParticipantsPanel({
  conferenceId,
  participantId,
  role,
}: {
  conferenceId: string;
  participantId: string;
  role: Role | undefined;
}) {
  const colors = useColors();
  const canAdmit = meetingAllows(role, "participant.admit");
  const participants = useParticipants(conferenceId);
  const waiting = useWaitingRoom(conferenceId, canAdmit);
  const hands = useHands(conferenceId, true);
  const { admit, deny, remove, muteAudio } = useAdmission(conferenceId);
  const raised = new Set((hands.query.data?.hands ?? []).map((h) => h.participantId));

  const onError = (e: unknown) => Alert.alert("Action failed", decodeApiError(e).message);
  const admitted = (participants.data ?? []).filter((p) => p.admissionState === "ADMITTED");
  const waitingList = waiting.data?.participants ?? [];

  return (
    <ScrollView contentContainerStyle={{ padding: 16, gap: 16 }}>
      {canAdmit && (
        <View>
          <View className="flex-row items-center justify-between mb-2">
            <Text className="font-bold">Waiting room ({waitingList.length})</Text>
            {waitingList.length > 1 && (
              <Button size="sm" variant="outline" onPress={() => admit.mutate("all", { onError })}>
                Admit all
              </Button>
            )}
          </View>
          {waiting.isError ? (
            <Text variant="muted" size="sm">
              {decodeApiError(waiting.error).message}
            </Text>
          ) : waitingList.length === 0 ? (
            <Text variant="muted" size="sm">
              Nobody is waiting.
            </Text>
          ) : (
            waitingList.map((p) => (
              <View key={p.id} className="flex-row items-center justify-between py-2">
                <Text className="flex-1" numberOfLines={1}>
                  {p.displayName}
                  {p.identityType === "GUEST" ? " (guest)" : ""}
                </Text>
                <View className="flex-row gap-2">
                  <Button size="sm" onPress={() => admit.mutate([p.id], { onError })}>
                    Admit
                  </Button>
                  <Button size="sm" variant="outline" onPress={() => deny.mutate([p.id], { onError })}>
                    Deny
                  </Button>
                </View>
              </View>
            ))
          )}
        </View>
      )}
      <View>
        <Text className="font-bold mb-2">In the meeting ({admitted.length})</Text>
        {participants.isError ? (
          <Text variant="muted" size="sm">
            {decodeApiError(participants.error).message}
          </Text>
        ) : null}
        {admitted.map((p) => {
          const self = p.id === participantId;
          const canAct = mayTarget(role, p.role, self);
          return (
            <View
              key={p.id}
              className="flex-row items-center justify-between py-2 border-b border-border dark:border-border-dark"
            >
              <View className="flex-1">
                <Text numberOfLines={1}>
                  {p.displayName}
                  {self ? " (you)" : ""}
                </Text>
                <Text variant="muted" size="xs">
                  {roleLabel(p.role)}
                  {raised.has(p.id) ? " · hand raised" : ""}
                </Text>
              </View>
              {raised.has(p.id) && <Ionicons name="hand-left" size={16} color={colors.warning} />}
              {canAct && meetingAllows(role, "participant.mute") && (
                <Pressable
                  accessibilityLabel={`Mute ${p.displayName}`}
                  className="p-2"
                  onPress={() => muteAudio.mutate(p.id, { onError })}
                >
                  <Ionicons name="mic-off-outline" size={18} color={colors.foreground} />
                </Pressable>
              )}
              {canAct && meetingAllows(role, "participant.remove") && (
                <Pressable
                  accessibilityLabel={`Remove ${p.displayName}`}
                  className="p-2"
                  onPress={() =>
                    Alert.alert("Remove participant", `Remove ${p.displayName} from the meeting?`, [
                      { text: "Cancel", style: "cancel" },
                      { text: "Remove", style: "destructive", onPress: () => remove.mutate(p.id, { onError }) },
                    ])
                  }
                >
                  <Ionicons name="person-remove-outline" size={18} color={colors.destructive} />
                </Pressable>
              )}
            </View>
          );
        })}
      </View>
    </ScrollView>
  );
}

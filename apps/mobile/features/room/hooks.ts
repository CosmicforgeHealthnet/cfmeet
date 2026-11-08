/** Live-room data from Meeting Service v2 (participants, waiting room, chat, hands). */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { meetingsApi } from "@/features/meetings/api";
import type { MessageView } from "@/lib/api/generated/meeting-service";

export const roomKeys = {
  participants: (id: string) => ["conference", id, "participants"] as const,
  waiting: (id: string) => ["conference", id, "waiting-room"] as const,
  messages: (id: string) => ["conference", id, "messages"] as const,
  hands: (id: string) => ["conference", id, "hands"] as const,
};

export function useParticipants(conferenceId: string, enabled = true) {
  return useQuery({
    queryKey: roomKeys.participants(conferenceId),
    queryFn: () => meetingsApi.participants(conferenceId),
    enabled,
    refetchInterval: 5000,
  });
}

/** Waiting room (only for roles that may admit; a 403 simply hides the section). */
export function useWaitingRoom(conferenceId: string, enabled: boolean) {
  return useQuery({
    queryKey: roomKeys.waiting(conferenceId),
    queryFn: () => meetingsApi.waitingRoom(conferenceId),
    enabled,
    refetchInterval: 4000,
    retry: false,
  });
}

export function useAdmission(conferenceId: string) {
  const qc = useQueryClient();
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: roomKeys.waiting(conferenceId) });
    void qc.invalidateQueries({ queryKey: roomKeys.participants(conferenceId) });
  };
  const admit = useMutation({
    mutationFn: (participantIds: string[] | "all") =>
      meetingsApi.admit(conferenceId, participantIds === "all" ? { all: true } : { participantIds }),
    onSettled: refresh,
  });
  const deny = useMutation({
    mutationFn: (participantIds: string[]) => meetingsApi.deny(conferenceId, { participantIds }),
    onSettled: refresh,
  });
  const remove = useMutation({
    mutationFn: (participantId: string) => meetingsApi.removeParticipant(conferenceId, participantId, {}),
    onSettled: refresh,
  });
  const muteAudio = useMutation({
    mutationFn: (participantId: string) => meetingsApi.muteParticipant(conferenceId, participantId, { trackKind: "AUDIO" }),
  });
  return { admit, deny, remove, muteAudio };
}

function byTime(a: MessageView, b: MessageView): number {
  return Date.parse(a.createdAt) - Date.parse(b.createdAt);
}

/** Main-room chat (MSG-012: v2 `/conferences/{id}/messages`, never legacy v1 chat). */
export function useConferenceChat(conferenceId: string, enabled: boolean) {
  const qc = useQueryClient();
  const query = useQuery({
    queryKey: roomKeys.messages(conferenceId),
    queryFn: async () => (await meetingsApi.messages(conferenceId)).messages.slice().sort(byTime),
    enabled,
    refetchInterval: 4000,
  });
  const send = useMutation({
    mutationFn: (body: string) => meetingsApi.sendMessage(conferenceId, { body }),
    onSuccess: (message) => {
      qc.setQueryData<MessageView[]>(roomKeys.messages(conferenceId), (old) =>
        [...(old ?? []).filter((m) => m.id !== message.id), message].sort(byTime)
      );
    },
  });
  return { query, send };
}

export function useHands(conferenceId: string, enabled: boolean) {
  const qc = useQueryClient();
  const query = useQuery({
    queryKey: roomKeys.hands(conferenceId),
    queryFn: () => meetingsApi.raisedHands(conferenceId),
    enabled,
    refetchInterval: 5000,
  });
  const toggle = useMutation({
    mutationFn: (raise: boolean) => (raise ? meetingsApi.raiseHand(conferenceId) : meetingsApi.lowerHand(conferenceId)),
    onSettled: () => void qc.invalidateQueries({ queryKey: roomKeys.hands(conferenceId) }),
  });
  return { query, toggle };
}

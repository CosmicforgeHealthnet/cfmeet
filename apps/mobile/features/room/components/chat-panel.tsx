/** In-meeting chat on the v2 conference chat (MSG-012). */
import { useState } from "react";
import { FlatList, KeyboardAvoidingView, Platform, Pressable, TextInput, View } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { useColors } from "@/hooks/useColors";
import { useConferenceChat } from "@/features/room/hooks";
import { decodeApiError } from "@/lib/api/errors";
import { clock } from "@/lib/meeting/agenda";
import { deviceTimeZone } from "@/lib/time/zoned";

export function ChatPanel({
  conferenceId,
  participantId,
  canSend,
}: {
  conferenceId: string;
  participantId: string;
  canSend: boolean;
}) {
  const colors = useColors();
  const { query, send } = useConferenceChat(conferenceId, true);
  const [text, setText] = useState("");
  const tz = deviceTimeZone();
  const messages = query.data ?? [];

  const submit = () => {
    const body = text.trim();
    if (!body) return;
    send.mutate(body, { onSuccess: () => setText("") });
  };

  return (
    <KeyboardAvoidingView className="flex-1" behavior={Platform.OS === "ios" ? "padding" : undefined}>
      {query.isError ? (
        <Text variant="destructive" size="sm" className="p-4">
          {decodeApiError(query.error).message}
        </Text>
      ) : null}
      <FlatList
        data={messages}
        keyExtractor={(m) => m.id}
        contentContainerStyle={{ padding: 16, gap: 10 }}
        ListEmptyComponent={
          <Text variant="muted" className="text-center mt-8">
            No messages yet.
          </Text>
        }
        renderItem={({ item }) => {
          const mine = item.senderParticipantId === participantId;
          return (
            <View className={mine ? "items-end" : "items-start"}>
              <Text variant="muted" size="xs">
                {mine ? "You" : item.senderDisplayName} · {clock(Date.parse(item.createdAt), tz)}
                {item.kind !== "MESSAGE" ? " · announcement" : ""}
              </Text>
              <View
                className={`mt-1 max-w-[85%] rounded-2xl px-3 py-2 ${mine ? "bg-primary" : "bg-muted dark:bg-muted-dark"}`}
              >
                <Text className={mine ? "text-white" : ""}>{item.deleted ? "Message removed" : (item.body ?? "")}</Text>
              </View>
            </View>
          );
        }}
      />
      {send.isError ? (
        <Text variant="destructive" size="sm" className="px-4">
          {decodeApiError(send.error).message}
        </Text>
      ) : null}
      <View className="flex-row items-center gap-2 border-t border-border dark:border-border-dark p-3">
        <TextInput
          value={text}
          onChangeText={setText}
          editable={canSend}
          placeholder={canSend ? "Message everyone" : "Chat is not available to you"}
          placeholderTextColor={colors.mutedForeground}
          maxLength={4000}
          className="flex-1 rounded-xl border border-border dark:border-border-dark px-3 py-2 text-foreground dark:text-foreground-dark"
          onSubmitEditing={submit}
        />
        <Pressable
          accessibilityLabel="Send message"
          onPress={submit}
          disabled={!canSend || send.isPending}
          className="p-2"
        >
          <Ionicons name="send" size={22} color={colors.primary} />
        </Pressable>
      </View>
    </KeyboardAvoidingView>
  );
}

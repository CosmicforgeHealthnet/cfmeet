/** One conversation. MANAGED: read and send. PRIVATE_E2EE: truthful notice, no content. */
import { useState } from "react";
import { FlatList, KeyboardAvoidingView, Platform, Pressable, TextInput, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { useLocalSearchParams } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { Notice, ScreenHeader } from "@/components/shared/screen-header";
import { useColors } from "@/hooks/useColors";
import {
  useConversation,
  useConversationLines,
  useMessagingIdentity,
  useNames,
  useSendMessage,
} from "@/features/messaging/hooks";
import { MANAGED_DISCLOSURE, PRIVATE_ON_WEB, conversationTitle } from "@/lib/messaging/relay-paths";
import { decodeApiError } from "@/lib/api/errors";
import { clock } from "@/lib/meeting/agenda";
import { deviceTimeZone } from "@/lib/time/zoned";

export default function ConversationScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const colors = useColors();
  const identity = useMessagingIdentity();
  const me = identity.data?.principalId ?? null;
  const conversation = useConversation(id);
  const readable = !!conversation.data?.readableHere;
  const lines = useConversationLines(id, me, readable && identity.isSuccess);
  const names = useNames([
    ...(conversation.data?.memberIds ?? []),
    ...(lines.data ?? []).map((l) => l.senderPrincipalId),
  ]);
  const send = useSendMessage(id);
  const [text, setText] = useState("");
  const tz = deviceTimeZone();

  if (conversation.isLoading) return <Loading fullScreen />;
  if (conversation.isError || !conversation.data) {
    return (
      <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
        <ScreenHeader title="Conversation" />
        <ErrorState message={decodeApiError(conversation.error).message} onRetry={() => void conversation.refetch()} />
      </SafeAreaView>
    );
  }
  const c = conversation.data;
  const title = conversationTitle(c, me, names.data ?? {});

  const submit = () => {
    const body = text.trim();
    if (!body) return;
    send.mutate(body, { onSuccess: () => setText("") });
  };

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title={title} subtitle={readable ? "Managed conversation" : "Private conversation"} />
      {!readable ? (
        <View className="p-6">
          <Notice title="Open on the web" body={PRIVATE_ON_WEB} />
        </View>
      ) : (
        <KeyboardAvoidingView className="flex-1" behavior={Platform.OS === "ios" ? "padding" : undefined}>
          <FlatList
            data={lines.data ?? []}
            keyExtractor={(l) => l.key}
            contentContainerStyle={{ padding: 16, gap: 10 }}
            ListHeaderComponent={
              <Text variant="muted" size="xs" className="mb-2 text-center">
                {MANAGED_DISCLOSURE}
              </Text>
            }
            ListEmptyComponent={
              lines.isError ? (
                <Text variant="destructive" size="sm" className="text-center">
                  {decodeApiError(lines.error).message}
                </Text>
              ) : (
                <Text variant="muted" className="text-center mt-8">
                  {lines.isLoading ? "Loading…" : "No messages yet."}
                </Text>
              )
            }
            renderItem={({ item }) => (
              <View className={item.mine ? "items-end" : "items-start"}>
                <Text variant="muted" size="xs">
                  {item.mine ? "You" : (names.data?.[item.senderPrincipalId] ?? "Member")} · {clock(Date.parse(item.createdAt), tz)}
                </Text>
                <View className={`mt-1 max-w-[85%] rounded-2xl px-3 py-2 ${item.mine ? "bg-primary" : "bg-muted dark:bg-muted-dark"}`}>
                  <Text className={item.mine ? "text-white" : ""}>
                    {item.removed ? "Message removed" : (item.text ?? (item.hasAttachment ? "Attachment (open on the web)" : ""))}
                  </Text>
                </View>
              </View>
            )}
          />
          {send.isError && (
            <Text variant="destructive" size="sm" className="px-4">
              Not sent: {decodeApiError(send.error).message}
            </Text>
          )}
          <View className="flex-row items-center gap-2 border-t border-border dark:border-border-dark p-3">
            <TextInput
              value={text}
              onChangeText={setText}
              editable={c.active}
              placeholder={c.active ? "Message" : "This conversation is closed"}
              placeholderTextColor={colors.mutedForeground}
              maxLength={4000}
              className="flex-1 rounded-xl border border-border dark:border-border-dark px-3 py-2 text-foreground dark:text-foreground-dark"
              onSubmitEditing={submit}
            />
            <Pressable accessibilityLabel="Send" onPress={submit} disabled={!c.active || send.isPending} className="p-2">
              <Ionicons name="send" size={22} color={colors.primary} />
            </Pressable>
          </View>
        </KeyboardAvoidingView>
      )}
    </SafeAreaView>
  );
}

/**
 * Contacts → new MANAGED chat. People come from the Messaging directory
 * (`GET /api/v2/messaging/directory`: colleagues the account may message). The managed
 * disclosure is shown and must be acknowledged before the conversation is created.
 */
import { useState } from "react";
import { Pressable, ScrollView, Switch, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { Notice, ScreenHeader } from "@/components/shared/screen-header";
import { useColors } from "@/hooks/useColors";
import { useCreateManagedConversation, useDirectory } from "@/features/messaging/hooks";
import { MANAGED_DISCLOSURE } from "@/lib/messaging/relay-paths";
import { decodeApiError } from "@/lib/api/errors";

export default function NewChatScreen() {
  const colors = useColors();
  const [q, setQ] = useState("");
  const [selected, setSelected] = useState<Record<string, string>>({});
  const [acknowledged, setAcknowledged] = useState(false);
  const directory = useDirectory(q.trim());
  const create = useCreateManagedConversation();
  const ids = Object.keys(selected);

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="New chat" subtitle="Contacts" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 12, paddingBottom: 48 }}>
        <Input placeholder="Search people" value={q} onChangeText={setQ} autoCapitalize="none" autoCorrect={false} />
        {directory.isLoading ? (
          <Loading />
        ) : directory.isError ? (
          <Text variant="destructive" size="sm">
            {decodeApiError(directory.error).message}
          </Text>
        ) : (directory.data ?? []).length === 0 ? (
          <Text variant="muted">No contacts found.</Text>
        ) : (
          (directory.data ?? []).map((p) => {
            const on = !!selected[p.userId];
            const reachable = !!p.principalId;
            return (
              <Pressable
                key={p.userId}
                disabled={!reachable}
                onPress={() =>
                  setSelected((s) => {
                    const next = { ...s };
                    if (on) delete next[p.userId];
                    else next[p.userId] = p.displayName;
                    return next;
                  })
                }
                className={`flex-row items-center gap-3 py-3 border-b border-border dark:border-border-dark ${reachable ? "" : "opacity-50"}`}
              >
                <Ionicons name={on ? "checkmark-circle" : "ellipse-outline"} size={22} color={colors.primary} />
                <View className="flex-1">
                  <Text>{p.displayName}</Text>
                  {!reachable && (
                    <Text variant="muted" size="xs">
                      Has not set up messaging yet
                    </Text>
                  )}
                </View>
              </Pressable>
            );
          })
        )}
        <Notice title="Managed conversation" body={MANAGED_DISCLOSURE} />
        <View className="flex-row items-center justify-between">
          <Text className="flex-1">I understand this conversation is not end-to-end encrypted.</Text>
          <Switch value={acknowledged} onValueChange={setAcknowledged} />
        </View>
        {create.isError && (
          <Text variant="destructive" size="sm">
            {decodeApiError(create.error).message}
          </Text>
        )}
        <Button
          disabled={ids.length === 0 || !acknowledged}
          isLoading={create.isPending}
          onPress={() =>
            create.mutate(ids, {
              onSuccess: (conv) => router.replace({ pathname: "/(chats)/[id]", params: { id: conv.conversationId } }),
            })
          }
        >
          {ids.length > 1 ? `Start group chat (${ids.length})` : "Start chat"}
        </Button>
        <Text variant="muted" size="xs">
          Private (end-to-end encrypted) conversations can be started in CF Meet on the web.
        </Text>
      </ScrollView>
    </SafeAreaView>
  );
}

/**
 * Chats: Unified Messaging conversations through the Meeting Service bridge. Mobile reads and
 * sends in MANAGED conversations only and labels them truthfully; private (end-to-end
 * encrypted) conversations are listed but open only on a registered web device.
 */
import { Pressable, RefreshControl, ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { EmptyState } from "@/components/shared/empty-state";
import { useColors } from "@/hooks/useColors";
import { useConversations, useMessagingIdentity, useNames } from "@/features/messaging/hooks";
import { conversationTitle } from "@/lib/messaging/relay-paths";
import { decodeApiError } from "@/lib/api/errors";

export default function ChatsScreen() {
  const colors = useColors();
  const identity = useMessagingIdentity();
  const conversations = useConversations(identity.isSuccess);
  const me = identity.data?.principalId ?? null;
  const names = useNames((conversations.data ?? []).flatMap((c) => c.memberIds));

  const unavailable = identity.isError ? decodeApiError(identity.error) : null;

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <View className="flex-row items-center justify-between px-6 py-4">
        <Text className="text-2xl font-bold">Chats</Text>
        <Pressable
          accessibilityLabel="New chat"
          onPress={() => router.push("/(chats)/new")}
          disabled={!identity.isSuccess}
          className="p-1"
        >
          <Ionicons name="create-outline" size={24} color={colors.primary} />
        </Pressable>
      </View>
      <ScrollView
        contentContainerStyle={{ padding: 24, paddingBottom: 120 }}
        refreshControl={<RefreshControl refreshing={conversations.isRefetching} onRefresh={() => void conversations.refetch()} />}
      >
        {identity.isLoading || conversations.isLoading ? (
          <Loading />
        ) : unavailable ? (
          <ErrorState
            title="Messaging is unavailable"
            message={
              unavailable.status === 503 || unavailable.kind === "unavailable"
                ? "Messaging is not available for your account right now."
                : unavailable.message
            }
            onRetry={() => void identity.refetch()}
          />
        ) : conversations.isError ? (
          <ErrorState message={decodeApiError(conversations.error).message} onRetry={() => void conversations.refetch()} />
        ) : (conversations.data ?? []).length === 0 ? (
          <EmptyState
            title="No conversations yet"
            description="Start a managed chat with someone from your contacts."
            actionLabel="New chat"
            onAction={() => router.push("/(chats)/new")}
          />
        ) : (
          (conversations.data ?? []).map((c) => (
            <Pressable
              key={c.id}
              onPress={() => router.push({ pathname: "/(chats)/[id]", params: { id: c.id } })}
              className="flex-row items-center gap-3 rounded-xl border border-border dark:border-border-dark p-4 mb-2"
            >
              <Ionicons
                name={c.readableHere ? (c.isDirect ? "person-circle-outline" : "people-circle-outline") : "lock-closed-outline"}
                size={28}
                color={colors.primary}
              />
              <View className="flex-1">
                <Text className="font-semibold" numberOfLines={1}>
                  {conversationTitle(c, me, names.data ?? {})}
                </Text>
                <Text variant="muted" size="xs">
                  {c.readableHere ? "Managed" : "Private · open on the web"}
                  {c.active ? "" : " · closed"}
                </Text>
              </View>
            </Pressable>
          ))
        )}
      </ScrollView>
    </SafeAreaView>
  );
}

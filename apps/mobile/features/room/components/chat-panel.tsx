import { useState, useRef, useEffect } from "react";
import { View, FlatList, TextInput, Pressable, KeyboardAvoidingView, Platform } from "react-native";
import { Text } from "@/components/ui/text";
import { Avatar } from "@/components/ui/avatar";
import { Loading } from "@/components/shared/loading";
import { useRoom } from "../providers/room-provider";
import { useChat, useSendMessage } from "../hooks/use-chat";
import { useToast } from "@/components/ui/toast";
import type { ChatMessage } from "@/features/meetings/types";

export function ChatPanel() {
  const [message, setMessage] = useState("");
  const flatListRef = useRef<FlatList<ChatMessage>>(null);
  const { joinData } = useRoom();
  const meetingId = joinData?.id ?? "";
  const currentParticipantId = joinData?.participant_id;

  const { data, isLoading } = useChat(meetingId);
  const sendMessage = useSendMessage(meetingId);
  const { toast } = useToast();

  const messages = data?.messages ?? [];

  useEffect(() => {
    if (messages.length > 0) {
      flatListRef.current?.scrollToEnd({ animated: true });
    }
  }, [messages.length]);

  const handleSend = () => {
    const trimmedMessage = message.trim();
    if (!trimmedMessage) return;

    sendMessage.mutate(trimmedMessage, {
      onSuccess: () => {
        setMessage("");
      },
      onError: (error) => {
        toast(error.message, "destructive");
      },
    });
  };

  const formatTime = (dateString: string) => {
    const date = new Date(dateString);
    return date.toLocaleTimeString("en-US", {
      hour: "numeric",
      minute: "2-digit",
      hour12: true,
    });
  };

  const renderMessage = ({ item }: { item: ChatMessage }) => {
    const isCurrentUser = item.participant_id === currentParticipantId;

    return (
      <View
        className={`mb-3 max-w-[85%] ${
          isCurrentUser ? "ml-auto items-end" : "mr-auto items-start"
        }`}
      >
        {!isCurrentUser && (
          <View className="mb-1 flex-row items-center gap-2">
            <Avatar fallback={item.display_name} size="sm" />
            <Text className="text-xs text-muted-foreground">
              {item.display_name}
            </Text>
          </View>
        )}
        <View
          className={`rounded-2xl px-4 py-2 ${
            isCurrentUser ? "bg-primary" : "bg-muted"
          }`}
        >
          <Text
            className={isCurrentUser ? "text-primary-foreground" : "text-foreground"}
          >
            {item.message}
          </Text>
        </View>
        <Text className="mt-1 text-xs text-muted-foreground">
          {formatTime(item.created_at)}
        </Text>
      </View>
    );
  };

  if (isLoading) {
    return <Loading message="Loading chat..." />;
  }

  return (
    <KeyboardAvoidingView
      behavior={Platform.OS === "ios" ? "padding" : "height"}
      className="flex-1"
      keyboardVerticalOffset={100}
    >
      <View className="flex-1">
        {messages.length === 0 ? (
          <View className="flex-1 items-center justify-center">
            <Text className="text-muted-foreground">No messages yet</Text>
            <Text className="mt-1 text-sm text-muted-foreground">
              Start the conversation!
            </Text>
          </View>
        ) : (
          <FlatList
            ref={flatListRef}
            data={messages}
            keyExtractor={(item) => item.id}
            renderItem={renderMessage}
            showsVerticalScrollIndicator={false}
            contentContainerClassName="py-4"
            onContentSizeChange={() => flatListRef.current?.scrollToEnd({ animated: true })}
          />
        )}
      </View>

      <View className="flex-row items-center gap-2 border-t border-border py-3">
        <TextInput
          value={message}
          onChangeText={setMessage}
          placeholder="Type a message..."
          placeholderTextColor="hsl(215.4 16.3% 46.9%)"
          className="flex-1 rounded-full border border-input bg-background px-4 py-2.5 text-foreground"
          returnKeyType="send"
          onSubmitEditing={handleSend}
          multiline
          maxLength={500}
        />
        <Pressable
          onPress={handleSend}
          disabled={!message.trim() || sendMessage.isPending}
          className={`h-10 w-10 items-center justify-center rounded-full ${
            message.trim() ? "bg-primary" : "bg-muted"
          }`}
        >
          <Text className={message.trim() ? "text-primary-foreground" : "text-muted-foreground"}>
            ➤
          </Text>
        </Pressable>
      </View>
    </KeyboardAvoidingView>
  );
}

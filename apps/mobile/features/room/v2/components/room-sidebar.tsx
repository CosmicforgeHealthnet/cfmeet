import { useState } from "react";
import { View, ScrollView, Pressable, TextInput } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import {
  useParticipants,
  useWaitingRoom,
  useAdmitParticipants,
  useDenyParticipants,
  useMessages,
  useSendMessage,
} from "@/features/meetings/v2";
import type { ParticipantResponse, MessageView, Role } from "@/features/meetings/v2";

interface RoomSidebarProps {
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

const ROLE_ICONS: Record<Role, { icon: string; color: string; label: string }> = {
  HOST: { icon: "star", color: "#f59e0b", label: "Host" },
  CO_HOST: { icon: "shield", color: "#3b82f6", label: "Co-Host" },
  PRESENTER: { icon: "easel", color: "#22c55e", label: "Presenter" },
  PARTICIPANT: { icon: "person", color: "#6b7280", label: "" },
  VIEWER: { icon: "eye", color: "#9ca3af", label: "Viewer" },
};

export function RoomSidebar({
  conferenceId,
  isHost,
  participantId,
  onClose,
}: RoomSidebarProps) {
  const [activeTab, setActiveTab] = useState<"participants" | "chat">("participants");
  const [newMessage, setNewMessage] = useState("");

  const { data: participantsData, isLoading: isLoadingParticipants } = useParticipants(conferenceId);
  const { data: waitingData } = useWaitingRoom(conferenceId, isHost);
  const { data: messagesData, isLoading: isLoadingMessages } = useMessages(conferenceId);
  const sendMessage = useSendMessage(conferenceId);

  const participants = participantsData?.participants || [];
  const waitingParticipants = waitingData?.participants || [];
  const messages = messagesData?.messages || [];

  const { toast } = useToast();
  const colors = useColors();

  const handleSendMessage = () => {
    if (!newMessage.trim()) return;
    sendMessage.mutate(
      { content: newMessage.trim() },
      {
        onSuccess: () => setNewMessage(""),
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  return (
    <View className="flex-1 bg-card dark:bg-card-dark">
      {/* Header */}
      <View className="flex-row items-center justify-between px-4 py-3 border-b border-border dark:border-border-dark">
        <View className="flex-row flex-1">
          <Pressable
            onPress={() => setActiveTab("participants")}
            className={`flex-1 items-center py-2 rounded-lg ${
              activeTab === "participants" ? "bg-primary/10 dark:bg-primary-dark/10" : ""
            }`}
          >
            <View className="flex-row items-center gap-1">
              <Ionicons
                name="people"
                size={18}
                color={activeTab === "participants" ? colors.primary : colors.mutedForeground}
              />
              <Text
                className={`text-sm font-medium ${
                  activeTab === "participants"
                    ? "text-primary dark:text-primary-dark"
                    : "text-muted-foreground dark:text-muted-foreground-dark"
                }`}
              >
                {participants.length}
              </Text>
            </View>
          </Pressable>
          <Pressable
            onPress={() => setActiveTab("chat")}
            className={`flex-1 items-center py-2 rounded-lg ${
              activeTab === "chat" ? "bg-primary/10 dark:bg-primary-dark/10" : ""
            }`}
          >
            <Ionicons
              name="chatbubble"
              size={18}
              color={activeTab === "chat" ? colors.primary : colors.mutedForeground}
            />
          </Pressable>
        </View>
        <Pressable onPress={onClose} className="p-2 rounded-full ml-2">
          <Ionicons name="close" size={20} color={colors.foreground} />
        </Pressable>
      </View>

      {/* Content */}
      {activeTab === "participants" ? (
        <ScrollView className="flex-1 px-4 py-4">
          {isLoadingParticipants ? (
            <Loading message="Loading participants..." />
          ) : (
            <View className="gap-4">
              {/* Waiting Room */}
              {isHost && waitingParticipants.length > 0 && (
                <WaitingRoomSection
                  conferenceId={conferenceId}
                  participants={waitingParticipants}
                />
              )}

              {/* Participants List */}
              <View>
                <Text className="text-sm font-bold text-foreground dark:text-foreground-dark mb-3">
                  Participants ({participants.length})
                </Text>
                <View className="gap-2">
                  {participants.map((participant) => (
                    <ParticipantItem
                      key={participant.id}
                      participant={participant}
                      isCurrentUser={participant.id === participantId}
                    />
                  ))}
                </View>
              </View>
            </View>
          )}
        </ScrollView>
      ) : (
        <View className="flex-1">
          {/* Messages */}
          <ScrollView className="flex-1 px-4 py-4">
            {isLoadingMessages ? (
              <Loading message="Loading messages..." />
            ) : messages.length === 0 ? (
              <View className="items-center py-10">
                <Ionicons name="chatbubble-outline" size={40} color={colors.mutedForeground} style={{ opacity: 0.5 }} />
                <Text className="mt-3 text-sm text-muted-foreground dark:text-muted-foreground-dark italic">
                  No messages yet
                </Text>
              </View>
            ) : (
              <View className="gap-3">
                {messages.map((msg) => (
                  <ChatMessage
                    key={msg.id}
                    message={msg}
                    isOwnMessage={msg.senderId === participantId}
                  />
                ))}
              </View>
            )}
          </ScrollView>

          {/* Input */}
          <View className="px-4 py-3 border-t border-border dark:border-border-dark">
            <View className="flex-row items-center gap-2">
              <TextInput
                placeholder="Type here..."
                value={newMessage}
                onChangeText={setNewMessage}
                onSubmitEditing={handleSendMessage}
                className="flex-1 h-10 bg-muted dark:bg-muted-dark rounded-full px-4 text-sm text-foreground dark:text-foreground-dark"
                placeholderTextColor="#9ca3af"
              />
              <Pressable
                onPress={handleSendMessage}
                disabled={!newMessage.trim() || sendMessage.isPending}
                className={`w-10 h-10 rounded-full items-center justify-center ${
                  !newMessage.trim() || sendMessage.isPending
                    ? "bg-muted dark:bg-muted-dark"
                    : "bg-primary dark:bg-primary-dark"
                }`}
              >
                <Ionicons
                  name="send"
                  size={18}
                  color={!newMessage.trim() || sendMessage.isPending ? colors.mutedForeground : "#fff"}
                />
              </Pressable>
            </View>
          </View>
        </View>
      )}
    </View>
  );
}

// Waiting Room Section
function WaitingRoomSection({
  conferenceId,
  participants,
}: {
  conferenceId: string;
  participants: ParticipantResponse[];
}) {
  const admitMutation = useAdmitParticipants(conferenceId);
  const denyMutation = useDenyParticipants(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

  const handleAdmit = (id: string) => {
    admitMutation.mutate(
      { participantIds: [id] },
      {
        onSuccess: () => success("Participant admitted"),
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  const handleDeny = (id: string) => {
    denyMutation.mutate(
      { participantIds: [id] },
      {
        onSuccess: () => success("Participant denied"),
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  const handleAdmitAll = () => {
    admitMutation.mutate(
      { all: true },
      {
        onSuccess: () => success("All participants admitted"),
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  return (
    <View className="bg-yellow-50 dark:bg-yellow-900/20 rounded-xl p-4">
      <View className="flex-row items-center justify-between mb-3">
        <View className="flex-row items-center gap-2">
          <Ionicons name="time" size={18} color="#ca8a04" />
          <Text className="text-sm font-bold text-yellow-700 dark:text-yellow-400">
            Waiting Room ({participants.length})
          </Text>
        </View>
        <Pressable
          onPress={handleAdmitAll}
          disabled={admitMutation.isPending}
        >
          <Text className="text-sm font-medium text-primary dark:text-primary-dark">
            Admit All
          </Text>
        </Pressable>
      </View>

      <View className="gap-2">
        {participants.map((participant) => (
          <View
            key={participant.id}
            className="flex-row items-center justify-between bg-card dark:bg-card-dark rounded-lg p-3"
          >
            <View className="flex-row items-center gap-2">
              <View className="w-8 h-8 rounded-full bg-primary/10 dark:bg-primary-dark/10 items-center justify-center">
                <Text className="text-sm font-bold text-primary dark:text-primary-dark">
                  {participant.displayName[0]}
                </Text>
              </View>
              <Text className="text-sm font-medium text-foreground dark:text-foreground-dark">
                {participant.displayName.length > 12
                  ? `${participant.displayName.slice(0, 12)}...`
                  : participant.displayName}
              </Text>
            </View>
            <View className="flex-row gap-2">
              <Pressable
                onPress={() => handleAdmit(participant.id)}
                disabled={admitMutation.isPending || denyMutation.isPending}
                className="w-8 h-8 rounded-full bg-primary dark:bg-primary-dark items-center justify-center"
              >
                <Ionicons name="checkmark" size={16} color="#fff" />
              </Pressable>
              <Pressable
                onPress={() => handleDeny(participant.id)}
                disabled={admitMutation.isPending || denyMutation.isPending}
                className="w-8 h-8 rounded-full bg-muted dark:bg-muted-dark items-center justify-center"
              >
                <Ionicons name="close" size={16} color={colors.mutedForeground} />
              </Pressable>
            </View>
          </View>
        ))}
      </View>
    </View>
  );
}

// Participant Item
function ParticipantItem({
  participant,
  isCurrentUser,
}: {
  participant: ParticipantResponse;
  isCurrentUser: boolean;
}) {
  const roleConfig = ROLE_ICONS[participant.role];

  return (
    <View className="flex-row items-center justify-between py-2">
      <View className="flex-row items-center gap-3">
        <View className="w-10 h-10 rounded-full bg-primary/10 dark:bg-primary-dark/10 items-center justify-center">
          <Text className="text-base font-bold text-primary dark:text-primary-dark">
            {participant.displayName[0]}
          </Text>
        </View>
        <View>
          <Text className="text-sm font-medium text-foreground dark:text-foreground-dark">
            {participant.displayName.length > 12
              ? `${participant.displayName.slice(0, 12)}...`
              : participant.displayName}
            {isCurrentUser && " (You)"}
          </Text>
          {roleConfig.label && (
            <View className="flex-row items-center gap-1 mt-0.5">
              <Ionicons name={roleConfig.icon as any} size={12} color={roleConfig.color} />
              <Text style={{ color: roleConfig.color }} className="text-xs font-medium">
                {roleConfig.label}
              </Text>
            </View>
          )}
        </View>
      </View>
    </View>
  );
}

// Chat Message
function ChatMessage({
  message,
  isOwnMessage,
}: {
  message: MessageView;
  isOwnMessage: boolean;
}) {
  return (
    <View className={`flex-col ${isOwnMessage ? "items-end" : "items-start"}`}>
      <Text className="text-[10px] text-muted-foreground dark:text-muted-foreground-dark font-bold px-1 mb-0.5">
        {message.senderDisplayName || "User"}
      </Text>
      <View
        className={`p-2.5 rounded-2xl max-w-[85%] ${
          isOwnMessage
            ? "bg-primary/10 dark:bg-primary-dark/10 rounded-tr-none"
            : "bg-muted dark:bg-muted-dark rounded-tl-none"
        }`}
      >
        <Text
          className={`text-sm ${
            isOwnMessage
              ? "text-primary dark:text-primary-dark"
              : "text-foreground dark:text-foreground-dark"
          }`}
        >
          {message.content}
        </Text>
      </View>
    </View>
  );
}

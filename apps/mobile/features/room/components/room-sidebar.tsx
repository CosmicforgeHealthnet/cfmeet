import { View, Pressable, Modal } from "react-native";
import { Text } from "@/components/ui/text";
import { useRoom } from "../providers/room-provider";
import { ParticipantsPanel } from "./participants-panel";
import { ChatPanel } from "./chat-panel";

export function RoomSidebar() {
  const { isSidebarOpen, activeSidebarTab, toggleSidebar, setSidebarTab } = useRoom();

  if (!isSidebarOpen) return null;

  return (
    <Modal
      visible={isSidebarOpen}
      transparent
      animationType="slide"
      onRequestClose={toggleSidebar}
      statusBarTranslucent
    >
      <Pressable onPress={toggleSidebar} className="flex-1 bg-black/50">
        <Pressable
          onPress={(e) => e.stopPropagation()}
          className="mt-auto h-[75%] rounded-t-3xl bg-background"
        >
          <View className="items-center py-3">
            <View className="h-1 w-10 rounded-full bg-muted" />
          </View>

          <View className="flex-row border-b border-border px-4">
            <Pressable
              onPress={() => setSidebarTab("participants")}
              className={`flex-1 items-center border-b-2 py-3 ${
                activeSidebarTab === "participants"
                  ? "border-primary"
                  : "border-transparent"
              }`}
            >
              <Text
                className={`font-medium ${
                  activeSidebarTab === "participants"
                    ? "text-primary"
                    : "text-muted-foreground"
                }`}
              >
                Participants
              </Text>
            </Pressable>
            <Pressable
              onPress={() => setSidebarTab("chat")}
              className={`flex-1 items-center border-b-2 py-3 ${
                activeSidebarTab === "chat"
                  ? "border-primary"
                  : "border-transparent"
              }`}
            >
              <Text
                className={`font-medium ${
                  activeSidebarTab === "chat"
                    ? "text-primary"
                    : "text-muted-foreground"
                }`}
              >
                Chat
              </Text>
            </Pressable>
          </View>

          <View className="flex-1 px-4 py-4">
            {activeSidebarTab === "participants" ? (
              <ParticipantsPanel />
            ) : (
              <ChatPanel />
            )}
          </View>
        </Pressable>
      </Pressable>
    </Modal>
  );
}

import { useState } from "react";
import { View, ScrollView, Pressable, Modal, TextInput } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import {
  useBreakouts,
  useCreateBreakouts,
  useAssignBreakouts,
  useOpenBreakout,
  useCloseAllBreakouts,
  useAnnounceBreakouts,
  useEnterBreakout,
  useLeaveBreakout,
} from "@/features/meetings/v2";
import type { BreakoutView, BreakoutStatus } from "@/features/meetings/v2";

interface BreakoutRoomsPanelProps {
  conferenceId: string;
  isHost: boolean;
  onClose: () => void;
}

const STATUS_COLORS: Record<BreakoutStatus, { bg: string; text: string }> = {
  PENDING: { bg: "bg-yellow-100 dark:bg-yellow-900/30", text: "text-yellow-700 dark:text-yellow-400" },
  OPEN: { bg: "bg-green-100 dark:bg-green-900/30", text: "text-green-700 dark:text-green-400" },
  CLOSING: { bg: "bg-orange-100 dark:bg-orange-900/30", text: "text-orange-700 dark:text-orange-400" },
  CLOSED: { bg: "bg-gray-100 dark:bg-gray-900/30", text: "text-gray-700 dark:text-gray-400" },
};

export function BreakoutRoomsPanel({
  conferenceId,
  isHost,
  onClose,
}: BreakoutRoomsPanelProps) {
  const { data, isLoading } = useBreakouts(conferenceId);
  const breakouts = data?.breakouts || [];
  const myContext = data?.myContext;

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [showAnnounceModal, setShowAnnounceModal] = useState(false);

  const closeAll = useCloseAllBreakouts(conferenceId);
  const enterBreakout = useEnterBreakout(conferenceId);
  const leaveBreakout = useLeaveBreakout(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

  const hasOpenRooms = breakouts.some((b) => b.status === "OPEN");
  const isInBreakout = myContext?.roomType === "BREAKOUT";

  const handleCloseAll = () => {
    closeAll.mutate({ countdownSeconds: 30 }, {
      onSuccess: () => success("Closing all breakout rooms"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  const handleEnterRoom = (breakoutId: string) => {
    enterBreakout.mutate(breakoutId, {
      onSuccess: () => success("Joined breakout room"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  const handleLeaveRoom = () => {
    leaveBreakout.mutate(undefined, {
      onSuccess: () => success("Left breakout room"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  return (
    <View className="flex-1 bg-card dark:bg-card-dark">
      {/* Header */}
      <View className="flex-row items-center justify-between px-4 py-3 border-b border-border dark:border-border-dark">
        <View className="flex-row items-center gap-2">
          <Ionicons name="people" size={20} color={colors.primary} />
          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark">
            Breakout Rooms
          </Text>
        </View>
        <Pressable onPress={onClose} className="p-2 rounded-full">
          <Ionicons name="close" size={20} color={colors.foreground} />
        </Pressable>
      </View>

      {/* Content */}
      <ScrollView className="flex-1 px-4 py-4">
        {isLoading ? (
          <Loading message="Loading breakout rooms..." />
        ) : breakouts.length === 0 ? (
          <View className="items-center py-10">
            <Ionicons name="people-outline" size={48} color={colors.mutedForeground} style={{ opacity: 0.5 }} />
            <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
              No breakout rooms
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
              {isHost
                ? "Create breakout rooms to split participants."
                : "No breakout rooms have been created yet."}
            </Text>
            {isHost && (
              <Button className="mt-4" onPress={() => setShowCreateModal(true)}>
                <Ionicons name="add" size={16} color="#fff" />
                <Text className="text-primary-foreground dark:text-primary-foreground-dark ml-1">
                  Create Rooms
                </Text>
              </Button>
            )}
          </View>
        ) : (
          <View className="gap-4">
            {/* Current room indicator */}
            {isInBreakout && myContext?.breakoutName && (
              <View className="bg-primary/10 dark:bg-primary-dark/10 rounded-xl p-4 flex-row items-center justify-between">
                <View>
                  <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
                    You are in:
                  </Text>
                  <Text className="font-bold text-primary dark:text-primary-dark">
                    {myContext.breakoutName}
                  </Text>
                </View>
                <Button
                  variant="outline"
                  size="sm"
                  onPress={handleLeaveRoom}
                  isLoading={leaveBreakout.isPending}
                >
                  Leave Room
                </Button>
              </View>
            )}

            {/* Room list */}
            {breakouts.map((room) => (
              <BreakoutRoomCard
                key={room.id}
                room={room}
                isHost={isHost}
                isCurrentRoom={myContext?.breakoutId === room.id}
                onEnter={() => handleEnterRoom(room.id)}
                conferenceId={conferenceId}
              />
            ))}
          </View>
        )}
      </ScrollView>

      {/* Footer Actions (Host only) */}
      {isHost && breakouts.length > 0 && (
        <View className="px-4 py-4 border-t border-border dark:border-border-dark gap-3">
          <View className="flex-row gap-2">
            {!hasOpenRooms && (
              <Button
                variant="outline"
                size="sm"
                className="flex-1"
                onPress={() => setShowCreateModal(true)}
              >
                <Ionicons name="add" size={16} color={colors.primary} />
                <Text className="text-primary dark:text-primary-dark ml-1">Add Rooms</Text>
              </Button>
            )}
            <Button
              variant="outline"
              size="sm"
              className="flex-1"
              onPress={() => setShowAnnounceModal(true)}
            >
              <Ionicons name="megaphone-outline" size={16} color={colors.primary} />
              <Text className="text-primary dark:text-primary-dark ml-1">Announce</Text>
            </Button>
          </View>
          {hasOpenRooms && (
            <Button
              variant="destructive"
              size="sm"
              onPress={handleCloseAll}
              isLoading={closeAll.isPending}
            >
              <Ionicons name="stop" size={16} color="#fff" />
              <Text className="text-white ml-1">Close All Rooms</Text>
            </Button>
          )}
        </View>
      )}

      {/* Modals */}
      {showCreateModal && (
        <CreateBreakoutsModal
          conferenceId={conferenceId}
          onClose={() => setShowCreateModal(false)}
        />
      )}
      {showAnnounceModal && (
        <AnnounceModal
          conferenceId={conferenceId}
          onClose={() => setShowAnnounceModal(false)}
        />
      )}
    </View>
  );
}

// Room Card Component
function BreakoutRoomCard({
  room,
  isHost,
  isCurrentRoom,
  onEnter,
  conferenceId,
}: {
  room: BreakoutView;
  isHost: boolean;
  isCurrentRoom: boolean;
  onEnter: () => void;
  conferenceId: string;
}) {
  const openBreakout = useOpenBreakout(conferenceId);
  const colors = useColors();
  const { toast, success } = useToast();

  const handleOpen = () => {
    openBreakout.mutate(room.id, {
      onSuccess: () => success("Breakout room opened"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  return (
    <View
      className={`rounded-xl border p-4 ${
        isCurrentRoom
          ? "border-primary dark:border-primary-dark bg-primary/5 dark:bg-primary-dark/5"
          : "border-border dark:border-border-dark"
      }`}
    >
      <View className="flex-row items-center justify-between mb-3">
        <View className="flex-row items-center gap-2 flex-1">
          <Text className="font-bold text-foreground dark:text-foreground-dark" numberOfLines={1}>
            {room.name}
          </Text>
          <View className={`px-2 py-0.5 rounded-full ${STATUS_COLORS[room.status].bg}`}>
            <Text className={`text-[10px] font-bold ${STATUS_COLORS[room.status].text}`}>
              {room.status}
            </Text>
          </View>
        </View>
        <View className="flex-row items-center gap-1">
          <Ionicons name="people-outline" size={14} color={colors.mutedForeground} />
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
            {room.participantCount}
          </Text>
        </View>
      </View>

      <View className="flex-row gap-2">
        {isHost && room.status === "PENDING" && (
          <Button
            size="sm"
            variant="outline"
            className="flex-1"
            onPress={handleOpen}
            isLoading={openBreakout.isPending}
          >
            <Ionicons name="play" size={14} color={colors.primary} />
            <Text className="text-primary dark:text-primary-dark ml-1">Open</Text>
          </Button>
        )}
        {room.status === "OPEN" && !isCurrentRoom && (
          <Button size="sm" className="flex-1" onPress={onEnter}>
            <Ionicons name="arrow-forward" size={14} color="#fff" />
            <Text className="text-white ml-1">Join</Text>
          </Button>
        )}
        {isCurrentRoom && (
          <Text className="text-sm font-bold text-primary dark:text-primary-dark">
            You are here
          </Text>
        )}
      </View>
    </View>
  );
}

// Create Modal
function CreateBreakoutsModal({
  conferenceId,
  onClose,
}: {
  conferenceId: string;
  onClose: () => void;
}) {
  const [roomCount, setRoomCount] = useState("3");
  const [autoOpen, setAutoOpen] = useState(false);
  const createBreakouts = useCreateBreakouts(conferenceId);
  const assignBreakouts = useAssignBreakouts(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

  const handleCreate = () => {
    createBreakouts.mutate(
      { count: parseInt(roomCount) || 3, autoOpen },
      {
        onSuccess: () => {
          success("Breakout rooms created");
          onClose();
        },
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  const handleRandomAssign = () => {
    assignBreakouts.mutate(
      { mode: "RANDOM" },
      {
        onSuccess: () => {
          success("Participants randomly assigned");
          onClose();
        },
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  return (
    <Modal visible animationType="slide" transparent onRequestClose={onClose}>
      <View className="flex-1 bg-black/50 justify-end">
        <View className="bg-card dark:bg-card-dark rounded-t-3xl p-6">
          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark mb-4">
            Create Breakout Rooms
          </Text>

          <Input
            label="Number of Rooms"
            keyboardType="number-pad"
            value={roomCount}
            onChangeText={setRoomCount}
          />

          <Pressable
            onPress={() => setAutoOpen(!autoOpen)}
            className="flex-row items-center justify-between py-4 mt-2"
          >
            <Text className="text-foreground dark:text-foreground-dark">
              Open rooms immediately
            </Text>
            <View
              className={`w-11 h-6 rounded-full p-0.5 ${
                autoOpen ? "bg-primary dark:bg-primary-dark" : "bg-muted dark:bg-muted-dark"
              }`}
            >
              <View
                className={`h-5 w-5 rounded-full bg-white ${autoOpen ? "ml-5" : "ml-0"}`}
              />
            </View>
          </Pressable>

          <View className="flex-row gap-3 mt-4">
            <Button variant="outline" className="flex-1" onPress={onClose}>
              Cancel
            </Button>
            <Button
              className="flex-1"
              onPress={handleCreate}
              isLoading={createBreakouts.isPending}
            >
              Create
            </Button>
          </View>

          <View className="border-t border-border dark:border-border-dark mt-4 pt-4">
            <Button
              variant="outline"
              onPress={handleRandomAssign}
              isLoading={assignBreakouts.isPending}
            >
              <Ionicons name="shuffle" size={16} color={colors.primary} />
              <Text className="text-primary dark:text-primary-dark ml-2">
                Randomly Assign Participants
              </Text>
            </Button>
          </View>
        </View>
      </View>
    </Modal>
  );
}

// Announce Modal
function AnnounceModal({
  conferenceId,
  onClose,
}: {
  conferenceId: string;
  onClose: () => void;
}) {
  const [message, setMessage] = useState("");
  const announceAll = useAnnounceBreakouts(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

  const handleAnnounce = () => {
    if (!message.trim()) return;
    announceAll.mutate(
      { message: message.trim() },
      {
        onSuccess: () => {
          success("Announcement sent to all rooms");
          onClose();
        },
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  return (
    <Modal visible animationType="slide" transparent onRequestClose={onClose}>
      <View className="flex-1 bg-black/50 justify-end">
        <View className="bg-card dark:bg-card-dark rounded-t-3xl p-6">
          <View className="items-center mb-4">
            <View className="w-14 h-14 rounded-full bg-primary/10 dark:bg-primary-dark/10 items-center justify-center">
              <Ionicons name="megaphone" size={28} color={colors.primary} />
            </View>
          </View>

          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark text-center mb-4">
            Announce to All Rooms
          </Text>

          <TextInput
            placeholder="Type your announcement..."
            value={message}
            onChangeText={setMessage}
            multiline
            numberOfLines={3}
            className="w-full rounded-xl border border-border dark:border-border-dark bg-transparent px-4 py-3 text-sm text-foreground dark:text-foreground-dark"
            placeholderTextColor="#9ca3af"
          />

          <View className="flex-row gap-3 mt-4">
            <Button variant="outline" className="flex-1" onPress={onClose}>
              Cancel
            </Button>
            <Button
              className="flex-1"
              onPress={handleAnnounce}
              disabled={!message.trim()}
              isLoading={announceAll.isPending}
            >
              Send
            </Button>
          </View>
        </View>
      </View>
    </Modal>
  );
}

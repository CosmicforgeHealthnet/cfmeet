import { useState } from "react";
import { View, Modal, Pressable } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { useDeleteMeeting } from "../hooks/use-meetings";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";

interface DeleteMeetingModalProps {
  meetingId: string;
  onClose: () => void;
}

export function DeleteMeetingModal({ meetingId, onClose }: DeleteMeetingModalProps) {
  const [error, setError] = useState("");
  const deleteMeeting = useDeleteMeeting();
  const { success } = useToast();
  const colors = useColors();

  const handleDelete = () => {
    setError("");
    deleteMeeting.mutate(meetingId, {
      onSuccess: () => {
        success("Meeting deleted");
        onClose();
      },
      onError: (err) => {
        setError(err.message || "Failed to delete meeting");
      },
    });
  };

  return (
    <Modal transparent visible animationType="fade" onRequestClose={onClose}>
      <Pressable className="flex-1 bg-black/40 items-center justify-center" onPress={onClose}>
        <Pressable className="bg-card dark:bg-card-dark rounded-2xl w-[90%] max-w-sm p-6">
          <View className="items-center mb-4">
            <View className="w-12 h-12 rounded-full bg-destructive/10 dark:bg-destructive-dark/10 items-center justify-center">
              <Ionicons name="trash-outline" size={24} color={colors.destructive} />
            </View>
          </View>

          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark text-center mb-1">
            Delete Meeting
          </Text>
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark text-center mb-6">
            Are you sure? This action cannot be undone.
          </Text>

          {error ? (
            <Text className="text-sm text-destructive dark:text-destructive-dark text-center mb-4">
              {error}
            </Text>
          ) : null}

          <View className="flex-row gap-3">
            <Button variant="outline" className="flex-1" onPress={onClose}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              className="flex-1"
              onPress={handleDelete}
              isLoading={deleteMeeting.isPending}
            >
              Delete
            </Button>
          </View>
        </Pressable>
      </Pressable>
    </Modal>
  );
}

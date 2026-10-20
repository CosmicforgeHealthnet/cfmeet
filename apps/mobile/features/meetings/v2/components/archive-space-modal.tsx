import { View, Modal, Pressable } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import { useArchiveSpace } from "../hooks";

interface ArchiveSpaceModalProps {
  spaceId: string;
  onClose: () => void;
}

export function ArchiveSpaceModal({ spaceId, onClose }: ArchiveSpaceModalProps) {
  const { success, toast } = useToast();
  const colors = useColors();
  const archiveSpace = useArchiveSpace();

  const handleArchive = () => {
    archiveSpace.mutate(spaceId, {
      onSuccess: () => {
        success("Meeting space archived successfully");
        onClose();
      },
      onError: (error) => {
        toast(error.message, "destructive");
      },
    });
  };

  return (
    <Modal visible animationType="fade" transparent onRequestClose={onClose}>
      <View className="flex-1 bg-black/50 items-center justify-center px-6">
        <View className="w-full bg-card dark:bg-card-dark rounded-2xl p-6">
          <View className="items-center mb-4">
            <View className="w-14 h-14 rounded-full bg-destructive/10 dark:bg-destructive-dark/10 items-center justify-center">
              <Ionicons name="archive-outline" size={28} color={colors.destructive} />
            </View>
          </View>

          <Text className="text-xl font-bold text-foreground dark:text-foreground-dark text-center">
            Archive Meeting Space
          </Text>
          <Text className="mt-2 text-center text-muted-foreground dark:text-muted-foreground-dark">
            Are you sure you want to archive this meeting space? You can restore it later if needed.
          </Text>

          <View className="flex-row gap-3 mt-6">
            <Button
              variant="outline"
              onPress={onClose}
              className="flex-1"
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onPress={handleArchive}
              isLoading={archiveSpace.isPending}
              className="flex-1"
            >
              Archive
            </Button>
          </View>
        </View>
      </View>
    </Modal>
  );
}

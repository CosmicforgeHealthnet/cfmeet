import { Modal as RNModal, View, Pressable, KeyboardAvoidingView, Platform } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "./text";

interface ModalProps {
  visible: boolean;
  onClose: () => void;
  title?: string;
  children: React.ReactNode;
  className?: string;
}

export function Modal({
  visible,
  onClose,
  title,
  children,
  className,
}: ModalProps) {
  return (
    <RNModal
      visible={visible}
      transparent
      animationType="fade"
      onRequestClose={onClose}
      statusBarTranslucent
    >
      <KeyboardAvoidingView
        behavior={Platform.OS === "ios" ? "padding" : "height"}
        className="flex-1"
      >
        <Pressable
          onPress={onClose}
          className="flex-1 items-center justify-center bg-black/50 px-4"
        >
          <Pressable
            onPress={(e) => e.stopPropagation()}
            className={cn(
              "w-full max-w-md rounded-2xl bg-background dark:bg-background-dark p-6",
              className
            )}
          >
            {title && (
              <Text className="mb-4 text-xl font-bold text-foreground dark:text-foreground-dark">
                {title}
              </Text>
            )}
            {children}
          </Pressable>
        </Pressable>
      </KeyboardAvoidingView>
    </RNModal>
  );
}

interface BottomSheetProps {
  visible: boolean;
  onClose: () => void;
  title?: string;
  children: React.ReactNode;
  snapPoints?: string[];
}

export function BottomSheet({
  visible,
  onClose,
  title,
  children,
}: BottomSheetProps) {
  return (
    <RNModal
      visible={visible}
      transparent
      animationType="slide"
      onRequestClose={onClose}
      statusBarTranslucent
    >
      <Pressable onPress={onClose} className="flex-1 bg-black/50">
        <Pressable
          onPress={(e) => e.stopPropagation()}
          className="mt-auto max-h-[85%] rounded-t-3xl bg-background dark:bg-background-dark"
        >
          <View className="items-center py-3">
            <View className="h-1 w-10 rounded-full bg-muted dark:bg-muted-dark" />
          </View>
          {title && (
            <View className="border-b border-border dark:border-border-dark px-6 pb-4">
              <Text className="text-lg font-semibold text-foreground dark:text-foreground-dark">
                {title}
              </Text>
            </View>
          )}
          <View className="p-6">{children}</View>
        </Pressable>
      </Pressable>
    </RNModal>
  );
}

import { KeyboardAvoidingView, Platform, ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { cn } from "@/lib/utils/cn";

interface KeyboardAwareViewProps {
  children: React.ReactNode;
  className?: string;
  contentClassName?: string;
  scrollable?: boolean;
  safe?: boolean;
  edges?: ("top" | "bottom" | "left" | "right")[];
  transparent?: boolean;
}

export function KeyboardAwareView({
  children,
  className,
  contentClassName,
  scrollable = true,
  safe = true,
  edges = ["top", "bottom"],
  transparent = false,
}: KeyboardAwareViewProps) {
  const Container = safe ? SafeAreaView : View;

  return (
    <Container edges={edges} className={cn("flex-1", !transparent && "bg-background", className)}>
      <KeyboardAvoidingView
        behavior={Platform.OS === "ios" ? "padding" : "height"}
        className="flex-1"
        keyboardVerticalOffset={Platform.OS === "ios" ? 0 : 20}
      >
        {scrollable ? (
          <ScrollView
            showsVerticalScrollIndicator={false}
            contentContainerStyle={{ flexGrow: 1 }}
            contentContainerClassName={contentClassName}
            keyboardShouldPersistTaps="handled"
          >
            {children}
          </ScrollView>
        ) : (
          <View className={cn("flex-1", contentClassName)}>{children}</View>
        )}
      </KeyboardAvoidingView>
    </Container>
  );
}

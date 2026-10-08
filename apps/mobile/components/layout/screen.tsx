import { View, ViewProps, ScrollView, ScrollViewProps, RefreshControl } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { cn } from "@/lib/utils/cn";

interface ScreenProps extends ViewProps {
  safe?: boolean;
  edges?: ("top" | "bottom" | "left" | "right")[];
}

export function Screen({
  safe = true,
  edges = ["top", "bottom"],
  className,
  children,
  ...props
}: ScreenProps) {
  const Container = safe ? SafeAreaView : View;

  return (
    <Container
      edges={edges}
      className={cn("flex-1 bg-background dark:bg-background-dark", className)}
      {...props}
    >
      {children}
    </Container>
  );
}

interface ScrollScreenProps extends ScrollViewProps {
  safe?: boolean;
  edges?: ("top" | "bottom" | "left" | "right")[];
  refreshing?: boolean;
  onRefresh?: () => void;
}

export function ScrollScreen({
  safe = true,
  edges = ["top", "bottom"],
  refreshing,
  onRefresh,
  className,
  contentContainerClassName,
  children,
  ...props
}: ScrollScreenProps & { contentContainerClassName?: string }) {
  const Container = safe ? SafeAreaView : View;

  return (
    <Container edges={edges} className={cn("flex-1 bg-background dark:bg-background-dark", className)}>
      <ScrollView
        showsVerticalScrollIndicator={false}
        contentContainerStyle={{ flexGrow: 1 }}
        contentContainerClassName={contentContainerClassName}
        refreshControl={
          onRefresh ? (
            <RefreshControl refreshing={refreshing ?? false} onRefresh={onRefresh} />
          ) : undefined
        }
        {...props}
      >
        {children}
      </ScrollView>
    </Container>
  );
}

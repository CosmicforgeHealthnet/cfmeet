import { View, ActivityIndicator } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "@/components/ui/text";
import { Logo } from "@/components/ui/logo";

interface LoadingProps {
  message?: string;
  size?: "small" | "large";
  className?: string;
  fullScreen?: boolean;
}

export function Loading({
  message,
  size = "large",
  className,
  fullScreen = false,
}: LoadingProps) {
  return (
    <View
      className={cn(
        "items-center justify-center",
        fullScreen && "flex-1 bg-background",
        className
      )}
    >
      <ActivityIndicator size={size} color="hsl(221.2 83.2% 53.3%)" />
      {message && (
        <Text className="mt-3 text-muted-foreground">{message}</Text>
      )}
    </View>
  );
}

export function FullScreenLoading({ message }: { message?: string }) {
  return (
    <View className="flex-1 items-center justify-center bg-background dark:bg-background-dark">
      <Logo variant="large" className="mb-8" />
      <ActivityIndicator size="large" color="hsl(221.2 83.2% 53.3%)" />
      {message && (
        <Text className="mt-3 text-muted-foreground dark:text-muted-foreground-dark">{message}</Text>
      )}
    </View>
  );
}

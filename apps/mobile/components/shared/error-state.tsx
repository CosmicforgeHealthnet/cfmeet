import { View } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";

interface ErrorStateProps {
  title?: string;
  message?: string;
  onRetry?: () => void;
  className?: string;
}

export function ErrorState({
  title = "Something went wrong",
  message = "An error occurred. Please try again.",
  onRetry,
  className,
}: ErrorStateProps) {
  return (
    <View className={cn("flex-1 items-center justify-center p-6", className)}>
      <View className="mb-4 h-16 w-16 items-center justify-center rounded-full bg-destructive/10">
        <Text className="text-3xl">!</Text>
      </View>
      <Text className="text-center text-lg font-semibold text-foreground">
        {title}
      </Text>
      <Text className="mt-2 text-center text-muted-foreground">{message}</Text>
      {onRetry && (
        <Button onPress={onRetry} className="mt-6">
          Try Again
        </Button>
      )}
    </View>
  );
}

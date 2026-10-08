import { View } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";

interface EmptyStateProps {
  icon?: React.ReactNode;
  title: string;
  description?: string;
  actionLabel?: string;
  onAction?: () => void;
  className?: string;
}

export function EmptyState({
  icon,
  title,
  description,
  actionLabel,
  onAction,
  className,
}: EmptyStateProps) {
  return (
    <View className={cn("flex-1 items-center justify-center p-6", className)}>
      {icon && <View className="mb-4">{icon}</View>}
      <Text className="text-center text-lg font-semibold text-foreground">
        {title}
      </Text>
      {description && (
        <Text className="mt-2 text-center text-muted-foreground">
          {description}
        </Text>
      )}
      {actionLabel && onAction && (
        <Button onPress={onAction} className="mt-6">
          {actionLabel}
        </Button>
      )}
    </View>
  );
}

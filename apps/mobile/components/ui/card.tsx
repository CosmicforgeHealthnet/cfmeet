import { View, ViewProps, Pressable, PressableProps } from "react-native";
import { cn } from "@/lib/utils/cn";

interface CardProps extends ViewProps {
  variant?: "default" | "outline";
}

export function Card({ variant = "default", className, ...props }: CardProps) {
  return (
    <View
      className={cn(
        "rounded-xl p-4",
        variant === "default"
          ? "bg-card dark:bg-card-dark"
          : "border border-border dark:border-border-dark bg-transparent",
        className
      )}
      {...props}
    />
  );
}

interface PressableCardProps extends PressableProps {
  variant?: "default" | "outline";
}

export function PressableCard({
  variant = "default",
  className,
  ...props
}: PressableCardProps) {
  return (
    <Pressable
      className={cn(
        "rounded-xl p-4",
        variant === "default"
          ? "bg-card dark:bg-card-dark"
          : "border border-border dark:border-border-dark bg-transparent",
        className
      )}
      {...props}
    />
  );
}

export function CardHeader({ className, ...props }: ViewProps) {
  return <View className={cn("mb-3", className)} {...props} />;
}

export function CardTitle({ className, ...props }: ViewProps) {
  return <View className={cn("mb-1", className)} {...props} />;
}

export function CardDescription({ className, ...props }: ViewProps) {
  return <View className={cn("", className)} {...props} />;
}

export function CardContent({ className, ...props }: ViewProps) {
  return <View className={cn("", className)} {...props} />;
}

export function CardFooter({ className, ...props }: ViewProps) {
  return (
    <View className={cn("mt-3 flex-row items-center", className)} {...props} />
  );
}

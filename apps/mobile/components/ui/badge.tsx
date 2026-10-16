import { View, ViewProps } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "./text";

interface BadgeProps extends ViewProps {
  variant?: "default" | "secondary" | "success" | "destructive" | "outline";
  children: string;
}

const variantStyles: Record<NonNullable<BadgeProps["variant"]>, string> = {
  default: "bg-primary",
  secondary: "bg-secondary",
  success: "bg-green-500",
  destructive: "bg-destructive",
  outline: "border border-border bg-transparent",
};

const textStyles: Record<NonNullable<BadgeProps["variant"]>, string> = {
  default: "text-primary-foreground",
  secondary: "text-secondary-foreground",
  success: "text-white",
  destructive: "text-destructive-foreground",
  outline: "text-foreground",
};

export function Badge({
  variant = "default",
  children,
  className,
  ...props
}: BadgeProps) {
  return (
    <View
      className={cn(
        "rounded-full px-2.5 py-0.5",
        variantStyles[variant],
        className
      )}
      {...props}
    >
      <Text className={cn("text-xs font-medium", textStyles[variant])}>
        {children}
      </Text>
    </View>
  );
}

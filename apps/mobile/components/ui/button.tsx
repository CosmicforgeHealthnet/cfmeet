import { Pressable, PressableProps, ActivityIndicator } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "./text";

interface ButtonProps extends Omit<PressableProps, "children"> {
  variant?: "default" | "secondary" | "destructive" | "outline" | "ghost";
  size?: "sm" | "default" | "lg" | "icon";
  isLoading?: boolean;
  children: React.ReactNode;
}

const variantStyles: Record<NonNullable<ButtonProps["variant"]>, string> = {
  default: "bg-primary dark:bg-primary-dark",
  secondary: "bg-secondary dark:bg-secondary-dark",
  destructive: "bg-destructive dark:bg-destructive-dark",
  outline: "border border-border dark:border-border-dark bg-transparent",
  ghost: "bg-transparent",
};

const variantTextStyles: Record<NonNullable<ButtonProps["variant"]>, string> = {
  default: "text-primary-foreground dark:text-primary-foreground-dark",
  secondary: "text-secondary-foreground dark:text-secondary-foreground-dark",
  destructive: "text-destructive-foreground dark:text-destructive-foreground-dark",
  outline: "text-foreground dark:text-foreground-dark",
  ghost: "text-foreground dark:text-foreground-dark",
};

const sizeStyles: Record<NonNullable<ButtonProps["size"]>, string> = {
  sm: "px-3 py-1.5 rounded-md",
  default: "px-4 py-2.5 rounded-lg",
  lg: "px-6 py-3.5 rounded-xl",
  icon: "p-2.5 rounded-lg",
};

const sizeTextStyles: Record<NonNullable<ButtonProps["size"]>, string> = {
  sm: "text-sm",
  default: "text-base",
  lg: "text-lg",
  icon: "text-base",
};

export function Button({
  variant = "default",
  size = "default",
  isLoading = false,
  disabled,
  className,
  children,
  ...props
}: ButtonProps) {
  const isDisabled = disabled || isLoading;

  return (
    <Pressable
      className={cn(
        "flex-row items-center justify-center",
        variantStyles[variant],
        sizeStyles[size],
        isDisabled && "opacity-50",
        className
      )}
      disabled={isDisabled}
      {...props}
    >
      {isLoading ? (
        <ActivityIndicator
          size="small"
          color={variant === "default" || variant === "destructive" ? "#fff" : "#000"}
        />
      ) : typeof children === "string" ? (
        <Text
          className={cn(
            "font-bold text-center",
            variantTextStyles[variant],
            sizeTextStyles[size]
          )}
        >
          {children}
        </Text>
      ) : (
        children
      )}
    </Pressable>
  );
}

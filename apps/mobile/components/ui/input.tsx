import { forwardRef } from "react";
import { TextInput, TextInputProps, View } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "./text";

interface InputProps extends TextInputProps {
  label?: string;
  error?: string;
  leftIcon?: React.ReactNode;
  rightIcon?: React.ReactNode;
}

export const Input = forwardRef<TextInput, InputProps>(
  ({ label, error, leftIcon, rightIcon, className, ...props }, ref) => {
    return (
      <View className="w-full">
        {label && (
          <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
            {label}
          </Text>
        )}
        <View
          className={cn(
            "flex-row items-center rounded-lg border bg-background dark:bg-background-dark px-3",
            error ? "border-destructive dark:border-destructive-dark" : "border-input dark:border-input-dark",
            props.editable === false && "opacity-50"
          )}
        >
          {leftIcon && <View className="mr-2">{leftIcon}</View>}
          <TextInput
            ref={ref}
            className={cn(
              "flex-1 py-3 text-base text-foreground dark:text-foreground-dark",
              className
            )}
            placeholderTextColor="#737373"
            {...props}
          />
          {rightIcon && <View className="ml-2">{rightIcon}</View>}
        </View>
        {error && (
          <Text className="mt-1 text-sm text-destructive dark:text-destructive-dark">{error}</Text>
        )}
      </View>
    );
  }
);

Input.displayName = "Input";

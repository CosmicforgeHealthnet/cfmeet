import { forwardRef, useState } from "react";
import { TextInput, TextInputProps, View, Pressable } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { cn } from "@/lib/utils/cn";
import { Text } from "@/components/ui/text";

interface AuthInputProps extends TextInputProps {
  label: string;
  error?: string;
}

export const AuthInput = forwardRef<TextInput, AuthInputProps>(
  ({ label, error, secureTextEntry, className, ...props }, ref) => {
    const [showPassword, setShowPassword] = useState(false);
    const isPassword = secureTextEntry !== undefined;

    return (
      <View className="w-full">
        <Text className="mb-1.5 text-xs font-medium text-foreground dark:text-foreground-dark">
          {label}
        </Text>
        <View
          className={cn(
            "flex-row items-center rounded-lg border bg-background dark:bg-background-dark px-3",
            error
              ? "border-destructive dark:border-destructive-dark"
              : "border-input dark:border-input-dark"
          )}
        >
          <TextInput
            ref={ref}
            className={cn(
              "flex-1 py-3 text-base text-foreground dark:text-foreground-dark",
              isPassword && "pr-10",
              className
            )}
            placeholderTextColor="#737373"
            secureTextEntry={isPassword && !showPassword}
            {...props}
          />
          {isPassword && (
            <Pressable
              onPress={() => setShowPassword(!showPassword)}
              className="absolute right-3"
            >
              <Ionicons
                name={showPassword ? "eye-off-outline" : "eye-outline"}
                size={20}
                color="#029CD4"
                style={{ opacity: 0.7 }}
              />
            </Pressable>
          )}
        </View>
        {error && (
          <Text className="mt-1 text-xs text-destructive dark:text-destructive-dark">
            {error}
          </Text>
        )}
      </View>
    );
  }
);

AuthInput.displayName = "AuthInput";

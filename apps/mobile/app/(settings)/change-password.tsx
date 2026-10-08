import { useState } from "react";
import { View, ScrollView, Pressable } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { useChangePassword } from "@/features/auth/hooks/use-change-password";

export default function ChangePasswordScreen() {
  const changePassword = useChangePassword();
  const { toast, success } = useToast();

  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [showCurrentPassword, setShowCurrentPassword] = useState(false);
  const [showNewPassword, setShowNewPassword] = useState(false);
  const [showConfirmPassword, setShowConfirmPassword] = useState(false);

  const validatePassword = (password: string): string | null => {
    if (password.length < 8) {
      return "Password must be at least 8 characters";
    }
    if (!/[A-Z]/.test(password)) {
      return "Password must contain at least one uppercase letter";
    }
    if (!/[a-z]/.test(password)) {
      return "Password must contain at least one lowercase letter";
    }
    if (!/[0-9]/.test(password)) {
      return "Password must contain at least one number";
    }
    if (!/[!@#$%^&*(),.?:{}|<>]/.test(password)) {
      return "Password must contain at least one special character";
    }
    return null;
  };

  const handleSubmit = () => {
    if (!currentPassword) {
      toast("Current password is required", "destructive");
      return;
    }

    const passwordError = validatePassword(newPassword);
    if (passwordError) {
      toast(passwordError, "destructive");
      return;
    }

    if (newPassword !== confirmPassword) {
      toast("Passwords do not match", "destructive");
      return;
    }

    if (currentPassword === newPassword) {
      toast("New password must be different from current password", "destructive");
      return;
    }

    changePassword.mutate(
      {
        current_password: currentPassword,
        new_password: newPassword,
        confirm_password: confirmPassword,
      },
      {
        onSuccess: () => {
          success("Password changed successfully");
          router.back();
        },
        onError: (error) => {
          toast(error.message, "destructive");
        },
      }
    );
  };

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      {/* Header */}
      <View className="flex-row items-center px-4 py-3 border-b border-border dark:border-border-dark">
        <Pressable onPress={() => router.back()} className="flex-row items-center gap-2">
          <Ionicons name="arrow-back" size={24} color="#029CD4" />
          <Text className="text-sm font-bold text-primary dark:text-primary-dark">Back</Text>
        </Pressable>
      </View>

      <ScrollView
        className="flex-1"
        contentContainerStyle={{ padding: 24, paddingBottom: 100 }}
        showsVerticalScrollIndicator={false}
        keyboardShouldPersistTaps="handled"
      >
        <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark mb-1">
          Change Password
        </Text>
        <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark mb-6">
          Enter your current password and choose a new one
        </Text>

        <View className="gap-4">
          {/* Current Password */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Current Password
            </Text>
            <View className="relative">
              <Input
                placeholder="Enter current password"
                value={currentPassword}
                onChangeText={setCurrentPassword}
                secureTextEntry={!showCurrentPassword}
                autoCapitalize="none"
              />
              <Pressable
                onPress={() => setShowCurrentPassword(!showCurrentPassword)}
                className="absolute right-3 top-3"
              >
                <Ionicons
                  name={showCurrentPassword ? "eye-off-outline" : "eye-outline"}
                  size={20}
                  color="#737373"
                />
              </Pressable>
            </View>
          </View>

          {/* New Password */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              New Password
            </Text>
            <View className="relative">
              <Input
                placeholder="Enter new password"
                value={newPassword}
                onChangeText={setNewPassword}
                secureTextEntry={!showNewPassword}
                autoCapitalize="none"
              />
              <Pressable
                onPress={() => setShowNewPassword(!showNewPassword)}
                className="absolute right-3 top-3"
              >
                <Ionicons
                  name={showNewPassword ? "eye-off-outline" : "eye-outline"}
                  size={20}
                  color="#737373"
                />
              </Pressable>
            </View>
            <Text className="mt-1 text-xs text-muted-foreground dark:text-muted-foreground-dark">
              8+ characters, uppercase, lowercase, number, special character
            </Text>
          </View>

          {/* Confirm Password */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Confirm New Password
            </Text>
            <View className="relative">
              <Input
                placeholder="Confirm new password"
                value={confirmPassword}
                onChangeText={setConfirmPassword}
                secureTextEntry={!showConfirmPassword}
                autoCapitalize="none"
              />
              <Pressable
                onPress={() => setShowConfirmPassword(!showConfirmPassword)}
                className="absolute right-3 top-3"
              >
                <Ionicons
                  name={showConfirmPassword ? "eye-off-outline" : "eye-outline"}
                  size={20}
                  color="#737373"
                />
              </Pressable>
            </View>
          </View>

          {/* Actions */}
          <View className="flex-row gap-3 mt-4">
            <Button
              onPress={handleSubmit}
              isLoading={changePassword.isPending}
              className="flex-1"
            >
              Change Password
            </Button>
            <Button variant="outline" onPress={() => router.back()}>
              Cancel
            </Button>
          </View>
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}

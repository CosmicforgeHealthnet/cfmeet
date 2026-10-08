import { useState, useEffect } from "react";
import { View, ScrollView, Pressable } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useUser, useUpdateUser } from "@/features/user/hooks/use-user";

export default function EditProfileScreen() {
  const { data: user, isLoading, isError } = useUser();
  const updateUser = useUpdateUser();
  const { toast, success } = useToast();

  const [username, setUsername] = useState("");
  const [isInitialized, setIsInitialized] = useState(false);

  useEffect(() => {
    if (user && !isInitialized) {
      setUsername(user.username);
      setIsInitialized(true);
    }
  }, [user, isInitialized]);

  const handleSubmit = () => {
    const trimmedUsername = username.trim();
    if (!trimmedUsername) {
      toast("Username cannot be empty", "destructive");
      return;
    }

    if (trimmedUsername.length < 3) {
      toast("Username must be at least 3 characters", "destructive");
      return;
    }

    if (!/^[a-zA-Z0-9_]+$/.test(trimmedUsername)) {
      toast("Username can only contain letters, numbers, and underscores", "destructive");
      return;
    }

    updateUser.mutate(
      { username: trimmedUsername },
      {
        onSuccess: () => {
          success("Profile updated successfully");
          router.back();
        },
        onError: (error) => {
          toast(error.message, "destructive");
        },
      }
    );
  };

  if (isLoading) {
    return <Loading fullScreen message="Loading profile..." />;
  }

  if (isError || !user) {
    return (
      <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark items-center justify-center px-6">
        <Ionicons name="alert-circle-outline" size={48} color="#dc2626" style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
          Failed to load profile
        </Text>
        <Button onPress={() => router.back()} className="mt-4" variant="outline">
          Go Back
        </Button>
      </SafeAreaView>
    );
  }

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
          Edit Profile
        </Text>
        <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark mb-6">
          Update your account information
        </Text>

        <View className="gap-4">
          {/* Email (read-only) */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Email Address
            </Text>
            <View className="rounded-lg border border-input dark:border-input-dark bg-muted dark:bg-muted-dark px-3 py-3">
              <Text className="text-muted-foreground dark:text-muted-foreground-dark">
                {user.email}
              </Text>
            </View>
            <Text className="mt-1 text-xs text-muted-foreground dark:text-muted-foreground-dark">
              Email cannot be changed
            </Text>
          </View>

          {/* Username */}
          <Input
            label="Username"
            placeholder="Enter your username"
            value={username}
            onChangeText={setUsername}
            autoCapitalize="none"
            autoCorrect={false}
          />

          {/* Role (read-only) */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Role
            </Text>
            <View className="rounded-lg border border-input dark:border-input-dark bg-muted dark:bg-muted-dark px-3 py-3">
              <Text className="text-muted-foreground dark:text-muted-foreground-dark capitalize">
                {user.role}
              </Text>
            </View>
          </View>

          {/* Status (read-only) */}
          <View>
            <Text className="mb-1.5 text-sm font-medium text-foreground dark:text-foreground-dark">
              Status
            </Text>
            <View className="rounded-lg border border-input dark:border-input-dark bg-muted dark:bg-muted-dark px-3 py-3">
              <Text className="text-muted-foreground dark:text-muted-foreground-dark capitalize">
                {user.status?.replace("_", " ")}
              </Text>
            </View>
          </View>

          {/* Actions */}
          <View className="flex-row gap-3 mt-4">
            <Button
              onPress={handleSubmit}
              isLoading={updateUser.isPending}
              disabled={username.trim() === user.username}
              className="flex-1"
            >
              Save Changes
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

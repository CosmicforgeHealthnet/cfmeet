import { useState } from "react";
import { View, ScrollView, Pressable, Modal } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Avatar } from "@/components/ui/avatar";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { useTheme } from "@/providers/theme-provider";
import { useAuthStore } from "@/store/auth.store";
import { useLogout } from "@/features/auth/hooks/use-logout";

type IoniconsName = React.ComponentProps<typeof Ionicons>["name"];

interface SettingsItemProps {
  icon: IoniconsName;
  label: string;
  value?: string;
  onPress?: () => void;
  color?: string;
}

function SettingsItem({ icon, label, value, onPress, color = "#029CD4" }: SettingsItemProps) {
  return (
    <Pressable
      onPress={onPress}
      className="flex-row items-center justify-between py-4"
    >
      <View className="flex-row items-center gap-3">
        <View className="w-8 h-8 rounded-lg bg-accent dark:bg-accent-dark items-center justify-center">
          <Ionicons name={icon} size={18} color={color} />
        </View>
        <Text className="text-foreground dark:text-foreground-dark font-semibold">{label}</Text>
      </View>
      <View className="flex-row items-center gap-2">
        {value && <Text className="text-muted-foreground dark:text-muted-foreground-dark">{value}</Text>}
        <Ionicons name="chevron-forward" size={18} color="#737373" />
      </View>
    </Pressable>
  );
}

interface LogoutModalProps {
  visible: boolean;
  onClose: () => void;
  onConfirm: () => void;
  isLoading: boolean;
}

function LogoutModal({ visible, onClose, onConfirm, isLoading }: LogoutModalProps) {
  return (
    <Modal transparent visible={visible} animationType="fade" onRequestClose={onClose}>
      <Pressable className="flex-1 bg-black/40 items-center justify-center" onPress={onClose}>
        <Pressable className="bg-card dark:bg-card-dark rounded-2xl w-[90%] max-w-sm p-6">
          <View className="items-center mb-4">
            <View className="w-12 h-12 rounded-full bg-destructive/10 dark:bg-destructive-dark/10 items-center justify-center">
              <Ionicons name="log-out-outline" size={24} color="#dc2626" />
            </View>
          </View>

          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark text-center mb-1">
            Sign Out
          </Text>
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark text-center mb-6">
            Are you sure you want to sign out of your account?
          </Text>

          <View className="flex-row gap-3">
            <Button variant="outline" className="flex-1" onPress={onClose} disabled={isLoading}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              className="flex-1"
              onPress={onConfirm}
              isLoading={isLoading}
            >
              Sign Out
            </Button>
          </View>
        </Pressable>
      </Pressable>
    </Modal>
  );
}

export default function SettingsScreen() {
  const { user } = useAuthStore();
  const { theme, setTheme } = useTheme();
  const logout = useLogout();
  const [showLogoutModal, setShowLogoutModal] = useState(false);

  const handleThemeChange = () => {
    const themes: Array<"system" | "light" | "dark"> = ["system", "light", "dark"];
    const currentIndex = themes.indexOf(theme);
    const nextIndex = (currentIndex + 1) % themes.length;
    setTheme(themes[nextIndex]);
  };

  const handleLogout = () => {
    logout.mutate(undefined, {
      onSettled: () => {
        setShowLogoutModal(false);
      },
    });
  };

  const capitalizedTheme = theme.charAt(0).toUpperCase() + theme.slice(1);

  return (
    <SafeAreaView edges={["top"]} className="flex-1 bg-background dark:bg-background-dark">
      <View className="border-b border-border dark:border-border-dark px-6 py-4">
        <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">Settings</Text>
        <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark mt-1">
          Manage your account settings
        </Text>
      </View>

      <ScrollView className="flex-1" showsVerticalScrollIndicator={false} contentContainerStyle={{ paddingBottom: 100 }}>
        <View className="p-6">
          <Card className="mb-6">
            <View className="flex-row items-center gap-4">
              <Avatar fallback={user?.username ?? "U"} size="lg" />
              <View className="flex-1">
                <Text className="text-lg font-bold text-foreground dark:text-foreground-dark">
                  {user?.username}
                </Text>
                <Text className="text-muted-foreground dark:text-muted-foreground-dark">{user?.email}</Text>
              </View>
              <View className="bg-accent dark:bg-accent-dark px-3 py-1 rounded-full">
                <Text className="text-xs font-semibold text-primary dark:text-primary-dark capitalize">
                  {user?.role ?? "user"}
                </Text>
              </View>
            </View>
          </Card>

          <Card className="mb-6">
            <Text className="mb-2 text-sm font-bold text-muted-foreground dark:text-muted-foreground-dark uppercase tracking-wider">
              Appearance
            </Text>
            <SettingsItem
              icon="color-palette-outline"
              label="Theme"
              value={capitalizedTheme}
              onPress={handleThemeChange}
            />
          </Card>

          <Card className="mb-6">
            <Text className="mb-2 text-sm font-bold text-muted-foreground dark:text-muted-foreground-dark uppercase tracking-wider">
              Account
            </Text>
            <SettingsItem
              icon="person-outline"
              label="Edit Profile"
              onPress={() => router.push("/(settings)/edit-profile")}
            />
            <View className="border-t border-border dark:border-border-dark" />
            <SettingsItem
              icon="lock-closed-outline"
              label="Change Password"
              onPress={() => router.push("/(settings)/change-password")}
            />
          </Card>

          <Card className="mb-6">
            <Text className="mb-2 text-sm font-bold text-muted-foreground dark:text-muted-foreground-dark uppercase tracking-wider">
              Support
            </Text>
            <SettingsItem icon="help-circle-outline" label="Help Center" />
            <View className="border-t border-border dark:border-border-dark" />
            <SettingsItem icon="document-text-outline" label="Terms of Service" />
            <View className="border-t border-border dark:border-border-dark" />
            <SettingsItem icon="shield-checkmark-outline" label="Privacy Policy" />
          </Card>

          <Card className="mb-6 bg-destructive/10 dark:bg-destructive-dark/10 border-destructive/20 dark:border-destructive-dark/20">
            <Text className="mb-2 text-sm font-bold text-destructive dark:text-destructive-dark uppercase tracking-wider">
              Danger Zone
            </Text>
            <Pressable
              onPress={() => setShowLogoutModal(true)}
              className="flex-row items-center justify-between py-4"
            >
              <View className="flex-row items-center gap-3">
                <View className="w-8 h-8 rounded-lg bg-destructive/20 dark:bg-destructive-dark/20 items-center justify-center">
                  <Ionicons name="log-out-outline" size={18} color="#dc2626" />
                </View>
                <Text className="text-destructive dark:text-destructive-dark font-semibold">Sign Out</Text>
              </View>
              <Ionicons name="chevron-forward" size={18} color="#dc2626" />
            </Pressable>
          </Card>
        </View>
      </ScrollView>

      <LogoutModal
        visible={showLogoutModal}
        onClose={() => setShowLogoutModal(false)}
        onConfirm={handleLogout}
        isLoading={logout.isPending}
      />
    </SafeAreaView>
  );
}

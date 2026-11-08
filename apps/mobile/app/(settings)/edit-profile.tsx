/** Profile (`GET/PATCH /api/v2/me/profile`). */
import { useEffect, useState } from "react";
import { ScrollView, View } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { ScreenHeader } from "@/components/shared/screen-header";
import { useToast } from "@/components/ui/toast";
import { useProfile, useUpdateProfile } from "@/features/account/hooks";
import { useAuthStore } from "@/store/auth.store";
import { userStorage } from "@/lib/storage/secure-store";
import { decodeApiError } from "@/lib/api/errors";
import { deviceTimeZone, isValidTimeZone } from "@/lib/time/zoned";

export default function EditProfileScreen() {
  const profile = useProfile();
  const update = useUpdateProfile();
  const { user, setUser } = useAuthStore();
  const { success } = useToast();
  const [displayName, setDisplayName] = useState("");
  const [username, setUsername] = useState("");
  const [timeZone, setTimeZone] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (profile.data) {
      setDisplayName(profile.data.displayName ?? "");
      setUsername(profile.data.username);
      setTimeZone(profile.data.timezone ?? "");
    }
  }, [profile.data]);

  if (profile.isLoading) return <Loading fullScreen />;
  if (profile.isError || !profile.data) {
    return (
      <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
        <ScreenHeader title="Profile" />
        <ErrorState message={decodeApiError(profile.error).message} onRetry={() => void profile.refetch()} />
      </SafeAreaView>
    );
  }
  const p = profile.data;

  const save = () => {
    setError(null);
    const u = username.trim();
    if (u.length < 3 || !/^[a-zA-Z0-9_]+$/.test(u)) return setError("Username: at least 3 letters, numbers or underscores.");
    if (timeZone.trim() && !isValidTimeZone(timeZone.trim())) return setError("Unknown time zone.");
    update.mutate(
      {
        displayName: displayName.trim() || null,
        username: u !== p.username ? u : undefined,
        timezone: timeZone.trim() || null,
      },
      {
        onSuccess: async (next) => {
          if (user) {
            const updated = { ...user, username: next.username };
            setUser(updated);
            await userStorage.setUserInfo(updated);
          }
          success("Profile updated");
          router.back();
        },
        onError: (e) => setError(decodeApiError(e).message),
      }
    );
  };

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="Profile" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 16 }}>
        <View>
          <Text variant="muted" size="xs">
            Email
          </Text>
          <Text>
            {p.email} {p.emailVerified ? "· verified" : "· not verified"}
          </Text>
        </View>
        <Input label="Display name" value={displayName} onChangeText={setDisplayName} maxLength={100} />
        <Input label="Username" value={username} onChangeText={setUsername} autoCapitalize="none" autoCorrect={false} />
        <Input
          label="Time zone"
          value={timeZone}
          onChangeText={setTimeZone}
          placeholder={deviceTimeZone()}
          autoCapitalize="none"
          autoCorrect={false}
        />
        {error && (
          <Text variant="destructive" size="sm">
            {error}
          </Text>
        )}
        <Button isLoading={update.isPending} onPress={save}>
          Save
        </Button>
      </ScrollView>
    </SafeAreaView>
  );
}

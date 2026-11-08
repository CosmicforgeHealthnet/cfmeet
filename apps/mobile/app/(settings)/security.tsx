/** Security: sign-in sessions (`/api/v2/me/sessions`), revoke one, sign out everywhere. */
import { Alert, ScrollView, View } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { ScreenHeader } from "@/components/shared/screen-header";
import { useRevokeSession, useSessions } from "@/features/account/hooks";
import { useLogout } from "@/features/auth/hooks/use-logout";
import { decodeApiError } from "@/lib/api/errors";

export default function SecurityScreen() {
  const sessions = useSessions();
  const revoke = useRevokeSession();
  const logout = useLogout();
  const onError = (e: unknown) => Alert.alert("Could not revoke", decodeApiError(e).message);

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="Security" subtitle="Where you're signed in" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 12 }}>
        <Button variant="outline" onPress={() => router.push("/(settings)/change-password")}>
          Change password
        </Button>
        {sessions.isLoading ? (
          <Loading />
        ) : sessions.isError ? (
          <ErrorState message={decodeApiError(sessions.error).message} onRetry={() => void sessions.refetch()} />
        ) : (
          (sessions.data?.sessions ?? []).map((s) => (
            <Card key={s.sessionId}>
              <View className="flex-row items-center justify-between">
                <View className="flex-1">
                  <Text className="font-semibold">
                    {s.clientLabel || s.authMethod}
                    {s.current ? " · this device" : ""}
                  </Text>
                  <Text variant="muted" size="xs">
                    Signed in {new Date(s.authenticatedAt).toLocaleString()} · {s.status.toLowerCase()}
                  </Text>
                </View>
                {!s.current && s.status.toUpperCase() === "ACTIVE" && (
                  <Button size="sm" variant="outline" onPress={() => revoke.mutate(s.sessionId, { onError })}>
                    Revoke
                  </Button>
                )}
              </View>
            </Card>
          ))
        )}
        <Button
          variant="destructive"
          className="mt-4"
          onPress={() =>
            Alert.alert("Sign out everywhere?", "Every session, including this one, will be signed out.", [
              { text: "Cancel", style: "cancel" },
              {
                text: "Sign out everywhere",
                style: "destructive",
                onPress: () => revoke.mutate("others", { onSuccess: () => logout.mutate(), onError }),
              },
            ])
          }
        >
          Sign out everywhere
        </Button>
      </ScrollView>
    </SafeAreaView>
  );
}

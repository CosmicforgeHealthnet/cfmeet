/**
 * My organizations (ORG-016): memberships from `GET /api/v2/me/organizations`, a member list,
 * leaving, and accepting an invitation. Administration (members, roles, invitations, API
 * credentials, calling, billing) is web-first.
 */
import { Alert, Pressable, ScrollView, View } from "react-native";
import { router } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { Notice, ScreenHeader } from "@/components/shared/screen-header";
import { useColors } from "@/hooks/useColors";
import { useLeaveOrganization, useMyOrganizations } from "@/features/account/hooks";
import { decodeApiError } from "@/lib/api/errors";
import { roleLabel } from "@/lib/meeting/authority";

export default function OrganizationsScreen() {
  const colors = useColors();
  const orgs = useMyOrganizations();
  const leave = useLeaveOrganization();

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="Organizations" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 12 }}>
        <Button variant="outline" onPress={() => router.push("/(settings)/invitation")}>
          Accept an invitation
        </Button>
        {orgs.isLoading ? (
          <Loading />
        ) : orgs.isError ? (
          <ErrorState message={decodeApiError(orgs.error).message} onRetry={() => void orgs.refetch()} />
        ) : (orgs.data ?? []).length === 0 ? (
          <Text variant="muted">You're not a member of any organization.</Text>
        ) : (
          (orgs.data ?? []).map((m) => (
            <View key={m.organization.id} className="rounded-xl border border-border dark:border-border-dark p-4">
              <Pressable
                onPress={() =>
                  router.push({
                    pathname: "/(settings)/organization/[id]",
                    params: { id: m.organization.id, name: m.organization.name },
                  })
                }
                className="flex-row items-center justify-between"
              >
                <View className="flex-1">
                  <Text className="font-semibold">{m.organization.name}</Text>
                  <Text variant="muted" size="xs">
                    {roleLabel(m.role)}
                    {m.organizationRole ? ` · ${m.organizationRole}` : ""}
                    {m.organization.status ? ` · ${String(m.organization.status).toLowerCase()}` : ""}
                  </Text>
                </View>
                <Ionicons name="chevron-forward" size={18} color={colors.mutedForeground} />
              </Pressable>
              {m.role !== "OWNER" && (
                <Button
                  size="sm"
                  variant="ghost"
                  className="self-start mt-2"
                  onPress={() =>
                    Alert.alert(`Leave ${m.organization.name}?`, "You will lose access to its meetings.", [
                      { text: "Cancel", style: "cancel" },
                      {
                        text: "Leave",
                        style: "destructive",
                        onPress: () =>
                          leave.mutate(m.organization.id, {
                            onError: (e) => Alert.alert("Could not leave", decodeApiError(e).message),
                          }),
                      },
                    ])
                  }
                >
                  Leave organization
                </Button>
              )}
            </View>
          ))
        )}
        <Notice
          title="Managing an organization?"
          body="Members, roles, invitations, API credentials, calling and billing are managed in CF Meet on the web."
        />
      </ScrollView>
    </SafeAreaView>
  );
}

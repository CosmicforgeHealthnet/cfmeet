/**
 * Membership view of one organization (`GET /api/v2/organizations/{id}/members`, read-only).
 * The contract's member record carries ids, roles and status but no display names (gap noted).
 */
import { ScrollView, View } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Text } from "@/components/ui/text";
import { Badge } from "@/components/ui/badge";
import { Loading } from "@/components/shared/loading";
import { ErrorState } from "@/components/shared/error-state";
import { ScreenHeader } from "@/components/shared/screen-header";
import { useOrganizationMembers } from "@/features/account/hooks";
import { useAuthStore } from "@/store/auth.store";
import { decodeApiError } from "@/lib/api/errors";
import { roleLabel } from "@/lib/meeting/authority";

export default function OrganizationMembersScreen() {
  const { id, name } = useLocalSearchParams<{ id: string; name?: string }>();
  const { user } = useAuthStore();
  const members = useOrganizationMembers(id);
  const list = (members.data ?? []).filter((m) => m.status !== "REMOVED");

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title={name || "Organization"} subtitle="Members" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 8 }}>
        {members.isLoading ? (
          <Loading />
        ) : members.isError ? (
          <ErrorState
            message={
              decodeApiError(members.error).kind === "forbidden"
                ? "Your role in this organization cannot view the member list."
                : decodeApiError(members.error).message
            }
            onRetry={() => void members.refetch()}
          />
        ) : (
          <>
            <Text variant="muted" size="sm">
              {list.length} member{list.length === 1 ? "" : "s"}
            </Text>
            {list.map((m) => (
              <View
                key={m.id}
                className="flex-row items-center justify-between py-3 border-b border-border dark:border-border-dark"
              >
                <View className="flex-1">
                  <Text className="font-semibold">{m.userId === user?.id ? "You" : `Member ${m.userId.slice(0, 8)}`}</Text>
                  <Text variant="muted" size="xs">
                    {roleLabel(m.role)}
                    {m.organizationRole ? ` · ${m.organizationRole}` : ""} · joined {new Date(m.createdAt).toLocaleDateString()}
                  </Text>
                </View>
                {m.status !== "ACTIVE" && <Badge variant="destructive">{m.status.toLowerCase()}</Badge>}
              </View>
            ))}
          </>
        )}
      </ScrollView>
    </SafeAreaView>
  );
}

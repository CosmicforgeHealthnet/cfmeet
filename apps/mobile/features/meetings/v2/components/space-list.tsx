import { useState, useCallback } from "react";
import { View, FlatList, Pressable, RefreshControl } from "react-native";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { SpaceCard } from "./space-card";
import { ArchiveSpaceModal } from "./archive-space-modal";
import { useSpaces } from "../hooks";
import { useColors } from "@/hooks/useColors";
import type { MeetingSpaceResponse, SpaceStatus } from "../types";

type StatusFilter = SpaceStatus | "all";

const STATUS_FILTERS: { label: string; value: StatusFilter }[] = [
  { label: "All", value: "all" },
  { label: "Active", value: "ACTIVE" },
  { label: "Archived", value: "ARCHIVED" },
];

export function SpaceList() {
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("ACTIVE");
  const [archivingId, setArchivingId] = useState<string | null>(null);
  const colors = useColors();

  const { data, isLoading, isError, refetch, isRefetching } = useSpaces();

  const spaces = data?.items ?? [];

  const filteredSpaces = statusFilter === "all"
    ? spaces
    : spaces.filter(s => s.status === statusFilter);

  const handleJoin = useCallback((space: MeetingSpaceResponse) => {
    router.push({
      pathname: "/(room)/[id]",
      params: { id: space.meetingCode },
    });
  }, []);

  const handleEdit = useCallback((_space: MeetingSpaceResponse) => {
    // TODO: Implement edit functionality
  }, []);

  const handleFilterChange = useCallback((filter: StatusFilter) => {
    setStatusFilter(filter);
  }, []);

  if (isLoading) {
    return <Loading fullScreen message="Loading meeting spaces..." />;
  }

  if (isError) {
    return (
      <View className="flex-1 items-center justify-center px-6">
        <Ionicons name="alert-circle-outline" size={48} color={colors.destructive} style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
          Failed to load meeting spaces
        </Text>
        <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
          Please check your connection and try again.
        </Text>
        <Button onPress={() => refetch()} className="mt-4">
          Retry
        </Button>
      </View>
    );
  }

  return (
    <View className="flex-1">
      {/* Status Filters */}
      <View className="px-4 py-3 border-b border-border dark:border-border-dark">
        <FlatList
          horizontal
          data={STATUS_FILTERS}
          keyExtractor={(item) => item.value}
          showsHorizontalScrollIndicator={false}
          renderItem={({ item }) => (
            <Pressable
              onPress={() => handleFilterChange(item.value)}
              className={`px-4 py-1.5 rounded-full mr-2 ${
                statusFilter === item.value
                  ? "bg-primary dark:bg-primary-dark"
                  : "bg-secondary dark:bg-secondary-dark"
              }`}
            >
              <Text
                className={`text-sm font-bold ${
                  statusFilter === item.value
                    ? "text-primary-foreground dark:text-primary-foreground-dark"
                    : "text-muted-foreground dark:text-muted-foreground-dark"
                }`}
              >
                {item.label}
              </Text>
            </Pressable>
          )}
        />
      </View>

      {/* Space List */}
      <FlatList
        data={filteredSpaces}
        keyExtractor={(item) => item.id}
        renderItem={({ item }) => (
          <SpaceCard
            space={item}
            onPress={() => handleJoin(item)}
            onEdit={() => handleEdit(item)}
            onArchive={() => setArchivingId(item.id)}
          />
        )}
        contentContainerStyle={{ padding: 16, paddingBottom: 100 }}
        showsVerticalScrollIndicator={false}
        refreshControl={
          <RefreshControl refreshing={isRefetching} onRefresh={refetch} tintColor={colors.primary} />
        }
        ListHeaderComponent={
          <View className="flex-row items-center justify-between mb-4">
            <Text className="text-base font-bold text-foreground dark:text-foreground-dark">
              {filteredSpaces.length} space{filteredSpaces.length !== 1 ? "s" : ""}
            </Text>
          </View>
        }
        ListEmptyComponent={
          <View className="items-center py-12">
            <Ionicons name="folder-open-outline" size={48} color={colors.primary} style={{ opacity: 0.5 }} />
            <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
              No meeting spaces found
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
              {statusFilter !== "all"
                ? `No ${statusFilter.toLowerCase()} meeting spaces to show.`
                : "Create your first meeting space to get started."}
            </Text>
          </View>
        }
      />

      {/* Archive Confirmation Modal */}
      {archivingId && (
        <ArchiveSpaceModal
          spaceId={archivingId}
          onClose={() => setArchivingId(null)}
        />
      )}
    </View>
  );
}

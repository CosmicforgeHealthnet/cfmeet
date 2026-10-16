import { useState, useCallback } from "react";
import { View, FlatList, Pressable, RefreshControl } from "react-native";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { MeetingCard } from "./meeting-card";
import { DeleteMeetingModal } from "./delete-meeting-modal";
import { useMeetings } from "../hooks/use-meetings";
import { useColors } from "@/hooks/useColors";
import type { Meeting, MeetingStatus } from "../types";

type StatusFilter = MeetingStatus | "all";

const STATUS_FILTERS: { label: string; value: StatusFilter }[] = [
  { label: "All", value: "all" },
  { label: "Scheduled", value: "scheduled" },
  { label: "Live", value: "live" },
  { label: "Ended", value: "ended" },
  { label: "Cancelled", value: "cancelled" },
];

export function MeetingList() {
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [page, setPage] = useState(1);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const colors = useColors();

  const { data, isLoading, isError, refetch, isRefetching } = useMeetings(page, 10);

  const meetings = data?.meetings ?? [];
  const total = data?.total ?? 0;
  const totalPages = data?.total_pages ?? 1;

  const filteredMeetings = statusFilter === "all"
    ? meetings
    : meetings.filter(m => m.status === statusFilter);

  const handleJoin = useCallback((meeting: Meeting) => {
    router.push({
      pathname: "/(room)/[id]",
      params: { id: meeting.meeting_identifier },
    });
  }, []);

  const handleEdit = useCallback((meeting: Meeting) => {
    router.push({
      pathname: "/(meetings)/[id]/edit",
      params: { id: meeting.id },
    });
  }, []);

  const handleFilterChange = useCallback((filter: StatusFilter) => {
    setStatusFilter(filter);
    setPage(1);
  }, []);

  if (isLoading) {
    return <Loading fullScreen message="Loading meetings..." />;
  }

  if (isError) {
    return (
      <View className="flex-1 items-center justify-center px-6">
        <Ionicons name="alert-circle-outline" size={48} color={colors.destructive} style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
          Failed to load meetings
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

      {/* Meeting List */}
      <FlatList
        data={filteredMeetings}
        keyExtractor={(item) => item.id}
        renderItem={({ item }) => (
          <MeetingCard
            meeting={item}
            onPress={() => handleJoin(item)}
            onEdit={() => handleEdit(item)}
            onDelete={() => setDeletingId(item.id)}
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
              {total} meeting{total !== 1 ? "s" : ""}
            </Text>
          </View>
        }
        ListEmptyComponent={
          <View className="items-center py-12">
            <Ionicons name="calendar-outline" size={48} color={colors.primary} style={{ opacity: 0.5 }} />
            <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
              No meetings found
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
              {statusFilter !== "all"
                ? `No ${statusFilter} meetings to show.`
                : "Schedule your first meeting to get started."}
            </Text>
          </View>
        }
        ListFooterComponent={
          totalPages > 1 ? (
            <View className="flex-row items-center justify-center gap-4 py-4">
              <Pressable
                onPress={() => setPage(p => Math.max(1, p - 1))}
                disabled={page <= 1}
                className={`p-2 rounded-lg ${page <= 1 ? "opacity-30" : "bg-secondary dark:bg-secondary-dark"}`}
              >
                <Ionicons name="chevron-back" size={20} color={colors.primary} />
              </Pressable>
              <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
                Page {page} of {totalPages}
              </Text>
              <Pressable
                onPress={() => setPage(p => Math.min(totalPages, p + 1))}
                disabled={page >= totalPages}
                className={`p-2 rounded-lg ${page >= totalPages ? "opacity-30" : "bg-secondary dark:bg-secondary-dark"}`}
              >
                <Ionicons name="chevron-forward" size={20} color={colors.primary} />
              </Pressable>
            </View>
          ) : null
        }
      />

      {/* Delete Confirmation Modal */}
      {deletingId && (
        <DeleteMeetingModal
          meetingId={deletingId}
          onClose={() => setDeletingId(null)}
        />
      )}
    </View>
  );
}

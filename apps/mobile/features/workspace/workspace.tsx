/**
 * Workspace context: Personal + the organizations the user belongs to (ORG-016).
 *
 * One app, no organization-branded build: the user switches context here. The list comes from
 * `GET /api/v2/me/workspaces`; the selection is a local preference reconciled with that list on
 * every refresh (a removed membership falls back to Personal and purges that context's cache).
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { Modal, Pressable, ScrollView, View } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { create } from "zustand";
import { Text } from "@/components/ui/text";
import { useColors } from "@/hooks/useColors";
import { STORAGE_KEYS } from "@/lib/constants";
import { organizationsApi } from "@/features/organizations/api";
import {
  PERSONAL_WORKSPACE_ID,
  isContextKey,
  isSuspended,
  organizationIdOf,
  reconcileWorkspace,
} from "@/lib/workspace/context";
import type { Workspace } from "@/lib/api/generated/meeting-service";
import { useAuthStore } from "@/store/auth.store";

interface WorkspaceSelection {
  selectedId: string;
  loaded: boolean;
  setSelected: (id: string) => void;
  load: () => Promise<void>;
}

export const useWorkspaceSelection = create<WorkspaceSelection>((set) => ({
  selectedId: PERSONAL_WORKSPACE_ID,
  loaded: false,
  setSelected: (id) => {
    set({ selectedId: id });
    AsyncStorage.setItem(STORAGE_KEYS.WORKSPACE, id).catch(() => undefined);
  },
  load: async () => {
    try {
      const stored = await AsyncStorage.getItem(STORAGE_KEYS.WORKSPACE);
      set({ selectedId: stored || PERSONAL_WORKSPACE_ID, loaded: true });
    } catch {
      set({ loaded: true });
    }
  },
}));

export const WORKSPACES_KEY = ["me", "workspaces"] as const;

export interface WorkspaceState {
  workspaces: Workspace[];
  current: Workspace | null;
  workspaceId: string;
  /** Organization id to pass to organization-scoped calls; undefined for Personal. */
  organizationId: string | undefined;
  isPersonal: boolean;
  suspended: boolean;
  isLoading: boolean;
  select: (id: string) => void;
}

export function useWorkspace(): WorkspaceState {
  const { isAuthenticated } = useAuthStore();
  const { selectedId, loaded, setSelected, load } = useWorkspaceSelection();
  const queryClient = useQueryClient();

  useEffect(() => {
    if (!loaded) void load();
  }, [loaded, load]);

  const query = useQuery({
    queryKey: WORKSPACES_KEY,
    queryFn: organizationsApi.workspaces,
    enabled: isAuthenticated,
    staleTime: 60_000,
  });
  const workspaces = useMemo(() => query.data?.items ?? [], [query.data]);

  useEffect(() => {
    if (!query.data) return;
    const { workspaceId, removedWorkspaceId } = reconcileWorkspace(selectedId, workspaces);
    if (removedWorkspaceId) {
      queryClient.removeQueries({ predicate: (q) => isContextKey(q.queryKey, removedWorkspaceId) });
      setSelected(workspaceId);
    }
  }, [query.data, selectedId, workspaces, queryClient, setSelected]);

  const current =
    workspaces.find((w) => w.id === selectedId) ??
    workspaces.find((w) => w.kind === "PERSONAL") ??
    null;
  const workspaceId = current?.id ?? PERSONAL_WORKSPACE_ID;

  const select = useCallback((id: string) => setSelected(id), [setSelected]);

  return {
    workspaces,
    current,
    workspaceId,
    organizationId: organizationIdOf(current),
    isPersonal: !current || current.kind === "PERSONAL",
    suspended: current ? isSuspended(current) : false,
    isLoading: query.isLoading,
    select,
  };
}

/** Compact switcher (header pill + sheet listing Personal and every organization). */
export function WorkspaceSwitcher() {
  const colors = useColors();
  const { workspaces, current, workspaceId, select } = useWorkspace();
  const [open, setOpen] = useState(false);

  return (
    <>
      <Pressable
        accessibilityRole="button"
        accessibilityLabel="Switch workspace"
        onPress={() => setOpen(true)}
        className="flex-row items-center gap-2 rounded-full border border-border dark:border-border-dark px-3 py-1.5"
      >
        <Ionicons
          name={current?.kind === "ORGANIZATION" ? "business-outline" : "person-outline"}
          size={16}
          color={colors.primary}
        />
        <Text className="text-sm font-semibold" numberOfLines={1}>
          {current?.displayName ?? "Personal"}
        </Text>
        <Ionicons name="chevron-down" size={14} color={colors.mutedForeground} />
      </Pressable>
      <Modal transparent visible={open} animationType="fade" onRequestClose={() => setOpen(false)}>
        <Pressable className="flex-1 bg-black/40 justify-end" onPress={() => setOpen(false)}>
          <Pressable className="bg-card dark:bg-card-dark rounded-t-3xl p-6 max-h-[70%]">
            <Text className="text-lg font-bold mb-1">Workspace</Text>
            <Text variant="muted" size="sm" className="mb-4">
              Meetings and calendar follow the workspace you choose.
            </Text>
            <ScrollView>
              {(workspaces.length ? workspaces : [personalPlaceholder]).map((w) => (
                <Pressable
                  key={w.id}
                  onPress={() => {
                    select(w.id);
                    setOpen(false);
                  }}
                  className="flex-row items-center justify-between py-3 border-b border-border dark:border-border-dark"
                >
                  <View className="flex-row items-center gap-3 flex-1">
                    <Ionicons
                      name={w.kind === "ORGANIZATION" ? "business-outline" : "person-outline"}
                      size={20}
                      color={colors.primary}
                    />
                    <View className="flex-1">
                      <Text className="font-semibold">{w.displayName}</Text>
                      <Text variant="muted" size="xs">
                        {w.kind === "ORGANIZATION"
                          ? `${w.role ?? "Member"}${isSuspended(w) ? " · suspended" : ""}`
                          : "Your personal meetings"}
                      </Text>
                    </View>
                  </View>
                  {w.id === workspaceId && <Ionicons name="checkmark" size={20} color={colors.primary} />}
                </Pressable>
              ))}
            </ScrollView>
          </Pressable>
        </Pressable>
      </Modal>
    </>
  );
}

const personalPlaceholder = {
  id: PERSONAL_WORKSPACE_ID,
  kind: "PERSONAL",
  displayName: "Personal",
} as Workspace;

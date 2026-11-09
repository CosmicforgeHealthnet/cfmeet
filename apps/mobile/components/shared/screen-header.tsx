import { Pressable, View } from "react-native";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { useColors } from "@/hooks/useColors";

interface ScreenHeaderProps {
  title: string;
  subtitle?: string;
  onBack?: () => void;
  right?: React.ReactNode;
}

/** Title bar with a back button for stack screens. */
export function ScreenHeader({ title, subtitle, onBack, right }: ScreenHeaderProps) {
  const colors = useColors();
  return (
    <View className="flex-row items-center gap-3 border-b border-border dark:border-border-dark px-4 py-3">
      <Pressable
        accessibilityRole="button"
        accessibilityLabel="Back"
        onPress={onBack ?? (() => (router.canGoBack() ? router.back() : router.replace("/(tabs)")))}
        className="p-1"
      >
        <Ionicons name="chevron-back" size={24} color={colors.foreground} />
      </Pressable>
      <View className="flex-1">
        <Text className="text-lg font-bold" numberOfLines={1}>
          {title}
        </Text>
        {subtitle ? (
          <Text variant="muted" size="xs" numberOfLines={1}>
            {subtitle}
          </Text>
        ) : null}
      </View>
      {right}
    </View>
  );
}

/** A labelled section card body used by settings-like screens. */
export function SectionTitle({ children }: { children: string }) {
  return (
    <Text className="mb-2 text-sm font-bold text-muted-foreground dark:text-muted-foreground-dark uppercase tracking-wider">
      {children}
    </Text>
  );
}

/** A truthful "not available on mobile / blocked" notice. */
export function Notice({ title, body, tone = "info" }: { title: string; body: string; tone?: "info" | "warning" }) {
  const colors = useColors();
  return (
    <View className="flex-row gap-3 rounded-xl border border-border dark:border-border-dark bg-muted dark:bg-muted-dark p-4">
      <Ionicons
        name={tone === "warning" ? "alert-circle-outline" : "information-circle-outline"}
        size={20}
        color={tone === "warning" ? colors.warning : colors.primary}
      />
      <View className="flex-1">
        <Text className="font-semibold">{title}</Text>
        <Text variant="muted" size="sm" className="mt-1">
          {body}
        </Text>
      </View>
    </View>
  );
}

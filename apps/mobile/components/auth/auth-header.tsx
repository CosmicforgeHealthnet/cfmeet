import { View, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";

interface AuthHeaderProps {
  title: string;
  subtitle: string;
  showBackButton?: boolean;
}

export function AuthHeader({
  title,
  subtitle,
  showBackButton = true,
}: AuthHeaderProps) {
  const router = useRouter();

  return (
    <View className="w-full mb-4">
      {showBackButton && (
        <Pressable
          onPress={() => router.back()}
          className="flex-row items-center mb-3"
        >
          <Ionicons name="chevron-back" size={20} color="#006BFF" />
          <Text className="text-primary text-sm">Go Back</Text>
        </Pressable>
      )}

      <View className="items-center">
        <Text className="text-xl font-bold text-foreground dark:text-foreground-dark tracking-tight">
          {title}
        </Text>
        <Text className="mt-1 text-sm text-foreground dark:text-foreground-dark font-medium">
          {subtitle}
        </Text>
      </View>
    </View>
  );
}

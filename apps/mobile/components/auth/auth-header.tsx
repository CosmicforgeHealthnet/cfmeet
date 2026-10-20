import { View, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Logo } from "@/components/ui/logo";
import { useColors } from "@/hooks/useColors";

interface AuthHeaderProps {
  title: string;
  subtitle: string;
  showBackButton?: boolean;
  showLogo?: boolean;
}

export function AuthHeader({
  title,
  subtitle,
  showBackButton = true,
  showLogo = true,
}: AuthHeaderProps) {
  const router = useRouter();
  const colors = useColors();

  return (
    <View className="w-full mb-4">
      {showBackButton && (
        <Pressable
          onPress={() => router.back()}
          className="flex-row items-center mb-3"
        >
          <Ionicons name="chevron-back" size={20} color={colors.primary} />
          <Text className="text-primary text-sm">Go Back</Text>
        </Pressable>
      )}

      <View className="items-center">
        {showLogo && (
          <Logo variant="medium" className="mb-4" />
        )}
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

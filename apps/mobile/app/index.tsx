import { useEffect } from "react";
import { View } from "react-native";
import { router } from "expo-router";
import { useAuthStore } from "@/store/auth.store";
import { FullScreenLoading } from "@/components/shared/loading";

export default function IndexScreen() {
  const { isAuthenticated, isLoading } = useAuthStore();

  useEffect(() => {
    if (!isLoading) {
      if (isAuthenticated) {
        router.replace("/(tabs)");
      } else {
        router.replace("/login");
      }
    }
  }, [isAuthenticated, isLoading]);

  return (
    <View className="flex-1 bg-background">
      <FullScreenLoading />
    </View>
  );
}

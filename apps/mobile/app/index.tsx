import { useEffect } from "react";
import { View } from "react-native";
import { router } from "expo-router";
import { useAuthStore } from "@/store/auth.store";
import { FullScreenLoading } from "@/components/shared/loading";
import { routeAfterSignIn } from "@/lib/auth/account-status";

export default function IndexScreen() {
  const { isAuthenticated, isLoading, user } = useAuthStore();

  useEffect(() => {
    if (isLoading) return;
    if (!isAuthenticated || !user) {
      router.replace("/login");
      return;
    }
    // ACC-012: an account that has not verified its email goes to verification, not the app.
    const next = routeAfterSignIn(user.status, user.email ?? "");
    if (next.kind === "verify") router.replace({ pathname: "/verify", params: { email: next.email } });
    else router.replace("/(tabs)");
  }, [isAuthenticated, isLoading, user]);

  return (
    <View className="flex-1 bg-background">
      <FullScreenLoading />
    </View>
  );
}

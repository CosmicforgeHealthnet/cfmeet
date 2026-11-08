import "../global.css";
import { registerGlobals } from "@livekit/react-native";

import { useEffect, useState, useCallback } from "react";
import { View } from "react-native";
import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";
import * as SplashScreen from "expo-splash-screen";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { ThemeProvider, useTheme } from "@/providers/theme-provider";
import { QueryProvider } from "@/providers/query-provider";
import { AuthProvider } from "@/providers/auth-provider";
import { ToastProvider } from "@/components/ui/toast";
import { useAuthStore } from "@/store/auth.store";

// LiveKit's WebRTC globals must exist before any room is created (requires a development
// build: the native WebRTC module is not part of Expo Go).
registerGlobals();

// Keep splash screen visible while we fetch resources
SplashScreen.preventAutoHideAsync();

function RootLayoutNav() {
  const { resolvedTheme } = useTheme();
  const { isLoading: isAuthLoading } = useAuthStore();
  const [appIsReady, setAppIsReady] = useState(false);

  useEffect(() => {
    async function prepare() {
      try {
        // Wait for auth state to be determined
        if (!isAuthLoading) {
          // Add a small delay for smoother transition
          await new Promise(resolve => setTimeout(resolve, 500));
          setAppIsReady(true);
        }
      } catch {
        setAppIsReady(true);
      }
    }

    prepare();
  }, [isAuthLoading]);

  const onLayoutRootView = useCallback(async () => {
    if (appIsReady) {
      // Hide splash screen once app is ready
      await SplashScreen.hideAsync();
    }
  }, [appIsReady]);

  if (!appIsReady) {
    return null;
  }

  return (
    <View style={{ flex: 1 }} onLayout={onLayoutRootView}>
      <StatusBar style={resolvedTheme === "dark" ? "light" : "dark"} />
      <Stack screenOptions={{ headerShown: false }}>
        <Stack.Screen name="index" />
        <Stack.Screen name="(auth)" />
        <Stack.Screen name="(tabs)" />
        <Stack.Screen name="(room)" options={{ gestureEnabled: false }} />
        <Stack.Screen name="(meetings)" />
        <Stack.Screen name="(settings)" />
        <Stack.Screen name="(chats)" />
        <Stack.Screen name="join" />
      </Stack>
    </View>
  );
}

export default function RootLayout() {
  return (
    <SafeAreaProvider>
      <ThemeProvider>
        <QueryProvider>
          <AuthProvider>
            <ToastProvider>
              <RootLayoutNav />
            </ToastProvider>
          </AuthProvider>
        </QueryProvider>
      </ThemeProvider>
    </SafeAreaProvider>
  );
}

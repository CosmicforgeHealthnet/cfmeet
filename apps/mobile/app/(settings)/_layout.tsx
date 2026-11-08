import { Stack } from "expo-router";

export default function SettingsLayout() {
  return (
    <Stack screenOptions={{ headerShown: false }}>
      <Stack.Screen name="edit-profile" />
      <Stack.Screen name="change-password" />
      <Stack.Screen name="security" />
      <Stack.Screen name="organizations" />
      <Stack.Screen name="organization/[id]" />
      <Stack.Screen name="invitation" />
      <Stack.Screen name="communication" />
    </Stack>
  );
}

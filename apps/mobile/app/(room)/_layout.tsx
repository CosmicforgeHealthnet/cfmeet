import { Stack } from "expo-router";

export default function RoomLayout() {
  return (
    <Stack screenOptions={{ headerShown: false }}>
      <Stack.Screen name="[id]/index" />
      <Stack.Screen name="[id]/live" />
    </Stack>
  );
}

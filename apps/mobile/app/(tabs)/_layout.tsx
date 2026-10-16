import { Tabs } from "expo-router";
import { View } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { useTheme } from "@/providers/theme-provider";

type IconName = "home" | "home-outline" | "list" | "list-outline" | "calendar" | "calendar-outline" | "settings" | "settings-outline";

interface TabIconProps {
  icon: IconName;
  iconOutline: IconName;
  focused: boolean;
  color: string;
}

function TabIcon({ icon, iconOutline, focused, color }: TabIconProps) {
  return (
    <View className="items-center justify-center">
      <Ionicons
        name={focused ? icon : iconOutline}
        size={28}
        color={color}
      />
    </View>
  );
}

export default function TabsLayout() {
  const { resolvedTheme } = useTheme();

  const isDark = resolvedTheme === "dark";

  // CF Meet brand colors
  const activeColor = isDark ? "#00C6FF" : "#006BFF";
  const inactiveColor = isDark ? "#94A3B8" : "#64748B";

  return (
    <Tabs
      screenOptions={{
        headerShown: false,
        tabBarShowLabel: false,
        tabBarStyle: {
          backgroundColor: isDark ? "#1E293B" : "#ffffff",
          borderTopColor: isDark ? "rgba(255, 255, 255, 0.1)" : "#E2E8F0",
          height: 70,
          paddingBottom: 10,
          paddingTop: 15,
          borderTopLeftRadius: 24,
          borderTopRightRadius: 24,
          position: "absolute",
          shadowColor: "#000",
          shadowOffset: { width: 0, height: -4 },
          shadowOpacity: 0.1,
          shadowRadius: 12,
          elevation: 10,
        },
        tabBarItemStyle: {
          paddingVertical: 0,
          paddingHorizontal: 0,
        },
      }}
    >
      <Tabs.Screen
        name="index"
        options={{
          tabBarIcon: ({ focused }) => (
            <TabIcon
              icon="home"
              iconOutline="home-outline"
              focused={focused}
              color={focused ? activeColor : inactiveColor}
            />
          ),
        }}
      />
      <Tabs.Screen
        name="meetings"
        options={{
          tabBarIcon: ({ focused }) => (
            <TabIcon
              icon="list"
              iconOutline="list-outline"
              focused={focused}
              color={focused ? activeColor : inactiveColor}
            />
          ),
        }}
      />
      <Tabs.Screen
        name="schedule"
        options={{
          tabBarIcon: ({ focused }) => (
            <TabIcon
              icon="calendar"
              iconOutline="calendar-outline"
              focused={focused}
              color={focused ? activeColor : inactiveColor}
            />
          ),
        }}
      />
      <Tabs.Screen
        name="settings"
        options={{
          tabBarIcon: ({ focused }) => (
            <TabIcon
              icon="settings"
              iconOutline="settings-outline"
              focused={focused}
              color={focused ? activeColor : inactiveColor}
            />
          ),
        }}
      />
    </Tabs>
  );
}

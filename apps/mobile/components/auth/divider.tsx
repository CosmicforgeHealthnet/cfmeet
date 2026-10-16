import { View } from "react-native";
import { Text } from "@/components/ui/text";

interface DividerProps {
  text?: string;
}

export function Divider({ text = "OR" }: DividerProps) {
  return (
    <View className="my-4 flex-row items-center">
      <View className="flex-1 h-px bg-primary dark:bg-primary-dark/30" />
      <View className="px-4">
        <Text className="text-sm font-bold tracking-widest text-primary dark:text-primary-dark/30">
          {text}
        </Text>
      </View>
      <View className="flex-1 h-px bg-primary dark:bg-primary-dark/30" />
    </View>
  );
}

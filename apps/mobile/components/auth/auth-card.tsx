import { View, ViewProps, Platform } from "react-native";
import { cn } from "@/lib/utils/cn";

interface AuthCardProps extends ViewProps {
  children: React.ReactNode;
}

export function AuthCard({ children, className, style, ...props }: AuthCardProps) {
  return (
    <View
      className={cn(
        "w-full bg-card dark:bg-card-dark rounded-3xl px-5 py-6",
        className
      )}
      style={[
        Platform.select({
          ios: {
            shadowColor: "#000",
            shadowOffset: { width: 0, height: 4 },
            shadowOpacity: 0.15,
            shadowRadius: 12,
          },
          android: {
            elevation: 8,
          },
        }),
        style,
      ]}
      {...props}
    >
      {children}
    </View>
  );
}

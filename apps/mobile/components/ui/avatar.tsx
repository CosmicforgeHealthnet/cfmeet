import { View, Image, ImageSourcePropType } from "react-native";
import { cn } from "@/lib/utils/cn";
import { Text } from "./text";

interface AvatarProps {
  source?: ImageSourcePropType | null;
  fallback?: string;
  size?: "sm" | "default" | "lg" | "xl";
  className?: string;
}

const sizeStyles: Record<NonNullable<AvatarProps["size"]>, string> = {
  sm: "h-8 w-8",
  default: "h-10 w-10",
  lg: "h-12 w-12",
  xl: "h-16 w-16",
};

const textSizeStyles: Record<NonNullable<AvatarProps["size"]>, string> = {
  sm: "text-xs",
  default: "text-sm",
  lg: "text-base",
  xl: "text-xl",
};

const getInitials = (name: string): string => {
  const parts = name.trim().split(" ");
  if (parts.length >= 2) {
    return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
  }
  return name.slice(0, 2).toUpperCase();
};

const getColorFromName = (name: string): string => {
  const colors = [
    "bg-red-500",
    "bg-orange-500",
    "bg-amber-500",
    "bg-yellow-500",
    "bg-lime-500",
    "bg-green-500",
    "bg-emerald-500",
    "bg-teal-500",
    "bg-cyan-500",
    "bg-sky-500",
    "bg-blue-500",
    "bg-indigo-500",
    "bg-violet-500",
    "bg-purple-500",
    "bg-fuchsia-500",
    "bg-pink-500",
    "bg-rose-500",
  ];

  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = name.charCodeAt(i) + ((hash << 5) - hash);
  }

  return colors[Math.abs(hash) % colors.length];
};

export function Avatar({
  source,
  fallback = "?",
  size = "default",
  className,
}: AvatarProps) {
  if (source) {
    return (
      <View
        className={cn(
          "overflow-hidden rounded-full",
          sizeStyles[size],
          className
        )}
      >
        <Image
          source={source}
          className="h-full w-full"
          resizeMode="cover"
        />
      </View>
    );
  }

  const bgColor = getColorFromName(fallback);
  const initials = getInitials(fallback);

  return (
    <View
      className={cn(
        "items-center justify-center rounded-full",
        sizeStyles[size],
        bgColor,
        className
      )}
    >
      <Text className={cn("font-semibold text-white", textSizeStyles[size])}>
        {initials}
      </Text>
    </View>
  );
}

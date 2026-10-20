import { Image, View } from "react-native";
import { useTheme } from "@/providers/theme-provider";

// Import logo assets
const lightModeLogo = require("@/assets/light-mode-cf-meet.png");
const darkModeLogo = require("@/assets/dark-mode-cfmeet.png");
const iconLogo = require("@/assets/CFLogo.png");

interface LogoProps {
  /**
   * Size variant of the logo
   * - "icon": Shows only the CF icon (32x32)
   * - "small": Full logo at small size (100x28)
   * - "medium": Full logo at medium size (120x32)
   * - "large": Full logo at large size (150x40)
   */
  variant?: "icon" | "small" | "medium" | "large";
  className?: string;
}

const sizeMap = {
  icon: { width: 32, height: 32 },
  small: { width: 100, height: 28 },
  medium: { width: 120, height: 32 },
  large: { width: 150, height: 40 },
};

export function Logo({ variant = "medium", className }: LogoProps) {
  const { resolvedTheme } = useTheme();
  const isDark = resolvedTheme === "dark";
  const { width, height } = sizeMap[variant];

  // Icon variant uses the same logo regardless of theme
  if (variant === "icon") {
    return (
      <View className={className}>
        <Image
          source={iconLogo}
          style={{ width, height }}
          resizeMode="contain"
        />
      </View>
    );
  }

  // Full logo with light/dark mode variants
  return (
    <View className={className}>
      <Image
        source={isDark ? darkModeLogo : lightModeLogo}
        style={{ width, height }}
        resizeMode="contain"
      />
    </View>
  );
}

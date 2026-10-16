import { useTheme } from "@/providers/theme-provider";
import { getColors, ThemeColors } from "@/lib/colors";

export function useColors(): ThemeColors {
  const { resolvedTheme } = useTheme();
  return getColors(resolvedTheme === "dark");
}

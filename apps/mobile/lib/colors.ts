/**
 * CF Meet Brand Colors
 * These colors match the globals.css in the web app
 */

export const colors = {
  light: {
    // Brand
    primary: "#006BFF",
    accent: "#00C6FF",

    // Backgrounds
    background: "#FFFFFF",
    card: "#FFFFFF",
    muted: "#F1F5F9",
    sidebar: "#FAFBFC",

    // Text
    foreground: "#0F172A",
    mutedForeground: "#64748B",

    // Borders
    border: "#E2E8F0",

    // Semantic
    destructive: "#EF4444",
    success: "#22C55E",
    warning: "#F59E0B",

    // Tab bar
    tabActive: "#006BFF",
    tabInactive: "#64748B",
  },
  dark: {
    // Brand
    primary: "#006BFF",
    accent: "#00C6FF",

    // Backgrounds
    background: "#0F172A",
    card: "#1E293B",
    muted: "#334155",
    sidebar: "#1E293B",

    // Text
    foreground: "#F8FAFC",
    mutedForeground: "#94A3B8",

    // Borders
    border: "rgba(255, 255, 255, 0.1)",

    // Semantic
    destructive: "#EF4444",
    success: "#22C55E",
    warning: "#F59E0B",

    // Tab bar
    tabActive: "#00C6FF",
    tabInactive: "#94A3B8",
  },
};

export type ThemeColors = typeof colors.light;

export const getColors = (isDark: boolean): ThemeColors => {
  return isDark ? colors.dark : colors.light;
};

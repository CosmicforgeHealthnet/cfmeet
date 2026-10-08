import AsyncStorage from "@react-native-async-storage/async-storage";
import { useColorScheme } from "nativewind";
import { createContext, useContext, useEffect, useState } from "react";
import { Appearance, ColorSchemeName } from "react-native";

type Theme = "light" | "dark" | "system";
type ResolvedTheme = "light" | "dark";

interface ThemeContextType {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  resolvedTheme: ResolvedTheme;
}

const ThemeContext = createContext<ThemeContextType | undefined>(undefined);

const THEME_STORAGE_KEY = "app-theme";

const normalizeColorScheme = (scheme: ColorSchemeName): ResolvedTheme => {
  return scheme === "dark" ? "dark" : "light";
};

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState<Theme>("system");
  const { setColorScheme } = useColorScheme();

  const getSystemTheme = (): ResolvedTheme => {
    const scheme = Appearance.getColorScheme();
    return scheme === "dark" ? "dark" : "light";
  };

  const resolvedTheme: ResolvedTheme = theme === "system" ? getSystemTheme() : theme;

  useEffect(() => {
    const loadTheme = async () => {
      try {
        const savedTheme = await AsyncStorage.getItem(THEME_STORAGE_KEY);
        if (savedTheme && ["light", "dark", "system"].includes(savedTheme)) {
          setThemeState(savedTheme as Theme);
        }
      } catch (error) {
        console.error("Failed to load theme:", error);
      }
    };
    loadTheme();
  }, []);

  useEffect(() => {
    setColorScheme(resolvedTheme);
  }, [resolvedTheme, setColorScheme]);

  useEffect(() => {
    if (theme !== "system") return;

    const subscription = Appearance.addChangeListener(({ colorScheme }) => {
      setColorScheme(normalizeColorScheme(colorScheme));
    });

    return () => subscription.remove();
  }, [theme, setColorScheme]);

  const setTheme = async (newTheme: Theme) => {
    setThemeState(newTheme);
    try {
      await AsyncStorage.setItem(THEME_STORAGE_KEY, newTheme);
    } catch (error) {
      console.error("Failed to save theme:", error);
    }
  };

  return (
    <ThemeContext.Provider value={{ theme, setTheme, resolvedTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}

export function useTheme() {
  const context = useContext(ThemeContext);
  if (context === undefined) {
    throw new Error("useTheme must be used within a ThemeProvider");
  }
  return context;
}

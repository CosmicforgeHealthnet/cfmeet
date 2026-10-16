/** @type {import('tailwindcss').Config} */
module.exports = {
  darkMode: "class",
  content: [
    "./app/**/*.{js,jsx,ts,tsx}",
    "./components/**/*.{js,jsx,ts,tsx}",
    "./features/**/*.{js,jsx,ts,tsx}",
    "./providers/**/*.{js,jsx,ts,tsx}",
  ],
  presets: [require("nativewind/preset")],
  theme: {
    extend: {
      colors: {
        /*
         * CF Meet Brand Colors
         * Cyan: #00C6FF
         * Electric Blue: #006BFF
         * Deep Blue: #2563EB
         * Violet: #A855F7
         * White: #FFFFFF
         * Deep Navy: #0F172A
         */
        brand: {
          cyan: "#00C6FF",
          "electric-blue": "#006BFF",
          "deep-blue": "#2563EB",
          violet: "#A855F7",
          white: "#FFFFFF",
          navy: "#0F172A",
          DEFAULT: "#006BFF",
          light: "#00C6FF",
          dark: "#2563EB",
        },
        // Core colors matching web globals.css
        border: {
          DEFAULT: "#E2E8F0",
          dark: "rgba(255, 255, 255, 0.1)",
        },
        input: {
          DEFAULT: "#E2E8F0",
          dark: "rgba(255, 255, 255, 0.15)",
        },
        ring: {
          DEFAULT: "#006BFF",
          dark: "#00C6FF",
        },
        background: {
          DEFAULT: "#FFFFFF",
          dark: "#0F172A",
        },
        foreground: {
          DEFAULT: "#0F172A",
          dark: "#F8FAFC",
        },
        primary: {
          DEFAULT: "#006BFF",
          dark: "#006BFF",
          foreground: "#FFFFFF",
          "foreground-dark": "#FFFFFF",
        },
        secondary: {
          DEFAULT: "#F1F5F9",
          dark: "#334155",
          foreground: "#0F172A",
          "foreground-dark": "#F8FAFC",
        },
        destructive: {
          DEFAULT: "#EF4444",
          dark: "#EF4444",
          foreground: "#FFFFFF",
          "foreground-dark": "#FFFFFF",
        },
        success: {
          DEFAULT: "#22C55E",
          dark: "#22C55E",
          foreground: "#FFFFFF",
          "foreground-dark": "#0F172A",
        },
        warning: {
          DEFAULT: "#F59E0B",
          dark: "#F59E0B",
          foreground: "#0F172A",
          "foreground-dark": "#0F172A",
        },
        muted: {
          DEFAULT: "#F1F5F9",
          dark: "#334155",
          foreground: "#64748B",
          "foreground-dark": "#94A3B8",
        },
        accent: {
          DEFAULT: "#00C6FF",
          dark: "#00C6FF",
          foreground: "#0F172A",
          "foreground-dark": "#0F172A",
        },
        card: {
          DEFAULT: "#FFFFFF",
          dark: "#1E293B",
          foreground: "#0F172A",
          "foreground-dark": "#F8FAFC",
        },
        popover: {
          DEFAULT: "#FFFFFF",
          dark: "#1E293B",
          foreground: "#0F172A",
          "foreground-dark": "#F8FAFC",
        },
        // Sidebar colors
        sidebar: {
          DEFAULT: "#FAFBFC",
          dark: "#1E293B",
          foreground: "#0F172A",
          "foreground-dark": "#F8FAFC",
          primary: "#006BFF",
          "primary-dark": "#00C6FF",
          "primary-foreground": "#FFFFFF",
          "primary-foreground-dark": "#0F172A",
          accent: "#F1F5F9",
          "accent-dark": "#334155",
          "accent-foreground": "#006BFF",
          "accent-foreground-dark": "#00C6FF",
          border: "#E2E8F0",
          "border-dark": "rgba(255, 255, 255, 0.1)",
          ring: "#006BFF",
          "ring-dark": "#00C6FF",
        },
        // Tab bar colors
        tab: {
          bar: "#FFFFFF",
          "bar-dark": "#1E293B",
          active: "#006BFF",
          "active-dark": "#00C6FF",
          inactive: "#64748B",
          "inactive-dark": "#94A3B8",
        },
        // Chart colors using brand palette
        chart: {
          1: "#00C6FF",
          "1-dark": "#00C6FF",
          2: "#006BFF",
          "2-dark": "#006BFF",
          3: "#2563EB",
          "3-dark": "#2563EB",
          4: "#A855F7",
          "4-dark": "#A855F7",
          5: "#0F172A",
          "5-dark": "#F8FAFC",
        },
      },
      borderRadius: {
        // Matching web globals.css (--radius: 0.625rem = 10px)
        sm: 6, // calc(var(--radius) - 4px)
        md: 8, // calc(var(--radius) - 2px)
        lg: 10, // var(--radius)
        xl: 14, // calc(var(--radius) + 4px)
        "2xl": 18, // calc(var(--radius) + 8px)
        "3xl": 22, // calc(var(--radius) + 12px)
        "4xl": 26, // calc(var(--radius) + 16px)
      },
    },
  },
  plugins: [],
};

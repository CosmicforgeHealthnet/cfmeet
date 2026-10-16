import { Text as RNText, TextProps as RNTextProps } from "react-native";
import { cn } from "@/lib/utils/cn";

interface TextProps extends RNTextProps {
  variant?: "default" | "heading" | "subheading" | "muted" | "destructive";
  size?: "xs" | "sm" | "base" | "lg" | "xl" | "2xl" | "3xl";
}

const variantStyles: Record<NonNullable<TextProps["variant"]>, string> = {
  default: "text-foreground dark:text-foreground-dark",
  heading: "text-foreground dark:text-foreground-dark font-bold",
  subheading: "text-foreground dark:text-foreground-dark font-semibold",
  muted: "text-muted-foreground dark:text-muted-foreground-dark",
  destructive: "text-destructive dark:text-destructive-dark",
};

const sizeStyles: Record<NonNullable<TextProps["size"]>, string> = {
  xs: "text-xs",
  sm: "text-sm",
  base: "text-base",
  lg: "text-lg",
  xl: "text-xl",
  "2xl": "text-2xl",
  "3xl": "text-3xl",
};

export function Text({
  variant = "default",
  size = "base",
  className,
  ...props
}: TextProps) {
  return (
    <RNText
      className={cn(variantStyles[variant], sizeStyles[size], className)}
      {...props}
    />
  );
}

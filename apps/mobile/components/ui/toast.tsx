import { createContext, useContext, useState, useCallback, useRef, useEffect } from "react";
import { View, Animated, Pressable } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { cn } from "@/lib/utils/cn";
import { Text } from "./text";

type ToastVariant = "default" | "success" | "destructive";

interface Toast {
  id: string;
  message: string;
  variant: ToastVariant;
}

interface ToastContextType {
  toast: (message: string, variant?: ToastVariant) => void;
  success: (message: string) => void;
  error: (message: string) => void;
}

const ToastContext = createContext<ToastContextType | undefined>(undefined);

const variantStyles: Record<ToastVariant, string> = {
  default: "bg-card dark:bg-card-dark border-border dark:border-border-dark",
  success: "bg-success dark:bg-success-dark border-success dark:border-success-dark",
  destructive: "bg-destructive dark:bg-destructive-dark border-destructive dark:border-destructive-dark",
};

const textStyles: Record<ToastVariant, string> = {
  default: "text-foreground dark:text-foreground-dark",
  success: "text-success-foreground dark:text-success-foreground-dark",
  destructive: "text-destructive-foreground dark:text-destructive-foreground-dark",
};

interface ToastItemProps {
  toast: Toast;
  onDismiss: (id: string) => void;
}

function ToastItem({ toast, onDismiss }: ToastItemProps) {
  const fadeAnim = useRef(new Animated.Value(0)).current;
  const translateY = useRef(new Animated.Value(-20)).current;

  useEffect(() => {
    Animated.parallel([
      Animated.timing(fadeAnim, {
        toValue: 1,
        duration: 200,
        useNativeDriver: true,
      }),
      Animated.timing(translateY, {
        toValue: 0,
        duration: 200,
        useNativeDriver: true,
      }),
    ]).start();

    const timeout = setTimeout(() => {
      Animated.parallel([
        Animated.timing(fadeAnim, {
          toValue: 0,
          duration: 200,
          useNativeDriver: true,
        }),
        Animated.timing(translateY, {
          toValue: -20,
          duration: 200,
          useNativeDriver: true,
        }),
      ]).start(() => onDismiss(toast.id));
    }, 3000);

    return () => clearTimeout(timeout);
  }, [fadeAnim, translateY, toast.id, onDismiss]);

  return (
    <Animated.View
      style={{
        opacity: fadeAnim,
        transform: [{ translateY }],
      }}
    >
      <Pressable
        onPress={() => onDismiss(toast.id)}
        className={cn(
          "mb-2 rounded-lg border px-4 py-3 shadow-lg",
          variantStyles[toast.variant]
        )}
      >
        <Text className={cn("text-sm font-medium", textStyles[toast.variant])}>
          {toast.message}
        </Text>
      </Pressable>
    </Animated.View>
  );
}

interface ToastProviderProps {
  children: React.ReactNode;
}

export function ToastProvider({ children }: ToastProviderProps) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const insets = useSafeAreaInsets();

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const showToast = useCallback((message: string, variant: ToastVariant = "default") => {
    const id = Date.now().toString();
    setToasts((prev) => [...prev, { id, message, variant }]);
  }, []);

  const success = useCallback(
    (message: string) => showToast(message, "success"),
    [showToast]
  );

  const error = useCallback(
    (message: string) => showToast(message, "destructive"),
    [showToast]
  );

  return (
    <ToastContext.Provider value={{ toast: showToast, success, error }}>
      {children}
      <View
        style={{ top: insets.top + 10 }}
        className="absolute left-4 right-4 z-50"
        pointerEvents="box-none"
      >
        {toasts.map((t) => (
          <ToastItem key={t.id} toast={t} onDismiss={dismissToast} />
        ))}
      </View>
    </ToastContext.Provider>
  );
}

export function useToast(): ToastContextType {
  const context = useContext(ToastContext);
  if (!context) {
    throw new Error("useToast must be used within a ToastProvider");
  }
  return context;
}

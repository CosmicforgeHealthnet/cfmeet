import { useState } from "react";
import { View, Pressable } from "react-native";
import { Link } from "expo-router";
import { useForm, Controller } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { KeyboardAwareView } from "@/components/layout/keyboard-aware-view";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { AuthHeader, AuthCard, AuthInput, Divider, AuthBackground } from "@/components/auth";
import { useLogin } from "../hooks/use-login";
import { loginSchema, type LoginFormData } from "../schemas/auth.schema";

export function LoginForm() {
  const [rememberMe, setRememberMe] = useState(false);
  const [generalError, setGeneralError] = useState<string | null>(null);
  const { toast } = useToast();
  const login = useLogin();

  const {
    control,
    handleSubmit,
    formState: { errors },
  } = useForm<LoginFormData>({
    resolver: zodResolver(loginSchema),
    defaultValues: {
      email: "",
      password: "",
      remember_me: false,
    },
  });

  const onSubmit = (data: LoginFormData) => {
    setGeneralError(null);
    login.mutate(
      { ...data, remember_me: rememberMe },
      {
        onError: (error) => {
          setGeneralError(error.message);
          toast(error.message, "destructive");
        },
      }
    );
  };

  return (
    <AuthBackground>
      <KeyboardAwareView contentClassName="justify-center px-4 py-6" transparent>
        <AuthCard>
        {/* Header */}
        <AuthHeader
          title="Welcome Back"
          subtitle="Log in to your account."
        />

        {/* General Error */}
        {generalError && (
          <View className="bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg px-4 py-2 mb-3">
            <Text className="text-destructive dark:text-destructive-dark text-sm">
              {generalError}
            </Text>
          </View>
        )}

        {/* Form Fields */}
        <View className="gap-3">
          <Controller
            control={control}
            name="email"
            render={({ field: { onChange, onBlur, value } }) => (
              <AuthInput
                label="Email"
                placeholder="you@example.com"
                keyboardType="email-address"
                autoCapitalize="none"
                autoComplete="email"
                value={value}
                onChangeText={onChange}
                onBlur={onBlur}
                error={errors.email?.message}
              />
            )}
          />

          <Controller
            control={control}
            name="password"
            render={({ field: { onChange, onBlur, value } }) => (
              <AuthInput
                label="Password"
                placeholder="Enter your password"
                secureTextEntry
                autoCapitalize="none"
                autoComplete="password"
                value={value}
                onChangeText={onChange}
                onBlur={onBlur}
                error={errors.password?.message}
              />
            )}
          />

          {/* Forgot Password */}
          <View className="items-end">
            <Link href="/forgot-password" asChild>
              <Pressable>
                <Text className="text-sm text-[#029CD4] font-semibold">
                  Forgot Password?
                </Text>
              </Pressable>
            </Link>
          </View>

          {/* Remember Me Checkbox */}
          <Pressable
            onPress={() => setRememberMe(!rememberMe)}
            className="flex-row items-center py-1"
          >
            <View
              className={`mr-3 h-5 w-5 items-center justify-center rounded border ${
                rememberMe
                  ? "border-[#029CD4] bg-[#029CD4]"
                  : "border-input dark:border-input-dark"
              }`}
            >
              {rememberMe && <Text className="text-xs text-white">✓</Text>}
            </View>
            <Text className="text-sm text-[#029CD4]">Remember Me</Text>
          </Pressable>

          {/* Main Log In Button */}
          <Button
            onPress={handleSubmit(onSubmit)}
            isLoading={login.isPending}
            size="lg"
            className="w-full"
          >
            Log In
          </Button>
        </View>

        {/* Signup Redirect */}
        <View className="flex-row items-center justify-center mt-2">
          <Text className="text-xs text-foreground dark:text-foreground-dark">
            Don't have an account?{" "}
          </Text>
          <Link href="/signup" asChild>
            <Pressable>
              <Text className="text-sm font-semibold text-[#029CD4]">
                Sign Up
              </Text>
            </Pressable>
          </Link>
        </View>

        {/* Divider */}
        <Divider text="OR" />

        {/* Google Sign In */}
        <Button
          variant="outline"
          className="w-full rounded-2xl"
          onPress={() => {
            // TODO: Implement Google OAuth for mobile
            toast("Google sign-in coming soon", "default");
          }}
        >
          <View className="flex-row items-center justify-center gap-2">
            <Text className="text-lg font-bold text-[#4285F4]">G</Text>
            <Text className="text-muted-foreground dark:text-muted-foreground-dark">
              Continue with Google
            </Text>
          </View>
        </Button>
        </AuthCard>
      </KeyboardAwareView>
    </AuthBackground>
  );
}

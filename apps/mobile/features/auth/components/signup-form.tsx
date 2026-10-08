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
import { useRegister } from "../hooks/use-register";
import { registerSchema, type RegisterFormData } from "../schemas/auth.schema";

export function SignupForm() {
  const [agreed, setAgreed] = useState(false);
  const [termsError, setTermsError] = useState<string | null>(null);
  const [generalError, setGeneralError] = useState<string | null>(null);
  const { toast } = useToast();
  const register = useRegister();

  const {
    control,
    handleSubmit,
    formState: { errors },
  } = useForm<RegisterFormData>({
    resolver: zodResolver(registerSchema),
    defaultValues: {
      email: "",
      username: "",
      password: "",
      confirmPassword: "",
    },
  });

  const onSubmit = (data: RegisterFormData) => {
    if (!agreed) {
      setTermsError("You must accept the Terms & Conditions");
      return;
    }
    setTermsError(null);
    setGeneralError(null);

    register.mutate(
      {
        email: data.email,
        username: data.username,
        password: data.password,
      },
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
          title="Create Your Account"
          subtitle="Welcome! Let's get you Started."
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
            name="username"
            render={({ field: { onChange, onBlur, value } }) => (
              <AuthInput
                label="Username"
                placeholder="Enter your preferred Username"
                autoCapitalize="none"
                autoComplete="username"
                value={value}
                onChangeText={onChange}
                onBlur={onBlur}
                error={errors.username?.message}
              />
            )}
          />

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
                placeholder="Create a Password"
                secureTextEntry
                autoCapitalize="none"
                value={value}
                onChangeText={onChange}
                onBlur={onBlur}
                error={errors.password?.message}
              />
            )}
          />

          <Controller
            control={control}
            name="confirmPassword"
            render={({ field: { onChange, onBlur, value } }) => (
              <AuthInput
                label="Confirm Password"
                placeholder="Confirm your password"
                secureTextEntry
                autoCapitalize="none"
                value={value}
                onChangeText={onChange}
                onBlur={onBlur}
                error={errors.confirmPassword?.message}
              />
            )}
          />

          {/* Terms Checkbox */}
          <View>
            <Pressable
              onPress={() => {
                setAgreed(!agreed);
                setTermsError(null);
              }}
              className="flex-row items-center py-1"
            >
              <View
                className={`mr-3 h-5 w-5 items-center justify-center rounded border ${
                  agreed
                    ? "border-[#029CD4] bg-[#029CD4]"
                    : "border-input dark:border-input-dark"
                }`}
              >
                {agreed && <Text className="text-xs text-white">✓</Text>}
              </View>
              <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
                I accept the{" "}
                <Text className="text-[#029CD4] font-semibold">
                  Terms & Condition
                </Text>
              </Text>
            </Pressable>
            {termsError && (
              <Text className="text-xs text-destructive dark:text-destructive-dark mt-1">
                {termsError}
              </Text>
            )}
          </View>

          {/* Main Sign Up Button */}
          <Button
            onPress={handleSubmit(onSubmit)}
            isLoading={register.isPending}
            size="lg"
            className="w-full"
          >
            Sign Up
          </Button>
        </View>

        {/* Login Redirect */}
        <View className="flex-row items-center justify-center mt-2">
          <Text className="text-xs text-foreground dark:text-foreground-dark">
            Already have an account?{" "}
          </Text>
          <Link href="/login" asChild>
            <Pressable>
              <Text className="text-sm font-semibold text-[#029CD4]">
                Log In
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

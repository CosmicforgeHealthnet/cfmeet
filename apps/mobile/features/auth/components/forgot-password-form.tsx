import { useState, useRef } from "react";
import { View, TextInput, Pressable } from "react-native";
import { Link, router } from "expo-router";
import { useForm, Controller } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { KeyboardAwareView } from "@/components/layout/keyboard-aware-view";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { AuthHeader, AuthCard, AuthInput, AuthBackground } from "@/components/auth";
import {
  useForgotPassword,
  useVerifyResetToken,
  useResetPassword,
} from "../hooks/use-password-reset";
import {
  forgotPasswordSchema,
  resetPasswordSchema,
  type ForgotPasswordFormData,
  type ResetPasswordFormData,
} from "../schemas/auth.schema";

type Step = "email" | "code" | "password";

function maskEmail(email: string) {
  const [local, domain] = email.split("@");
  const visible = local.slice(0, 2);
  return `${visible}${"*".repeat(Math.max(local.length - 2, 0))}@${domain}`;
}

export function ForgotPasswordForm() {
  const [step, setStep] = useState<Step>("email");
  const [email, setEmail] = useState("");
  const [resetToken, setResetToken] = useState("");
  const [code, setCode] = useState(["", "", "", "", "", ""]);
  const [error, setError] = useState<string | null>(null);
  const [resendMessage, setResendMessage] = useState<string | null>(null);
  const inputRefs = useRef<(TextInput | null)[]>([]);
  const { toast, success } = useToast();

  const forgotPassword = useForgotPassword();
  const verifyResetToken = useVerifyResetToken();
  const resetPassword = useResetPassword();

  const emailForm = useForm<ForgotPasswordFormData>({
    resolver: zodResolver(forgotPasswordSchema),
    defaultValues: { email: "" },
  });

  const passwordForm = useForm<ResetPasswordFormData>({
    resolver: zodResolver(resetPasswordSchema),
    defaultValues: { password: "", confirmPassword: "" },
  });

  const handleEmailSubmit = (data: ForgotPasswordFormData) => {
    setError(null);
    forgotPassword.mutate(data, {
      onSuccess: () => {
        setEmail(data.email);
        setStep("code");
        success("Reset code sent to your email");
      },
      onError: (err) => {
        setError(err.message);
        toast(err.message, "destructive");
      },
    });
  };

  const handleCodeChange = (value: string, index: number) => {
    setError(null);

    if (value.length > 1) {
      const chars = value.split("");
      const newCode = [...code];
      chars.forEach((char, i) => {
        if (index + i < 6) {
          newCode[index + i] = char;
        }
      });
      setCode(newCode);
      const nextIndex = Math.min(index + chars.length, 5);
      inputRefs.current[nextIndex]?.focus();
      return;
    }

    const newCode = [...code];
    newCode[index] = value;
    setCode(newCode);

    if (value && index < 5) {
      inputRefs.current[index + 1]?.focus();
    }
  };

  const handleKeyPress = (key: string, index: number) => {
    if (key === "Backspace" && !code[index] && index > 0) {
      inputRefs.current[index - 1]?.focus();
    }
  };

  const handleCodeSubmit = () => {
    const codeString = code.join("");
    if (codeString.length !== 6) {
      setError("Please enter the 6-digit code");
      return;
    }

    setError(null);
    verifyResetToken.mutate(
      { email, code: codeString },
      {
        onSuccess: (response) => {
          setResetToken(response.reset_token);
          setStep("password");
        },
        onError: (err) => {
          setError(err.message);
          toast(err.message, "destructive");
        },
      }
    );
  };

  const handleResend = () => {
    setResendMessage(null);
    setError(null);

    forgotPassword.mutate(
      { email },
      {
        onSuccess: () => {
          setResendMessage("A new code has been sent to your email");
          success("Reset code sent!");
        },
        onError: (err) => {
          setError(err.message);
          toast(err.message, "destructive");
        },
      }
    );
  };

  const handlePasswordSubmit = (data: ResetPasswordFormData) => {
    setError(null);
    resetPassword.mutate(
      {
        email,
        reset_token: resetToken,
        new_password: data.password,
      },
      {
        onSuccess: () => {
          success("Password reset successfully!");
          router.replace("/login");
        },
        onError: (err) => {
          setError(err.message);
          toast(err.message, "destructive");
        },
      }
    );
  };

  // Step 1: Email input
  if (step === "email") {
    return (
      <AuthBackground>
        <KeyboardAwareView contentClassName="justify-center px-4 py-6" transparent>
          <AuthCard>
          {/* Header */}
          <AuthHeader
            title="Forgot Password"
            subtitle="Enter your email to receive a reset code"
          />

          {/* Error */}
          {error && (
            <View className="bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg px-4 py-2 mb-3">
              <Text className="text-destructive dark:text-destructive-dark text-sm">
                {error}
              </Text>
            </View>
          )}

          {/* Form */}
          <View className="gap-3">
            <Controller
              control={emailForm.control}
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
                  error={emailForm.formState.errors.email?.message}
                />
              )}
            />

            <Button
              onPress={emailForm.handleSubmit(handleEmailSubmit)}
              isLoading={forgotPassword.isPending}
              size="lg"
              className="w-full mt-2"
            >
              Send Reset Code
            </Button>
          </View>

          {/* Back to Login */}
          <Link href="/login" asChild>
            <Pressable className="mt-4">
              <Text className="text-center text-[#029CD4]">Return to Log In Page</Text>
            </Pressable>
          </Link>
          </AuthCard>
        </KeyboardAwareView>
      </AuthBackground>
    );
  }

  // Step 2: OTP code input
  if (step === "code") {
    return (
      <AuthBackground>
        <KeyboardAwareView contentClassName="justify-center px-4 py-6" transparent>
          <AuthCard className="items-center">
          {/* Header */}
          <AuthHeader
            title="Verify Code"
            subtitle="Enter OTP code"
          />

          {/* Email info */}
          <View className="items-center mb-4">
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              We sent you a 6-digit code via your email
            </Text>
            <Text className="text-sm font-bold text-foreground dark:text-foreground-dark mt-1">
              {maskEmail(email)}
            </Text>
          </View>

          {/* OTP Input Group */}
          <View className="flex-row justify-between gap-2 mb-3 w-full">
            {code.map((digit, index) => (
              <TextInput
                key={index}
                ref={(ref) => {
                  inputRefs.current[index] = ref;
                }}
                value={digit}
                onChangeText={(value) => handleCodeChange(value, index)}
                onKeyPress={({ nativeEvent }) => handleKeyPress(nativeEvent.key, index)}
                keyboardType="number-pad"
                maxLength={1}
                className="h-10 w-11 rounded-sm bg-sky-100 dark:bg-sky-900/30 text-center text-xl font-semibold text-foreground dark:text-foreground-dark"
                selectTextOnFocus
              />
            ))}
          </View>

          {/* Error */}
          {error && (
            <View className="w-full bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg px-4 py-2 mb-3">
              <Text className="text-destructive dark:text-destructive-dark text-sm">
                {error}
              </Text>
            </View>
          )}

          {/* Success message */}
          {resendMessage && (
            <View className="w-full bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg px-4 py-2 mb-3">
              <Text className="text-green-600 dark:text-green-400 text-sm">
                {resendMessage}
              </Text>
            </View>
          )}

          {/* Resend */}
          <View className="flex-row items-center mb-3">
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              Didn't receive code?{" "}
            </Text>
            <Pressable onPress={handleResend} disabled={forgotPassword.isPending}>
              <Text className="text-sm font-bold text-[#029CD4]">Resend!</Text>
            </Pressable>
          </View>

          {/* Submit Button */}
          <Button
            onPress={handleCodeSubmit}
            isLoading={verifyResetToken.isPending}
            size="lg"
            className="w-full"
          >
            Submit Code
          </Button>

          {/* Use different email */}
          <Pressable onPress={() => setStep("email")} className="mt-3">
            <Text className="text-sm text-[#029CD4]">Use different email</Text>
          </Pressable>
          </AuthCard>
        </KeyboardAwareView>
      </AuthBackground>
    );
  }

  // Step 3: New password
  return (
    <AuthBackground>
      <KeyboardAwareView contentClassName="justify-center px-4 py-6" transparent>
        <AuthCard>
          {/* Header */}
        <AuthHeader
          title="Password Reset!"
          subtitle="Create New Password."
        />

        {/* Error */}
        {error && (
          <View className="bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg px-4 py-2 mb-3">
            <Text className="text-destructive dark:text-destructive-dark text-sm">
              {error}
            </Text>
          </View>
        )}

        {/* Form */}
        <View className="gap-3">
          <Controller
            control={passwordForm.control}
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
                error={passwordForm.formState.errors.password?.message}
              />
            )}
          />

          <Controller
            control={passwordForm.control}
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
                error={passwordForm.formState.errors.confirmPassword?.message}
              />
            )}
          />

          {/* Submit Button */}
          <Button
            onPress={passwordForm.handleSubmit(handlePasswordSubmit)}
            isLoading={resetPassword.isPending}
            size="lg"
            className="w-full mt-2"
          >
            Submit
          </Button>
        </View>

        {/* Back to Login */}
        <Link href="/login" asChild>
          <Pressable className="mt-4">
            <Text className="text-center text-[#029CD4]">Return to Log In Page</Text>
          </Pressable>
        </Link>
        </AuthCard>
      </KeyboardAwareView>
    </AuthBackground>
  );
}

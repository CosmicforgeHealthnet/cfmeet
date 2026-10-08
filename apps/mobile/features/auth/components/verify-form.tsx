import { useState, useRef, useEffect } from "react";
import { View, TextInput, Pressable } from "react-native";
import { useLocalSearchParams, Link } from "expo-router";
import { KeyboardAwareView } from "@/components/layout/keyboard-aware-view";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { useToast } from "@/components/ui/toast";
import { AuthHeader, AuthCard, AuthBackground } from "@/components/auth";
import { useVerifyEmail, useResendVerification } from "../hooks/use-verify-email";

function maskEmail(email: string) {
  const [local, domain] = email.split("@");
  const visible = local.slice(0, 2);
  return `${visible}${"*".repeat(Math.max(local.length - 2, 0))}@${domain}`;
}

export function VerifyForm() {
  const { email } = useLocalSearchParams<{ email: string }>();
  const [code, setCode] = useState(["", "", "", "", "", ""]);
  const [countdown, setCountdown] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [resendMessage, setResendMessage] = useState<string | null>(null);
  const inputRefs = useRef<(TextInput | null)[]>([]);
  const { toast, success } = useToast();

  const verifyEmail = useVerifyEmail();
  const resendVerification = useResendVerification();

  useEffect(() => {
    if (countdown > 0) {
      const timer = setTimeout(() => setCountdown(countdown - 1), 1000);
      return () => clearTimeout(timer);
    }
  }, [countdown]);

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

  const handleVerify = () => {
    const codeString = code.join("");
    if (codeString.length !== 6) {
      setError("Please enter the 6-digit code");
      return;
    }

    if (!email) {
      setError("Email is missing. Please go back and try again.");
      return;
    }

    setError(null);
    verifyEmail.mutate(
      { email, code: codeString },
      {
        onError: (err) => {
          setError(err.message);
          toast(err.message, "destructive");
        },
      }
    );
  };

  const handleResend = () => {
    if (!email) return;

    setResendMessage(null);
    setError(null);

    resendVerification.mutate(email, {
      onSuccess: () => {
        setResendMessage("A new code has been sent to your email");
        success("Verification code sent!");
        setCountdown(60);
      },
      onError: (err) => {
        setError(err.message);
        toast(err.message, "destructive");
      },
    });
  };

  return (
    <AuthBackground>
      <KeyboardAwareView contentClassName="justify-center px-4 py-6" transparent>
        <AuthCard className="items-center">
        {/* Header */}
        <AuthHeader
          title="Verify your Email"
          subtitle="Enter OTP code"
        />

        {/* Email info */}
        <View className="items-center mb-4">
          <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
            We sent you a 6-digit code via your email
          </Text>
          <Text className="text-sm font-bold text-foreground dark:text-foreground-dark mt-1">
            {email ? maskEmail(email) : ""}
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
          {countdown > 0 ? (
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              Resend in {countdown}s
            </Text>
          ) : (
            <Pressable onPress={handleResend} disabled={resendVerification.isPending}>
              <Text className="text-sm font-bold text-primary dark:text-primary-dark">Resend!</Text>
            </Pressable>
          )}
        </View>

        {/* Submit Button */}
        <Button
          onPress={handleVerify}
          isLoading={verifyEmail.isPending}
          size="lg"
          className="w-full"
        >
          Submit Code
        </Button>

        {/* Return to Login */}
        <Link href="/login" asChild>
          <Pressable className="mt-3">
            <Text className="text-sm text-primary dark:text-primary-dark">Return to Log In Page</Text>
          </Pressable>
        </Link>
        </AuthCard>
      </KeyboardAwareView>
    </AuthBackground>
  );
}

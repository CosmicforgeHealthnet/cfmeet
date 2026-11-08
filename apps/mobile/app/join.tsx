/**
 * Join by meeting code or link — signed in or as a guest. Deep links:
 * `cfmeet://join?code=<code>[&invitation=<token>]`.
 */
import { useEffect, useRef, useState } from "react";
import { View } from "react-native";
import { router, useLocalSearchParams } from "expo-router";
import { KeyboardAwareView } from "@/components/layout/keyboard-aware-view";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { useOpenMeeting } from "@/features/meetings/open-meeting";
import { useAuthStore } from "@/store/auth.store";

export default function JoinScreen() {
  const params = useLocalSearchParams<{ code?: string; invitation?: string }>();
  const { isAuthenticated } = useAuthStore();
  const [code, setCode] = useState(params.code ?? "");
  const { open, pending, error } = useOpenMeeting();
  const autoOpened = useRef(false);

  useEffect(() => {
    if (params.code && !autoOpened.current) {
      autoOpened.current = true;
      void open(params.code, params.invitation);
    }
  }, [params.code, params.invitation, open]);

  return (
    <KeyboardAwareView contentClassName="justify-center px-6 py-8">
      <View className="mb-8 items-center">
        <Text className="text-3xl font-bold">Join a meeting</Text>
        <Text variant="muted" className="mt-2 text-center">
          Enter the meeting code or paste the meeting link.
        </Text>
      </View>
      <View className="gap-4">
        <Input
          label="Meeting code or link"
          placeholder="abc-defg-hij"
          value={code}
          onChangeText={setCode}
          autoCapitalize="none"
          autoCorrect={false}
          returnKeyType="go"
          onSubmitEditing={() => void open(code, params.invitation)}
          error={error ?? undefined}
        />
        <Button
          size="lg"
          className="mt-2"
          isLoading={pending}
          disabled={!code.trim()}
          onPress={() => void open(code, params.invitation)}
        >
          Continue
        </Button>
        <Button
          variant="ghost"
          onPress={() => (router.canGoBack() ? router.back() : router.replace(isAuthenticated ? "/(tabs)" : "/login"))}
        >
          Cancel
        </Button>
      </View>
    </KeyboardAwareView>
  );
}

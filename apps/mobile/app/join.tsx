import { useState } from "react";
import { View } from "react-native";
import { router } from "expo-router";
import { KeyboardAwareView } from "@/components/layout/keyboard-aware-view";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";

export default function JoinScreen() {
  const [meetingCode, setMeetingCode] = useState("");

  const handleJoin = () => {
    if (!meetingCode.trim()) return;

    router.push({
      pathname: "/(room)/[id]",
      params: { id: meetingCode.trim() },
    });
  };

  return (
    <KeyboardAwareView contentClassName="justify-center px-6 py-8">
      <View className="mb-8 items-center">
        <Text className="text-3xl font-bold text-foreground">Join Meeting</Text>
        <Text className="mt-2 text-center text-muted-foreground">
          Enter a meeting code to join
        </Text>
      </View>

      <View className="gap-4">
        <Input
          label="Meeting Code"
          placeholder="Enter meeting code"
          value={meetingCode}
          onChangeText={setMeetingCode}
          autoCapitalize="none"
          returnKeyType="go"
          onSubmitEditing={handleJoin}
        />

        <Button
          onPress={handleJoin}
          disabled={!meetingCode.trim()}
          className="mt-4"
          size="lg"
        >
          Join
        </Button>

        <Button
          variant="ghost"
          onPress={() => router.back()}
          className="mt-2"
        >
          Cancel
        </Button>
      </View>
    </KeyboardAwareView>
  );
}

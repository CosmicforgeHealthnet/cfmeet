import { useState } from "react";
import { View } from "react-native";
import { router } from "expo-router";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";

export function JoinMeetingInput() {
  const [meetingCode, setMeetingCode] = useState("");

  const handleJoin = () => {
    if (!meetingCode.trim()) return;

    router.push({
      pathname: "/(room)/[id]",
      params: { id: meetingCode.trim() },
    });
  };

  return (
    <View className="flex-row gap-2">
      <View className="flex-1">
        <Input
          placeholder="Enter meeting code"
          value={meetingCode}
          onChangeText={setMeetingCode}
          autoCapitalize="none"
          returnKeyType="go"
          onSubmitEditing={handleJoin}
        />
      </View>
      <Button onPress={handleJoin} disabled={!meetingCode.trim()}>
        Join
      </Button>
    </View>
  );
}

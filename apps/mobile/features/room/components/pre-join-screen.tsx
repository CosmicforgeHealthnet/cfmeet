import { useState, useEffect } from "react";
import { View, Pressable, Platform } from "react-native";
import { Camera, CameraView } from "expo-camera";
import { Ionicons } from "@expo/vector-icons";
import { Screen } from "@/components/layout/screen";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Avatar } from "@/components/ui/avatar";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { usePublicMeeting } from "@/features/meetings/hooks/use-meetings";
import { useJoinMeeting } from "@/features/meetings/hooks/use-join-meeting";
import { useAuthStore } from "@/store/auth.store";

interface PreJoinScreenProps {
  meetingIdentifier: string;
}

export function PreJoinScreen({ meetingIdentifier }: PreJoinScreenProps) {
  const [displayName, setDisplayName] = useState("");
  const [isAudioEnabled, setIsAudioEnabled] = useState(true);
  const [isVideoEnabled, setIsVideoEnabled] = useState(true);
  const [hasCameraPermission, setHasCameraPermission] = useState(false);
  const [isCheckingPermissions, setIsCheckingPermissions] = useState(true);

  const { toast } = useToast();
  const { user, isAuthenticated } = useAuthStore();
  const { data: meeting, isLoading, isError } = usePublicMeeting(meetingIdentifier);
  const joinMeeting = useJoinMeeting();

  useEffect(() => {
    if (user?.username) {
      setDisplayName(user.username);
    }
  }, [user]);

  useEffect(() => {
    const checkPermissions = async () => {
      try {
        const cameraStatus = await Camera.requestCameraPermissionsAsync();
        setHasCameraPermission(cameraStatus.status === "granted");
      } catch {
        setHasCameraPermission(false);
      } finally {
        setIsCheckingPermissions(false);
      }
    };

    checkPermissions();
  }, []);

  const handleJoin = () => {
    if (!displayName.trim()) {
      toast("Please enter your display name", "destructive");
      return;
    }

    joinMeeting.mutate(
      {
        identifier: meetingIdentifier,
        data: {
          display_name: displayName.trim(),
          is_audio_enabled: isAudioEnabled,
          is_video_enabled: isVideoEnabled,
        },
        isGuest: !isAuthenticated,
      },
      {
        onError: (error) => {
          toast(error.message, "destructive");
        },
      }
    );
  };

  if (isLoading || isCheckingPermissions) {
    return <Loading fullScreen message="Loading meeting..." />;
  }

  if (isError || !meeting) {
    return (
      <Screen className="items-center justify-center px-6">
        <Ionicons name="alert-circle-outline" size={64} color="#029CD4" style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-xl font-bold text-foreground dark:text-foreground-dark">Meeting Not Found</Text>
        <Text className="mt-2 text-center text-muted-foreground dark:text-muted-foreground-dark">
          The meeting you're looking for doesn't exist or has ended.
        </Text>
      </Screen>
    );
  }

  if (meeting.status === "ended") {
    return (
      <Screen className="items-center justify-center px-6">
        <Ionicons name="checkmark-circle-outline" size={64} color="#029CD4" style={{ opacity: 0.5 }} />
        <Text className="mt-4 text-xl font-bold text-foreground dark:text-foreground-dark">Meeting Ended</Text>
        <Text className="mt-2 text-center text-muted-foreground dark:text-muted-foreground-dark">
          This meeting has already ended.
        </Text>
      </Screen>
    );
  }

  return (
    <Screen edges={["top"]} className="px-6">
      <View className="flex-1">
        <View className="mb-6 mt-4">
          <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">{meeting.title}</Text>
          <Text className="mt-1 text-muted-foreground dark:text-muted-foreground-dark">
            Hosted by {meeting.host_username}
          </Text>
        </View>

        <View className="mb-6 aspect-video overflow-hidden rounded-2xl bg-muted dark:bg-muted-dark">
          {hasCameraPermission && isVideoEnabled && Platform.OS !== "web" ? (
            <CameraView style={{ flex: 1 }} facing="front" />
          ) : (
            <View className="flex-1 items-center justify-center">
              <Avatar fallback={displayName || "You"} size="xl" />
              {!hasCameraPermission && Platform.OS !== "web" && (
                <Text className="mt-4 text-sm text-muted-foreground dark:text-muted-foreground-dark">
                  Camera permission required
                </Text>
              )}
            </View>
          )}
        </View>

        <View className="mb-6 flex-row justify-center gap-4">
          <Pressable
            onPress={() => setIsAudioEnabled(!isAudioEnabled)}
            className={`h-14 w-14 items-center justify-center rounded-full ${
              isAudioEnabled ? "bg-primary dark:bg-primary-dark" : "bg-destructive dark:bg-destructive-dark"
            }`}
          >
            <Ionicons
              name={isAudioEnabled ? "mic" : "mic-off"}
              size={24}
              color="#ffffff"
            />
          </Pressable>

          <Pressable
            onPress={() => setIsVideoEnabled(!isVideoEnabled)}
            className={`h-14 w-14 items-center justify-center rounded-full ${
              isVideoEnabled ? "bg-primary dark:bg-primary-dark" : "bg-destructive dark:bg-destructive-dark"
            }`}
          >
            <Ionicons
              name={isVideoEnabled ? "videocam" : "videocam-off"}
              size={24}
              color="#ffffff"
            />
          </Pressable>
        </View>

        <Input
          label="Your Name"
          placeholder="Enter your display name"
          value={displayName}
          onChangeText={setDisplayName}
          autoCapitalize="words"
        />

        {meeting.is_private && (
          <View className="mt-4 rounded-lg bg-muted dark:bg-muted-dark p-3">
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              This is a private meeting. The host will need to admit you.
            </Text>
          </View>
        )}
      </View>

      <View className="pb-6">
        <Button
          onPress={handleJoin}
          isLoading={joinMeeting.isPending}
          size="lg"
          className="w-full"
        >
          Join Meeting
        </Button>
      </View>
    </Screen>
  );
}

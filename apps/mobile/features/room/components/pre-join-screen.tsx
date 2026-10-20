import { useState, useEffect, useCallback, useRef } from "react";
import { View, Pressable, Platform } from "react-native";
import { Camera, CameraView } from "expo-camera";
import { router } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Screen } from "@/components/layout/screen";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Avatar } from "@/components/ui/avatar";
import { Logo } from "@/components/ui/logo";
import { FullScreenLoading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useResolve, useJoinConference } from "@/features/meetings/v2/hooks";
import { useAuthStore } from "@/store/auth.store";
import { useColors } from "@/hooks/useColors";
import type { JoinConferenceResponse } from "@/features/meetings/v2/types";

interface PreJoinScreenProps {
  meetingIdentifier: string;
}

export function PreJoinScreen({ meetingIdentifier }: PreJoinScreenProps) {
  const [displayName, setDisplayName] = useState("");
  const [isAudioEnabled, setIsAudioEnabled] = useState(true);
  const [isVideoEnabled, setIsVideoEnabled] = useState(true);
  const [hasCameraPermission, setHasCameraPermission] = useState(false);
  const [isCheckingPermissions, setIsCheckingPermissions] = useState(true);
  const [isWaiting, setIsWaiting] = useState(false);
  const pollIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const { toast } = useToast();
  const { user, isAuthenticated } = useAuthStore();
  const { data: resolveData, isLoading, isError } = useResolve(meetingIdentifier);
  const joinConference = useJoinConference();
  const colors = useColors();

  // Helper to map V2 join response to legacy format for LiveRoom
  const mapV2JoinToLegacy = useCallback((v2Response: JoinConferenceResponse) => {
    if (!v2Response.rtc) {
      return null; // User is in waiting room
    }
    const rtcCredential = v2Response.rtc.credential;
    return {
      meeting_id: v2Response.conference.meetingSpaceId,
      participant_id: v2Response.participant.id,
      role: v2Response.participant.role.toLowerCase(),
      join_token: rtcCredential,
      livekit_url: v2Response.rtc.url,
      room_name: v2Response.conference.id,
    };
  }, []);

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

  const navigateToLiveRoom = useCallback(
    (joinData: NonNullable<ReturnType<typeof mapV2JoinToLegacy>>) => {
      router.replace({
        pathname: "/(room)/[id]/live",
        params: {
          id: meetingIdentifier,
          joinData: JSON.stringify(joinData),
        },
      });
    },
    [meetingIdentifier]
  );

  // Poll to check if participant has been admitted to the meeting.
  // Re-joins via the V2 API; once the host admits us, the response
  // includes RTC credentials and we proceed into the room.
  const startPolling = useCallback(
    (conferenceId: string, name: string) => {
      if (pollIntervalRef.current) {
        clearInterval(pollIntervalRef.current);
      }

      pollIntervalRef.current = setInterval(async () => {
        try {
          const response = await joinConference.mutateAsync({
            conferenceId,
            data: { displayName: name },
            isGuest: !isAuthenticated,
          });

          const joinData = mapV2JoinToLegacy(response);
          if (joinData) {
            if (pollIntervalRef.current) {
              clearInterval(pollIntervalRef.current);
              pollIntervalRef.current = null;
            }
            setIsWaiting(false);
            navigateToLiveRoom(joinData);
          }
        } catch {
          // Still in waiting room or temporary network error - keep polling
        }
      }, 3000);
    },
    [joinConference, isAuthenticated, mapV2JoinToLegacy, navigateToLiveRoom]
  );

  // Cleanup polling interval on unmount
  useEffect(() => {
    return () => {
      if (pollIntervalRef.current) {
        clearInterval(pollIntervalRef.current);
      }
    };
  }, []);

  const handleJoin = () => {
    if (!displayName.trim()) {
      toast("Please enter your display name", "destructive");
      return;
    }

    const conferenceId = resolveData?.conference?.id;
    if (!conferenceId) {
      toast("No active conference found for this meeting", "destructive");
      return;
    }

    const name = displayName.trim();

    joinConference.mutate(
      {
        conferenceId,
        data: {
          displayName: name,
        },
        isGuest: !isAuthenticated,
      },
      {
        onSuccess: (response) => {
          const joinData = mapV2JoinToLegacy(response);
          if (joinData) {
            navigateToLiveRoom(joinData);
          } else {
            // User is in waiting room - poll until the host admits them
            setIsWaiting(true);
            startPolling(conferenceId, name);
          }
        },
        onError: (error) => {
          toast(error.message, "destructive");
        },
      }
    );
  };

  const handleCancel = () => {
    if (pollIntervalRef.current) {
      clearInterval(pollIntervalRef.current);
      pollIntervalRef.current = null;
    }
    setIsWaiting(false);
  };

  if (isLoading || isCheckingPermissions) {
    return <FullScreenLoading message="Loading meeting..." />;
  }

  if (isError || !resolveData?.meetingSpace) {
    return (
      <Screen className="items-center justify-center px-6 gap-6">
        <Logo variant="large" />

        <View className="w-20 h-20 rounded-full bg-destructive/10 dark:bg-destructive-dark/10 items-center justify-center">
          <Ionicons name="close-circle" size={40} color={colors.destructive} />
        </View>

        <View className="items-center gap-2">
          <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">Meeting Not Found</Text>
          <Text className="text-center text-muted-foreground dark:text-muted-foreground-dark">
            The meeting <Text className="font-medium text-foreground dark:text-foreground-dark">{meetingIdentifier}</Text> doesn&apos;t exist or you don&apos;t have access to it.
          </Text>
        </View>

        <Button onPress={() => router.replace("/(tabs)")} size="lg" className="w-full max-w-xs">
          Back to Home
        </Button>
      </Screen>
    );
  }

  const conferenceStatus = resolveData.conference?.status;
  if (conferenceStatus === "ENDED" || conferenceStatus === "CANCELLED") {
    return (
      <Screen className="items-center justify-center px-6 gap-6">
        <Logo variant="large" />

        <View className="w-20 h-20 rounded-full bg-muted dark:bg-muted-dark items-center justify-center">
          <Ionicons name="time" size={40} color={colors.mutedForeground} />
        </View>

        <View className="items-center gap-2">
          <Text className="text-2xl font-bold text-foreground dark:text-foreground-dark">Meeting Has Ended</Text>
          <Text className="text-center text-muted-foreground dark:text-muted-foreground-dark">
            {resolveData.meetingSpace.title ? (
              <>The meeting <Text className="font-medium text-foreground dark:text-foreground-dark">&quot;{resolveData.meetingSpace.title}&quot;</Text> has ended.</>
            ) : (
              <>The meeting <Text className="font-medium text-foreground dark:text-foreground-dark">{meetingIdentifier}</Text> has ended.</>
            )}
          </Text>
        </View>

        <Button onPress={() => router.replace("/(tabs)")} size="lg" className="w-full max-w-xs">
          Back to Home
        </Button>
      </Screen>
    );
  }

  const meetingSpace = resolveData.meetingSpace;

  return (
    <Screen edges={["top"]} className="px-6">
      <View className="flex-1">
        {/* Logo */}
        <View className="items-center mt-4 mb-4">
          <Logo variant="medium" />
        </View>

        {/* Meeting Info */}
        <View className="mb-4">
          <Text className="text-xl font-bold text-foreground dark:text-foreground-dark text-center">{meetingSpace.title || "Meeting"}</Text>
          <Text className="mt-1 text-muted-foreground dark:text-muted-foreground-dark text-center">
            Code: {meetingSpace.meetingCode}
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

        {!isWaiting && (
          <Input
            label="Your Name"
            placeholder="Enter your display name"
            value={displayName}
            onChangeText={setDisplayName}
            autoCapitalize="words"
          />
        )}

        {meetingSpace.accessPolicy !== "PUBLIC" && (
          <View className="mt-4 rounded-lg bg-muted dark:bg-muted-dark p-3">
            <Text className="text-sm text-muted-foreground dark:text-muted-foreground-dark">
              {meetingSpace.requiresPasscode
                ? "This meeting requires a passcode."
                : "This is a private meeting. The host may need to admit you."}
            </Text>
          </View>
        )}
      </View>

      <View className="pb-6">
        {isWaiting ? (
          <View className="items-center gap-3">
            <View className="flex-row items-center gap-2">
              <View className="h-3 w-3 rounded-full bg-primary" />
              <Text className="text-sm font-medium text-foreground dark:text-foreground-dark">
                Waiting for the host to let you in...
              </Text>
            </View>
            <Button onPress={handleCancel} variant="ghost" size="sm">
              Cancel
            </Button>
          </View>
        ) : (
          <Button
            onPress={handleJoin}
            isLoading={joinConference.isPending}
            size="lg"
            className="w-full"
          >
            Join Meeting
          </Button>
        )}
      </View>
    </Screen>
  );
}

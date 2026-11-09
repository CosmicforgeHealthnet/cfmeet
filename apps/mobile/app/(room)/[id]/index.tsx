/**
 * Pre-join for one conference (`id` = conference id). See features/room/use-join-flow.ts.
 */
import { useEffect, useMemo, useState } from "react";
import { ActivityIndicator, ScrollView, Switch, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { router, useLocalSearchParams } from "expo-router";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ScreenHeader, Notice } from "@/components/shared/screen-header";
import { ErrorState } from "@/components/shared/error-state";
import { useColors } from "@/hooks/useColors";
import { useAuthStore } from "@/store/auth.store";
import { useJoinFlow } from "@/features/room/use-join-flow";
import { decodeApiError } from "@/lib/api/errors";
import type { ConsentKind } from "@/lib/api/generated/meeting-service";

const CONSENT_TEXT: Record<ConsentKind, string> = {
  RECORDING: "I agree that this meeting may be recorded.",
  TRANSCRIPTION: "I agree that this meeting may be transcribed.",
  AI_PROCESSING: "I agree to AI processing of this meeting.",
  DATA_SHARING: "I agree to the meeting's data sharing terms.",
  HEALTHCARE_TERMS: "I accept the healthcare consultation terms.",
  PRIVACY_NOTICE: "I have read the privacy notice.",
};

const HOST_ROLES = ["OWNER", "HOST", "CO_HOST"];

export default function PreJoinScreen() {
  const params = useLocalSearchParams<{ id: string; title?: string; code?: string; invitation?: string }>();
  const conferenceId = params.id;
  const colors = useColors();
  const { user, isAuthenticated } = useAuthStore();
  const { preJoin, phase, join, start, cancelWaiting, reset } = useJoinFlow(conferenceId, {
    title: params.title ?? null,
    meetingCode: params.code ?? null,
  });

  const data = preJoin.data;
  const [displayName, setDisplayName] = useState("");
  const [passcode, setPasscode] = useState("");
  const [granted, setGranted] = useState<Record<string, boolean>>({});
  const [micOn, setMicOn] = useState(true);
  const [camOn, setCamOn] = useState(true);
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    if (data && !displayName) setDisplayName(data.displayName.suggested ?? user?.username ?? "");
  }, [data, displayName, user?.username]);

  useEffect(() => {
    if (phase.kind === "live") {
      router.replace({
        pathname: "/(room)/[id]/live",
        params: { id: conferenceId, mic: micOn ? "1" : "0", cam: camOn ? "1" : "0" },
      });
    }
  }, [phase, conferenceId, micOn, camOn]);

  const required = useMemo(
    () => (data ? data.consents.required.filter((c) => !data.consents.granted.includes(c)) : []),
    [data]
  );
  const consentsComplete = required.every((c) => granted[c]);
  const passcodeNeeded = !!data && data.access.passcodeRequired && !data.access.alreadyAuthorized;
  const signInNeeded = !!data && data.access.authenticationRequired && !isAuthenticated;

  if (preJoin.isLoading) {
    return (
      <SafeAreaView className="flex-1 items-center justify-center bg-background dark:bg-background-dark">
        <ActivityIndicator color={colors.primary} />
        <Text variant="muted" className="mt-3">
          Loading meeting…
        </Text>
      </SafeAreaView>
    );
  }

  if (preJoin.isError || !data) {
    const err = decodeApiError(preJoin.error);
    return (
      <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
        <ScreenHeader title="Join meeting" />
        <ErrorState
          title={err.kind === "not_found" ? "Meeting not found" : "Can't open this meeting"}
          message={err.message}
          onRetry={() => preJoin.refetch()}
        />
      </SafeAreaView>
    );
  }

  const title = data.title ?? params.title ?? "Meeting";
  const terminal = ["ENDED", "CANCELLED", "FAILED"].includes(data.conferenceStatus);
  const notLive = data.conferenceStatus !== "LIVE" && !terminal;
  const isHost = HOST_ROLES.includes(data.media.role) && isAuthenticated;

  const onJoin = () =>
    join({
      displayName,
      passcode: passcodeNeeded ? passcode : undefined,
      invitationToken: params.invitation,
      consents: data.consents.required.map((type) => ({ type, version: data.consents.version })),
    });

  const busy = phase.kind === "joining" || phase.kind === "admitted_connecting";

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader
        title={title}
        subtitle={[data.organization?.name, data.organizer ? `Hosted by ${data.organizer.displayName}` : null]
          .filter(Boolean)
          .join(" · ")}
        onBack={phase.kind === "waiting" ? () => void cancelWaiting().then(() => router.back()) : undefined}
      />
      <ScrollView contentContainerStyle={{ padding: 24, paddingBottom: 48 }}>
        {phase.kind === "waiting" ? (
          <Card className="items-center py-10">
            <ActivityIndicator color={colors.primary} />
            <Text className="mt-4 text-lg font-bold">Waiting to be let in</Text>
            <Text variant="muted" className="mt-2 text-center">
              The host will admit you shortly. Keep this screen open.
            </Text>
            {phase.retrying && (
              <Text variant="muted" size="sm" className="mt-2 text-center">
                Connection problem — retrying…
              </Text>
            )}
            <Button variant="outline" className="mt-6" onPress={() => void cancelWaiting()}>
              Leave waiting room
            </Button>
          </Card>
        ) : phase.kind === "refused" ? (
          <Card className="items-center py-10">
            <Ionicons name="close-circle-outline" size={40} color={colors.destructive} />
            <Text className="mt-4 text-lg font-bold">You can't join</Text>
            <Text variant="muted" className="mt-2 text-center">
              {phase.reason}
            </Text>
            <Button variant="outline" className="mt-6" onPress={() => router.back()}>
              Back
            </Button>
          </Card>
        ) : terminal ? (
          <Notice
            title={data.conferenceStatus === "CANCELLED" ? "Meeting cancelled" : "Meeting ended"}
            body="This meeting is no longer available to join."
          />
        ) : notLive ? (
          <Card className="items-center py-8">
            <Ionicons name="time-outline" size={36} color={colors.primary} />
            <Text className="mt-3 text-lg font-bold">Not started yet</Text>
            <Text variant="muted" className="mt-2 text-center">
              {isHost
                ? "Start the meeting to let people join."
                : "You can join once the host starts the meeting. This screen updates automatically."}
            </Text>
            {isHost && (
              <Button
                className="mt-6"
                isLoading={starting}
                onPress={async () => {
                  setStarting(true);
                  await start();
                  setStarting(false);
                }}
              >
                Start meeting
              </Button>
            )}
          </Card>
        ) : (
          <View className="gap-5">
            {signInNeeded ? (
              <Notice
                tone="warning"
                title="Sign in required"
                body="This meeting is only for signed-in CF Meet users."
              />
            ) : null}
            <Input
              label="Your name"
              value={displayName}
              onChangeText={setDisplayName}
              editable={data.displayName.editable}
              maxLength={100}
            />
            {passcodeNeeded && (
              <Input
                label="Meeting passcode"
                value={passcode}
                onChangeText={setPasscode}
                secureTextEntry
                autoCapitalize="none"
              />
            )}
            <Card>
              <Row label="Microphone on" value={micOn} onChange={setMicOn} disabled={!data.media.permissions.audioPublish} />
              <Row label="Camera on" value={camOn} onChange={setCamOn} disabled={!data.media.permissions.videoPublish} />
            </Card>
            {(data.notices.recordingActive || data.notices.transcriptionActive || data.notices.aiNotice) && (
              <Notice
                title="Notice"
                body={[
                  data.notices.recordingActive ? "This meeting is being recorded." : null,
                  data.notices.transcriptionActive ? "This meeting is being transcribed." : null,
                  data.notices.aiNotice ? "AI features may process this meeting." : null,
                ]
                  .filter(Boolean)
                  .join(" ")}
              />
            )}
            {required.length > 0 && (
              <Card>
                {required.map((c) => (
                  <Row key={c} label={CONSENT_TEXT[c]} value={!!granted[c]} onChange={(v) => setGranted((g) => ({ ...g, [c]: v }))} />
                ))}
              </Card>
            )}
            {data.access.waitingRoomExpected && (
              <Text variant="muted" size="sm">
                The host may need to let you in from the waiting room.
              </Text>
            )}
            {phase.kind === "error" && (
              <Text variant="destructive" size="sm">
                {phase.message}
              </Text>
            )}
            <Button
              size="lg"
              isLoading={busy}
              disabled={busy || signInNeeded || !consentsComplete || !displayName.trim() || (passcodeNeeded && !passcode)}
              onPress={() => {
                if (phase.kind === "error") reset();
                onJoin();
              }}
            >
              {phase.kind === "admitted_connecting" ? "Connecting…" : "Join now"}
            </Button>
            {signInNeeded && (
              <Button variant="outline" onPress={() => router.push("/login")}>
                Sign in
              </Button>
            )}
          </View>
        )}
      </ScrollView>
    </SafeAreaView>
  );
}

function Row({
  label,
  value,
  onChange,
  disabled,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <View className="flex-row items-center justify-between py-2 gap-4">
      <Text className="flex-1">{label}</Text>
      <Switch value={value && !disabled} onValueChange={onChange} disabled={disabled} />
    </View>
  );
}

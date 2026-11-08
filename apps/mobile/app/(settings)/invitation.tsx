/**
 * Organization invitation accept / decline (ORG-016). The token comes from the invitation link
 * (`…/organizations/invitations#<token>`), the deep link
 * `cfmeet://invitation?token=<token>`, or a pasted link/token. It is sent only in the body of
 * `POST /api/v2/organization-invitations/accept|decline` and never logged.
 */
import { useState } from "react";
import { ScrollView } from "react-native";
import { router, useLocalSearchParams } from "expo-router";
import { SafeAreaView } from "react-native-safe-area-context";
import { Text } from "@/components/ui/text";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Notice, ScreenHeader } from "@/components/shared/screen-header";
import { useInvitationDecision } from "@/features/account/hooks";
import { useAuthStore } from "@/store/auth.store";
import { parseInvitationToken } from "@/lib/invitations";
import { decodeApiError } from "@/lib/api/errors";

export default function InvitationScreen() {
  const params = useLocalSearchParams<{ token?: string }>();
  const { isAuthenticated } = useAuthStore();
  const [input, setInput] = useState(params.token ?? "");
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const decision = useInvitationDecision();
  const token = parseInvitationToken(input);

  const decide = (accept: boolean) => {
    if (!token) return;
    setResult(null);
    decision.mutate(
      { token, accept },
      {
        onSuccess: () => {
          setResult({ ok: true, text: accept ? "Invitation accepted. The organization is now in your workspaces." : "Invitation declined." });
          setInput("");
        },
        onError: (e) => {
          const d = decodeApiError(e);
          setResult({
            ok: false,
            text:
              d.status === 410
                ? "This invitation has expired or was already used."
                : d.status === 404
                  ? "This invitation was not found."
                  : d.kind === "forbidden"
                    ? "This invitation was sent to a different email address."
                    : d.message,
          });
        },
      }
    );
  };

  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="Organization invitation" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 16 }}>
        {!isAuthenticated ? (
          <>
            <Notice title="Sign in first" body="Sign in with the email address the invitation was sent to, then open the link again." />
            <Button onPress={() => router.replace("/login")}>Sign in</Button>
          </>
        ) : (
          <>
            <Input
              label="Invitation link or code"
              value={input}
              onChangeText={setInput}
              autoCapitalize="none"
              autoCorrect={false}
              secureTextEntry={!!params.token}
              error={input && !token ? "This doesn't look like an invitation link." : undefined}
            />
            {result && (
              <Text variant={result.ok ? "default" : "destructive"} size="sm">
                {result.text}
              </Text>
            )}
            <Button disabled={!token} isLoading={decision.isPending && decision.variables?.accept} onPress={() => decide(true)}>
              Accept invitation
            </Button>
            <Button
              variant="outline"
              disabled={!token}
              isLoading={decision.isPending && decision.variables?.accept === false}
              onPress={() => decide(false)}
            >
              Decline
            </Button>
          </>
        )}
      </ScrollView>
    </SafeAreaView>
  );
}

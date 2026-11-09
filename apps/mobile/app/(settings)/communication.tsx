/**
 * Truthful status of calls, notifications and messaging security on mobile (Section 17).
 * Nothing here pretends a capability exists: each blocked item says why.
 */
import { ScrollView } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Notice, ScreenHeader } from "@/components/shared/screen-header";

export default function CommunicationStatusScreen() {
  return (
    <SafeAreaView className="flex-1 bg-background dark:bg-background-dark">
      <ScreenHeader title="Calls, chats & notifications" />
      <ScrollView contentContainerStyle={{ padding: 24, gap: 16 }}>
        <Notice
          title="Chats: managed conversations"
          body="On mobile you can read and send managed conversations. CF Meet can read managed messages; they are not end-to-end encrypted. Private (end-to-end encrypted) conversations open in CF Meet on the web."
        />
        <Notice
          tone="warning"
          title="Calls: not available on mobile yet"
          body="One-to-one and group calls are available in CF Meet on the web. The mobile app cannot place or receive calls yet. Meetings work on mobile."
        />
        <Notice
          tone="warning"
          title="Push notifications: not available yet"
          body="The app does not receive push notifications yet. Chats, meetings and waiting-room requests update while the app is open."
        />
      </ScrollView>
    </SafeAreaView>
  );
}

import { useLocalSearchParams } from "expo-router";
import { PreJoinScreen } from "@/features/room/components/pre-join-screen";

export default function PreJoinScreenRoute() {
  const { id } = useLocalSearchParams<{ id: string }>();

  return <PreJoinScreen meetingIdentifier={id ?? ""} />;
}

import MeetingSpaceWorkspace from "@/components/meeting-spaces/MeetingSpaceWorkspace";

export default async function MeetingSpacePage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <MeetingSpaceWorkspace id={id} />;
}

import EditMeetingContent from "@/components/dashboard/EditMeetingContent";

export default async function EditMeetingPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;
  return <EditMeetingContent meetingId={id} />;
}

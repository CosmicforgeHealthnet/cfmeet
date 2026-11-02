import ConferenceWorkspace from "@/components/conferences/ConferenceWorkspace";

export default async function ConferencePage({
  params,
}: {
  params: Promise<{ id: string; conferenceId: string }>;
}) {
  const { id, conferenceId } = await params;
  return <ConferenceWorkspace id={conferenceId} spaceId={id} />;
}

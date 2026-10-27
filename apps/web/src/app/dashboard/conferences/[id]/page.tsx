import ConferenceWorkspace from "@/components/conferences/ConferenceWorkspace";

export default async function ConferencePage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <ConferenceWorkspace id={id} />;
}

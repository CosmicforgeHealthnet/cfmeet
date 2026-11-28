import { Suspense } from "react";
import { notFound } from "next/navigation";
import ChannelPage from "@/components/updates/ChannelPage";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export default async function UpdatesChannelPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  if (!UUID.test(id)) notFound();
  return (
    <Suspense>
      <ChannelPage channelId={id} />
    </Suspense>
  );
}

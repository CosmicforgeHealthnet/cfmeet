import { notFound } from "next/navigation";
import ChannelAdminTools from "@/components/channels/ChannelAdminTools";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const TABS = ["details", "admins", "invites", "scheduled", "analytics", "verification", "audit"] as const;

export default async function ManageChannelPage({ params, searchParams }: { params: Promise<{ id: string }>; searchParams: Promise<{ tab?: string }> }) {
  const { id } = await params;
  const { tab } = await searchParams;
  if (!UUID.test(id)) notFound();
  const initial = TABS.find((t) => t === tab);
  return <ChannelAdminTools channelId={id} initialTab={initial} />;
}

import { notFound } from "next/navigation";
import OrganizationAdmin from "@/components/organizations/OrganizationAdmin";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export default async function OrganizationPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  if (!UUID.test(id)) notFound();
  return <OrganizationAdmin organizationId={id} />;
}

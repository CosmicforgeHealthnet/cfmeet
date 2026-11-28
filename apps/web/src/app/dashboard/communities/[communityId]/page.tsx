import CommunityPage from "@/components/communities/CommunityPage";

export default async function CommunityRoute({ params }: { params: Promise<{ communityId: string }> }) {
  const { communityId } = await params;
  return <CommunityPage key={communityId} communityId={communityId} />;
}

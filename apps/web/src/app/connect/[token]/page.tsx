import ConnectInvite from "@/components/communications/ConnectInvite";

/** Landing page of a Hub connect invite / QR payload (`{public app}/connect/{token}`). */
export default async function ConnectPage({ params }: { params: Promise<{ token: string }> }) {
  const { token } = await params;
  return <ConnectInvite token={token} />;
}

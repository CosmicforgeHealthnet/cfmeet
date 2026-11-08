"use client";

import { useEffect, useState } from "react";
import { useParams } from "next/navigation";
import ConferenceRoom from "@/components/room/ConferenceRoom";

/** `/conferences/{conferenceId}`: pre-join, waiting and live room of one conference (FE-017). */
export default function ConferenceRoomPage() {
  const params = useParams();
  const conferenceId = params?.conferenceId as string;
  // An invitation token may arrive in the fragment (never sent to the server); read it once.
  const [token, setToken] = useState<string | undefined>();
  const [ready, setReady] = useState(false);
  useEffect(() => {
    const fragment = window.location.hash.slice(1);
    if (fragment) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setToken(decodeURIComponent(fragment));
      window.history.replaceState(window.history.state, "", window.location.pathname);
    }
    setReady(true);
  }, []);
  if (!conferenceId || !ready) return null;
  return <ConferenceRoom key={conferenceId} conferenceId={decodeURIComponent(conferenceId)} initialInvitationToken={token} />;
}

import { useEffect, useState } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { authService } from "@/services/auth.service";
import { googleErrorMessage, parseGoogleCallback, postSignInRoute } from "@/lib/account/account-core";

interface GoogleCallbackResult {
  error: string;
  isLoading: boolean;
}

/** An exchange code is single-use: never redeem the same one twice (React strict mode). */
const redeemed = new Map<string, ReturnType<typeof authService.exchangeGoogleCode>>();

/**
 * Google sign-in callback (ACC-004/005). Preferred: `?exchange_code=` redeemed once with
 * `POST /api/v1/auth/oauth/google/exchange`. Transitional: the legacy token query the API
 * sends while `GOOGLE_OAUTH_TOKEN_DELIVERY=legacy_query`. Either way the query is removed
 * from the address bar and history before anything else happens, and the tokens go to the
 * httpOnly session model (ACC-019), never to script-readable storage.
 */
export function useGoogleCallback(): GoogleCallbackResult {
  const searchParams = useSearchParams();
  const router = useRouter();
  const [error, setError] = useState("");

  useEffect(() => {
    const callback = parseGoogleCallback(searchParams);
    if (typeof window !== "undefined" && window.location.search) {
      window.history.replaceState(null, "", window.location.pathname);
    }
    let cancelled = false;
    const fail = (message: string) => {
      if (!cancelled) setError(message);
    };
    switch (callback.kind) {
      case "error":
        fail(googleErrorMessage(callback.reason));
        break;
      case "invalid":
        fail("Invalid callback parameters. Please try again.");
        break;
      case "exchange": {
        let pending = redeemed.get(callback.exchangeCode);
        if (!pending) {
          pending = authService.exchangeGoogleCode(callback.exchangeCode);
          redeemed.set(callback.exchangeCode, pending);
        }
        pending
          .then((response) => {
            if (!cancelled) router.replace(postSignInRoute(response.user.status));
          })
          .catch(() => fail("The Google sign-in link expired or was already used. Please sign in again."));
        break;
      }
      case "legacy":
        authService
          .adoptLegacyGoogleTokens({
            access_token: callback.accessToken,
            refresh_token: callback.refreshToken,
            user: callback.user,
          })
          .then(() => {
            if (!cancelled) router.replace(postSignInRoute(callback.user.status));
          })
          .catch(() => fail("Google sign-in could not be completed. Please try again."));
        break;
    }
    return () => {
      cancelled = true;
    };
    // The query is read once on arrival; it is erased from the URL above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { error, isLoading: !error };
}

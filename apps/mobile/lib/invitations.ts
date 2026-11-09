/**
 * Invitation token parsing. Pure; runs under `node --test`.
 *
 * Organization invitation emails link to `{APP_URL}/organizations/invitations#<token>`; the
 * token (base64url, 43 characters for 256 bits) is the fragment. Users can paste the link or
 * the bare token; the app deep link `cfmeet://organizations/invitations?token=<token>` is
 * accepted too. The token is sent only in the body of
 * `POST /api/v2/organization-invitations/accept|decline`, never logged or put in a URL.
 */

const TOKEN = /^[A-Za-z0-9_-]{16,256}$/;

export function parseInvitationToken(input: string | null | undefined): string | null {
  let value = (input ?? "").trim();
  if (!value) return null;
  const hash = value.indexOf("#");
  if (hash >= 0) {
    value = value.slice(hash + 1);
  } else {
    const query = /[?&]token=([^&#]+)/.exec(value);
    if (query) {
      try {
        value = decodeURIComponent(query[1]);
      } catch {
        return null;
      }
    }
  }
  value = value.trim();
  return TOKEN.test(value) ? value : null;
}

/** Meeting code or link → identifier for `GET /api/v2/resolve/{identifier}`. */
export function parseMeetingIdentifier(input: string | null | undefined): string | null {
  let value = (input ?? "").trim();
  if (!value) return null;
  // Links like https://app.cfmeet.com/meeting/abc-defg-hij or …/join/abc-defg-hij
  const link = /^[a-z][a-z0-9+.-]*:\/\/[^/]+\/(?:.*\/)?([^/?#]+)\/?(?:[?#].*)?$/i.exec(value);
  if (link) value = link[1];
  try {
    value = decodeURIComponent(value);
  } catch {
    return null;
  }
  value = value.trim();
  return /^[A-Za-z0-9_-]{3,128}$/.test(value) ? value : null;
}

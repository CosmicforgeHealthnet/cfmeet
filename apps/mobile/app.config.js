/**
 * Dynamic Expo config (FE-006). Static settings stay in app.json; this file only derives
 * the environment-specific values so no build ever ships a hard-coded LAN host or
 * cleartext traffic by accident.
 *
 * Inputs (all optional):
 * - EXPO_PUBLIC_API_URL  v1 API base, e.g. https://staging-api.cfmeet.com/api/v1.
 *                        Default: https://api.cfmeet.com/api/v1.
 * - APP_ENV              development | preview | production. Falls back to
 *                        EAS_BUILD_PROFILE, then to "development" for local `expo start`.
 *
 * Release profiles (preview, production) refuse a cleartext http:// API URL to a
 * non-loopback host: the config fails to load, so the build fails instead of shipping it.
 * Android cleartext traffic is enabled only for the development profile with an http:// URL.
 */

const DEFAULT_API_URL = "https://api.cfmeet.com/api/v1";
const LOOPBACK = new Set(["localhost", "127.0.0.1", "::1", "[::1]", "10.0.2.2"]);

function profileName() {
  return (process.env.APP_ENV || process.env.EAS_BUILD_PROFILE || "development").toLowerCase();
}

module.exports = ({ config }) => {
  const profile = profileName();
  const isRelease = profile !== "development";
  const apiUrl = (process.env.EXPO_PUBLIC_API_URL || DEFAULT_API_URL).trim().replace(/\/+$/, "");

  let parsed;
  try {
    parsed = new URL(apiUrl);
  } catch {
    throw new Error(`EXPO_PUBLIC_API_URL is not a valid absolute URL (profile ${profile})`);
  }
  const cleartext = parsed.protocol === "http:";
  if (cleartext && isRelease && !LOOPBACK.has(parsed.hostname)) {
    throw new Error(
      `Refusing cleartext API URL ${parsed.protocol}//${parsed.host} for the ${profile} profile: release builds require https://`
    );
  }

  return {
    ...config,
    android: {
      ...config.android,
      usesCleartextTraffic: cleartext && !isRelease,
    },
    extra: {
      ...config.extra,
      apiUrl,
      appEnv: profile,
    },
  };
};

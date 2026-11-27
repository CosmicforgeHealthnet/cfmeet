// S-10 / Notification Service roadmap N0-11: the iOS app declares the `voip` background mode
// only once a PushKit / CallKit integration exists (an unused VoIP capability is an App Store
// review risk). Audio (meetings in the background) stays.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const appRoot = resolve(here, "..");
const require = createRequire(import.meta.url);

const appJson = JSON.parse(readFileSync(resolve(appRoot, "app.json"), "utf8"));
const pkg = JSON.parse(readFileSync(resolve(appRoot, "package.json"), "utf8"));

/** Packages that provide CallKit / PushKit (VoIP push) on iOS. */
const CALLKIT_MODULES = [
  "react-native-callkeep",
  "react-native-voip-push-notification",
  "expo-callkit",
  "@config-plugins/react-native-callkeep",
];
const dependencies = { ...pkg.dependencies, ...pkg.devDependencies };
const hasCallKit = CALLKIT_MODULES.some((name) => name in dependencies);

/** The config Expo builds: app.json through app.config.js (release profile, default API). */
function resolvedExpoConfig() {
  const dynamic = require(resolve(appRoot, "app.config.js"));
  const saved = { APP_ENV: process.env.APP_ENV, EXPO_PUBLIC_API_URL: process.env.EXPO_PUBLIC_API_URL };
  process.env.APP_ENV = "production";
  delete process.env.EXPO_PUBLIC_API_URL;
  try {
    return dynamic({ config: structuredClone(appJson.expo) });
  } finally {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  }
}

test("no `voip` background mode while no CallKit / PushKit module is installed", () => {
  if (hasCallKit) return; // a real integration may declare it
  for (const [label, expo] of [["app.json", appJson.expo], ["app.config.js", resolvedExpoConfig()]] as const) {
    const modes: unknown = expo.ios?.infoPlist?.UIBackgroundModes ?? [];
    assert.ok(Array.isArray(modes), `${label}: UIBackgroundModes is a list`);
    assert.ok(!modes.includes("voip"), `${label} must not declare the voip background mode`);
  }
});

test("background audio for meetings stays declared", () => {
  assert.deepEqual(appJson.expo.ios.infoPlist.UIBackgroundModes, ["audio"]);
  assert.deepEqual(resolvedExpoConfig().ios.infoPlist.UIBackgroundModes, ["audio"]);
});

test("no config plugin option that requires the voip background mode is enabled", () => {
  if (hasCallKit) return;
  // @livekit/react-native-expo-plugin: enableMultitaskingCameraAccess requires `voip`.
  for (const plugin of appJson.expo.plugins as unknown[]) {
    if (Array.isArray(plugin) && plugin[0] === "@livekit/react-native-expo-plugin") {
      assert.notEqual((plugin[1] as { ios?: { enableMultitaskingCameraAccess?: boolean } })?.ios?.enableMultitaskingCameraAccess, true);
    }
  }
});

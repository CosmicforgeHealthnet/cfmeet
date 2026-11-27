import { getLogger, LoggerNames } from "livekit-client";

/**
 * LiveKit logs these as errors whenever the server closes a media connection on purpose
 * (hang up, the other person leaving, hold, a breakout move): the data channels are torn down
 * from the server side, which the SDK reports as an "unknown" channel error. They aren't
 * failures, and in development Next.js shows every console error as an overlay. Drop exactly
 * these messages; every other LiveKit log still goes through.
 */
const EXPECTED_TEARDOWN = [
  /^Unknown DataChannel error on (lossy|reliable)/,
  /^DataChannel error on (lossy|reliable)/,
  /^Tried to add a track for a participant, that's not present/,
];

let installed = false;

export function quietExpectedLiveKitTeardown() {
  if (installed || typeof window === "undefined") return;
  installed = true;
  for (const name of [LoggerNames.Engine, LoggerNames.Room, LoggerNames.PCTransport, LoggerNames.PCManager]) {
    const logger = getLogger(name);
    const original = logger.methodFactory;
    logger.methodFactory = (methodName, level, loggerName) => {
      const raw = original(methodName, level, loggerName);
      return (...args: unknown[]) => {
        if (typeof args[0] === "string" && EXPECTED_TEARDOWN.some((re) => re.test(args[0] as string))) return;
        raw(...args);
      };
    };
    logger.setLevel(logger.getLevel());
  }
}

// Resolve extensionless relative imports inside src/ to their `.ts` file, the way the Next
// bundler does, so tests can import modules that import their siblings ("./fonts").
//   import { register } from "node:module";
//   register("./helpers/ts-resolve.mjs", import.meta.url);
//   const lib = await import("../src/lib/media-editor/overlays.ts");
export async function resolve(specifier, context, next) {
  if (/^\.\.?\//.test(specifier) && !/\.[cm]?[jt]sx?$/.test(specifier) && context.parentURL?.endsWith(".ts")) {
    try {
      return await next(`${specifier}.ts`, context);
    } catch {
      // fall through to the default resolution
    }
  }
  return next(specifier, context);
}

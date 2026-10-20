/**
 * CF Meet production build asset integrity, independent of Next.js SRI support.
 * Run after next build and before promoting the SAME artifact.
 * This is NOT browser-enforced Subresource Integrity or E2EE certification.
 *
 * Covers the compiled Next static assets AND the executable files the app serves from
 * public/: the Messaging crypto worker, glue and WebAssembly module (whose provenance is
 * scripts/messaging/build-web-crypto.sh --check) and the Trusted Types default policy.
 * CFMEET_PUBLIC_DIR overrides public/ (tests).
 *
 * node scripts/verify-build-assets.mjs --write [build-dir]
 * node scripts/verify-build-assets.mjs --verify [build-dir]
 */
import { createHash } from "node:crypto";
import { lstat, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { resolve, relative, join, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("../", import.meta.url)));
const mode = process.argv[2];
if (!["--write", "--verify"].includes(mode) || process.argv.length > 4)
  throw new Error("Usage: --write|--verify [build-dir]");
const buildDir = resolve(process.argv[3] ?? join(root, ".next"));
const staticDir = join(buildDir, "static");
const manifestPath = join(buildDir, "cfmeet-build-assets.sha384.json");
const assetExtension = /\.(js|css|wasm)$/i;
const publicDir = resolve(process.env.CFMEET_PUBLIC_DIR ?? join(root, "public"));
const PUBLIC_EXECUTABLE = ["messaging-crypto", "security"];

async function inventory(staticDir, prefix = "") {
  const result = {};
  async function walk(dir) {
    const names = await readdir(dir);
    names.sort();
    for (const name of names) {
      const absolute = join(dir, name);
      const stat = await lstat(absolute);
      if (stat.isSymbolicLink()) throw new Error("Symlink in production static assets");
      if (stat.isDirectory()) await walk(absolute);
      else if (stat.isFile() && assetExtension.test(name)) {
        const path = prefix + relative(staticDir, absolute).split(sep).join("/");
        if (!path || path.startsWith("../") || Object.hasOwn(result, path))
          throw new Error("Unsafe or duplicate build asset");
        const body = await readFile(absolute);
        result[path] = {
          bytes: body.length,
          sha384: createHash("sha384").update(body).digest("base64"),
        };
      }
    }
  }
  await walk(staticDir);
  return result;
}

const assets = await inventory(staticDir);
if (!Object.keys(assets).some((p) => p.endsWith(".js")))
  throw new Error("No JavaScript build assets; refusing empty inventory");
const served = {};
for (const d of PUBLIC_EXECUTABLE) {
  const dir = join(publicDir, d);
  const exists = await lstat(dir).then((s) => s.isDirectory()).catch(() => false);
  if (exists) Object.assign(served, await inventory(dir, `public/${d}/`));
}
if (mode === "--write") {
  await mkdir(buildDir, { recursive: true });
  await writeFile(manifestPath, JSON.stringify({
    schema: "cfmeet.next.static.sha384.v2",
    note: "Artifact integrity only; not browser SRI.",
    assets,
    public: served,
  }, null, 2) + "\n", { flag: "wx" });
  console.log("Recorded SHA-384 for " + Object.keys(assets).length + " static assets and " + Object.keys(served).length + " served public executables");
} else {
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  if (manifest.schema !== "cfmeet.next.static.sha384.v2" ||
      !manifest.assets || typeof manifest.assets !== "object" ||
      JSON.stringify(manifest.assets) !== JSON.stringify(assets) ||
      JSON.stringify(manifest.public ?? null) !== JSON.stringify(served))
    throw new Error("Build integrity FAILED: missing, modified or added asset");
  console.log("Verified SHA-384 for " + Object.keys(assets).length + " static assets and " + Object.keys(served).length + " served public executables");
}

#!/usr/bin/env node
// Asserts the built SDK (dist/) is safe to ship to any runtime, browsers included:
//   * no literal credential or signing secret (cfmk_<env>_<id>..., whsec_<secret>);
//   * no process.env access (credentials come from the application, never the SDK);
//   * no Node built-in imports (the runtime code uses only web-standard globals).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const dir = process.argv[2] ?? 'dist';
const rules = [
  [/cfmk_(?:sbx|test|live)_[A-Za-z0-9]{6,}/, 'a literal platform credential'],
  [/cfmk_live/, 'a live credential prefix'],
  [/whsec_[A-Za-z0-9_-]{8,}/, 'a literal webhook signing secret'],
  [/process\s*\.\s*env|process\s*\[\s*['"]env['"]\s*\]/, 'a process.env read'],
  [/from\s+['"](?:node:)?(?:fs|path|os|http|https|net|tls|child_process|crypto|process)['"]|require\s*\(/, 'a Node built-in import / require'],
];

function* files(d) {
  for (const name of readdirSync(d)) {
    const p = join(d, name);
    if (statSync(p).isDirectory()) yield* files(p);
    else if (/\.(js|mjs|cjs|d\.ts)$/.test(name)) yield p;
  }
}

let problems = 0;
let count = 0;
for (const f of files(dir)) {
  count++;
  const text = readFileSync(f, 'utf8');
  for (const [re, what] of rules) {
    const m = re.exec(text);
    if (m) {
      problems++;
      console.error(`${f}: ${what}: ${JSON.stringify(m[0].slice(0, 24))}`);
    }
  }
}
if (count === 0) {
  console.error(`no built files under ${dir}: run npm run build first`);
  process.exit(1);
}
if (problems) {
  console.error(`check-bundle: FAILED — ${problems} problem(s)`);
  process.exit(1);
}
console.log(`check-bundle: OK — ${count} file(s): no literal credentials or secrets, no process.env, no Node built-ins`);

// Checks the SDK's type-level guarantees.
//
//   test/positive.ts        must type-check
//   test/negative/*.ts      must not
//
// Without the second, a type loose enough to accept everything would pass.
// ADR-0004 claims an unknown ID fails to build; this is where that claim
// is actually checked.

import { execFileSync } from "node:child_process";
import { readdirSync, writeFileSync, rmSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const sdk = join(here, "..");
const TSC = join(sdk, "node_modules", "typescript", "lib", "tsc.js");

let failed = 0;
const ok = (m) => console.log(`  \x1b[32mOK\x1b[0m   ${m}`);
const ng = (m, d) => {
  failed++;
  console.log(`  \x1b[31mNG\x1b[0m   ${m}`);
  if (d) console.log(`       ${d.split("\n").slice(0, 3).join("\n       ")}`);
};

/** Type-checks one file, returning the errors (empty means it passed). */
function typecheck(file) {
  const cfg = join(here, ".tsconfig.tmp.json");
  writeFileSync(
    cfg,
    JSON.stringify({
      extends: "../tsconfig.json",
      compilerOptions: {
        noEmit: true,
        // The base config emits only from src/, so a test file outside rootDir
        // fails with TS6059. Undone here.
        rootDir: "..",
        declaration: false,
      },
      // The base exclude drops test/**, so that is undone too. Forgetting it
      // fails with TS18003 (no input files) and lets the negative cases pass
      // for the wrong reason.
      exclude: [],
      include: [file.replace(/\\/g, "/")],
    }),
  );
  try {
    // Neither npx nor tsc.cmd.
    // Spawning a .cmd on Windows needs a shell, which stops arguments being
    // escaped; and a failed spawn produces no output, which reads as a clean
    // type-check. Running tsc.js under node directly avoids both.
    execFileSync(process.execPath, [TSC, "-p", cfg], { cwd: sdk, stdio: "pipe" });
    return "";
  } catch (e) {
    const out = String(e.stdout ?? "") + String(e.stderr ?? "");
    if (!out.trim()) {
      // tsc did not start, so nothing was verified and this must not be
      // quietly treated as a failure.
      console.error(`\n\x1b[31mcannot run tsc\x1b[0m: ${e.message}`);
      process.exit(1);
    }
    return out;
  } finally {
    rmSync(cfg, { force: true });
  }
}

console.log("must type-check");
{
  const err = typecheck(join(here, "positive.ts"));
  if (err) ng("positive.ts", err);
  else ok("positive.ts");
}

console.log("\nmust not type-check");
// The kind of error matters: a misconfiguration failing is not a type
// error. TS18003 (no input files) and TS5xxx (configuration) prove
// nothing about the types.
const CONFIG_ERRORS = /error TS(18003|5\d{3}|6\d{3})/;

for (const f of readdirSync(join(here, "negative")).filter((f) => f.endsWith(".ts"))) {
  const err = typecheck(join(here, "negative", f));
  if (!err) {
    ng(`${f} — it passed`);
    continue;
  }
  if (CONFIG_ERRORS.test(err)) {
    ng(`${f} — failed on configuration, so nothing was verified`, err);
    continue;
  }
  const first = err.split("\n").find((l) => /error TS\d+/.test(l)) ?? "";
  ok(`${f.padEnd(24)} ${first.replace(/^.*error /, "").slice(0, 64)}`);
}

console.log();
if (failed > 0) {
  console.log(`\x1b[31m${failed} failed\x1b[0m`);
  process.exit(1);
}
console.log("\x1b[32mall as expected\x1b[0m");

console.log("\nruntime resolves ctx.data per node");
{
  // Run through node directly: .cmd shims do not spawn cleanly everywhere.
  const ESBUILD = join(sdk, "node_modules", "esbuild", "bin", "esbuild");
  const bundle = join(here, ".runtime.tmp.mjs");
  const script = join(here, ".runtime-test.tmp.mjs");
  try {
    execFileSync(
      process.execPath,
      [ESBUILD, "src/runtime.ts", "--bundle", "--format=esm", `--outfile=${bundle}`],
      { cwd: sdk, stdio: "pipe" },
    );
    writeFileSync(
      script,
      `import { registerPatch } from ${JSON.stringify("./.runtime.tmp.mjs")};
const seen = {};
registerPatch("chat.message", (node, ctx) => { seen.message = ctx.data; return node; });
registerPatch("chat.message.content", (node, ctx) => { seen.content = ctx.data; return node; });
registerPatch("primitive.text", (node, ctx) => { seen.text = ctx.data; return node; });
const tree = { id: "chat.message", key: "1", children: [
  { id: "chat.message.content", children: [{ id: "primitive.text" }] },
] };
const ctx = { data: {
  "chat.message\\n1": { id: "1" },
  "chat.message.content\\n": { id: "1" },
} };
const apply = globalThis["__gumicord_apply"];
const assert = (c, m) => { if (!c) { console.error("NG " + m); process.exit(1); } };
const out = apply(tree, ctx);
assert(seen.message && seen.message.id === "1", "message data");
assert(seen.content && seen.content.id === "1", "content data");
assert(seen.text === undefined, "unmapped reads undefined");
assert(out.children[0].children[0].id === "primitive.text", "tree intact");
// An empty ctx never crashes the lookup.
apply(tree, {});
apply(tree);
console.log("per-node data resolves");
`,
    );
    const out = execFileSync(process.execPath, [script], { cwd: here, encoding: "utf8" });
    if (!out.includes("per-node data resolves")) throw new Error("assertions silent");
    ok("ctx.data resolves per node");
  } catch (e) {
    ng("ctx.data resolves per node", e.message ?? String(e));
  } finally {
    rmSync(bundle, { force: true });
    rmSync(script, { force: true });
  }
}

console.log();
if (failed > 0) {
  console.log(`\x1b[31m${failed} failed\x1b[0m`);
  process.exit(1);
}
console.log("\x1b[32mall runtime checks passed\x1b[0m");

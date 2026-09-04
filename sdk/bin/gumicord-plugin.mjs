#!/usr/bin/env node
// Builds a plugin directory for loading: src/index.ts -> plugin.js.
//
//   gumicord-plugin build <dir>   one minified bundle
//   gumicord-plugin dev <dir>     rebuild on every source change
//
// `@gumicord/sdk` resolves to this package's source, so a plugin needs no
// setup beyond importing it. The bundle is self-contained classic script:
// QuickJS runs it with a plain eval, no module loader involved.
import { context, build } from "esbuild";
import { existsSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const sdkEntry = join(here, "..", "src", "index.ts");
const BANNER =
  "// Built by gumicord-plugin; do not edit. Change src/index.ts and rebuild.";

function options(dir, minify) {
  const entry = join(dir, "src", "index.ts");
  if (!existsSync(entry)) {
    console.error(`no plugin source: ${entry}`);
    process.exit(1);
  }
  return {
    entryPoints: [entry],
    bundle: true,
    format: "iife",
    minify,
    outfile: join(dir, "plugin.js"),
    alias: { "@gumicord/sdk": sdkEntry },
    banner: { js: BANNER },
    logLevel: "warning",
  };
}

const [command, target] = process.argv.slice(2);
if (!command || !target) {
  console.error("usage: gumicord-plugin <build|dev> <plugin-dir>");
  process.exit(1);
}
const dir = resolve(target);

if (command === "build") {
  await build(options(dir, true));
  console.log(`built ${join(dir, "plugin.js")}`);
} else if (command === "dev") {
  const ctx = await context(options(dir, false));
  await ctx.watch();
  console.log(`watching ${join(dir, "src")} — save to rebuild`);
} else {
  console.error(`unknown command: ${command}`);
  process.exit(1);
}

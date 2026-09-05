#!/usr/bin/env node
// Builds a plugin directory for loading: src/index.ts -> plugin.js, plus
// src/<name>.ts -> <name>.js when the manifest declares a settings page.
//
//   gumicord-plugin build <dir>   one minified bundle per entry
//   gumicord-plugin dev <dir>     rebuild on every source change
//
// `@gumicord/sdk` resolves to this package's source, so a plugin needs no
// setup beyond importing it. The bundle is self-contained classic script:
// QuickJS runs it with a plain eval, no module loader involved.
import { context, build } from "esbuild";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname, resolve, basename } from "node:path";
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

/// A second bundle for the settings page, when declared. The manifest names
/// the output (`settings.js`); the source is the same name under `src/`.
function settingsOptions(dir, minify) {
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"));
  } catch {
    return null;
  }
  const output = manifest?.settings;
  if (typeof output !== "string" || !output.endsWith(".js")) return null;
  const base = basename(output, ".js");
  if (base === "" || base === "." || base === ".." || output !== `${base}.js`) return null;
  const entry = join(dir, "src", `${base}.ts`);
  if (!existsSync(entry)) {
    console.error(`no settings source: ${entry}`);
    process.exit(1);
  }
  return {
    entryPoints: [entry],
    bundle: true,
    format: "iife",
    minify,
    outfile: join(dir, output),
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
  const settings = settingsOptions(dir, true);
  if (settings) {
    await build(settings);
    console.log(`built ${settings.outfile}`);
  }
} else if (command === "dev") {
  const ctx = await context(options(dir, false));
  await ctx.watch();
  const settings = settingsOptions(dir, false);
  if (settings) {
    const settingsCtx = await context(settings);
    await settingsCtx.watch();
  }
  console.log(`watching ${join(dir, "src")}  Esave to rebuild`);
} else {
  console.error(`unknown command: ${command}`);
  process.exit(1);
}

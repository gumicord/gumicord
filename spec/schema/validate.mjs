// Checks spec/schema/*.schema.json.
//
// Usage:
//   node spec/schema/validate.mjs
//
// CI runs this when spec/schema/** or examples/** changes. It checks not
// only that the schemas are well formed, but that the sample themes pass
// them and that deliberately broken input does not.

import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..", "..");

const ajv = new Ajv2020({ allErrors: true, strict: true });
addFormats(ajv);

let failed = 0;
const ok = (msg) => console.log(`  [32mOK[0m   ${msg}`);
const ng = (msg, detail) => {
  failed++;
  console.log(`  [31mNG[0m   ${msg}`);
  if (detail) console.log(`       ${detail}`);
};

// ---------------------------------------------------------------- The schemas
console.log("compiling the schemas");
const schemas = {};
for (const f of readdirSync(here).filter((f) => f.endsWith(".schema.json"))) {
  const src = JSON.parse(readFileSync(join(here, f), "utf8"));
  try {
    schemas[f] = ajv.compile(src);
    ok(f);
  } catch (e) {
    ng(f, e.message);
  }
}

const themeValidate = schemas["theme.schema.json"];
if (!themeValidate) {
  console.log("\ntheme.schema.json did not compile; stopping here");
  process.exit(1);
}

// ---------------------------------------------------------------- The samples
console.log("\nsample themes");
const themesDir = join(repo, "examples", "themes");
if (existsSync(themesDir)) {
  for (const name of readdirSync(themesDir)) {
    const p = join(themesDir, name, "theme.json");
    if (!existsSync(p)) continue;
    const data = JSON.parse(readFileSync(p, "utf8"));
    if (themeValidate(data)) {
      ok(`${name} (${data.rules?.length ?? 0} rules, ${Object.keys(data.tokens ?? {}).length} tokens)`);
    } else {
      ng(name, JSON.stringify(themeValidate.errors?.slice(0, 3), null, 2));
      continue;
    }

    // The schema sees only the format. Whether a bundled asset exists, and
    // whether an external URL is declared in manifest.remoteAssets, cannot be
    // expressed there, so it is checked here.
    const dir = join(themesDir, name);
    const declared = new Set(data.manifest?.remoteAssets ?? []);
    const refs = collectAssetRefs(data);
    for (const ref of refs) {
      if (ref.startsWith("data:")) continue;
      if (ref.startsWith("https://")) {
        const host = new URL(ref).hostname;
        if (declared.has(host)) ok(`  external ${host} — declared in remoteAssets`);
        else ng(`  external ${host} — not declared in manifest.remoteAssets`);
        continue;
      }
      if (existsSync(join(dir, ref))) ok(`  bundled ${ref}`);
      else ng(`  bundled ${ref} — no such file`);
    }

    // A host declared and never used is asking for more than it needs.
    const usedHosts = new Set(
      refs.filter((r) => r.startsWith("https://")).map((r) => new URL(r).hostname),
    );
    for (const h of declared) {
      if (!usedHosts.has(h)) ng(`  ${h} is declared in remoteAssets but never used`);
    }
  }
}

/** Collects every asset reference in a theme. */
function collectAssetRefs(theme) {
  const out = [];
  const visit = (v) => {
    if (!v || typeof v !== "object") return;
    if (Array.isArray(v)) return v.forEach(visit);
    for (const [k, val] of Object.entries(v)) {
      if ((k === "image" || k === "family") && typeof val === "string") {
        // family is a font name or an asset reference; an extension means an asset.
        if (k === "family" && !/\.(woff2|ttf|otf)$/i.test(val)) continue;
        out.push(val);
      } else {
        visit(val);
      }
    }
  };
  visit(theme.tokens);
  visit(theme.rules);
  return out;
}

// ---------------------------------------------------------------- Plugin manifests
console.log("\nsample plugin manifests");
const manifestValidate = schemas["plugin-manifest.schema.json"];
if (manifestValidate) {
  const pluginsDir = join(repo, "examples", "plugins");
  if (existsSync(pluginsDir)) {
    for (const name of readdirSync(pluginsDir)) {
      const p = join(pluginsDir, name, "manifest.json");
      if (!existsSync(p)) continue;
      const data = JSON.parse(readFileSync(p, "utf8"));
      if (!manifestValidate(data)) {
        ng(name, JSON.stringify(manifestValidate.errors?.slice(0, 3), null, 2));
        continue;
      }
      if (data.id !== name) {
        ng(`${name} — id does not match the directory`);
        continue;
      }
      ok(`${name} (${(data.capabilities ?? []).join(", ") || "no capabilities"})`);
    }
  }

  console.log("\nmanifests that must be rejected");
  const manifestBase = { id: "com.example.hello", name: "Hello", version: "1.0.0" };
  for (const [label, data] of [
    ["no id", { name: "Hello", version: "1.0.0" }],
    ["id is not a reverse domain", { ...manifestBase, id: "hello" }],
    ["version is not semver", { ...manifestBase, version: "1.0" }],
    ["entry escapes the directory", { ...manifestBase, entry: "../evil.js" }],
    ["entry in a subdirectory", { ...manifestBase, entry: "sub/plugin.js" }],
    ["unknown capability", { ...manifestBase, capabilities: ["network"] }],
    ["unknown top-level key", { ...manifestBase, main: "plugin.js" }],
  ]) {
    if (manifestValidate(data)) ng(`${label} — it passed`);
    else ok(label);
  }

  console.log("\nmanifests that must be accepted");
  for (const [label, data] of [
    ["the minimum", manifestBase],
    ["an entry and capabilities", { ...manifestBase, entry: "plugin.qjsc", capabilities: ["log", "storage"] }],
  ]) {
    if (manifestValidate(data)) ok(label);
    else ng(label, JSON.stringify(manifestValidate.errors?.slice(0, 2)));
  }
}

// ---------------------------------------------------------------- Must fail
// Checks the schema rejects what it should. Without this, a schema loose
// enough to accept everything would pass.
console.log("\nmust be rejected");

const base = {
  manifest: { id: "dev.gumicord.t", name: "T", version: "1.0.0", abi: 1 },
};

const shouldFail = [
  ["no manifest", { tokens: {} }],
  ["no abi", { manifest: { id: "dev.gumicord.t", name: "T", version: "1.0.0" } }],
  ["id is not a reverse domain", { manifest: { ...base.manifest, id: "midnight" } }],
  ["version is not semver", { manifest: { ...base.manifest, version: "1.0" } }],
  ["malformed colour", { ...base, tokens: { "color.a": "rgb(1,2,3)" } }],
  ["negative length", { ...base, tokens: { "radius.a": -4 } }],
  ["uppercase in a token name", { ...base, tokens: { "Color.Bg": "#fff" } }],
  ["wildcard in select", { ...base, rules: [{ select: "chat.*", style: {} }] }],
  ["five-level select", { ...base, rules: [{ select: "a.b.c.d.e", style: {} }] }],
  ["unknown state", { ...base, rules: [{ select: "chat.message", when: { state: "pressed" }, style: {} }] }],
  ["unknown platform", { ...base, rules: [{ select: "chat.message", when: { platform: "web" }, style: {} }] }],
  ["unknown when key", { ...base, rules: [{ select: "chat.message", when: { theme: "dark" }, style: {} }] }],
  ["unknown style property", { ...base, rules: [{ select: "chat.message", style: { boxShadow: 1 } }] }],
  ["opacity out of range", { ...base, rules: [{ select: "chat.message", style: { opacity: 2 } }] }],
  ["wrong number of padding values", { ...base, rules: [{ select: "chat.message", style: { padding: [1, 2] } }] }],
  ["malformed token reference", { ...base, rules: [{ select: "chat.message", style: { color: "$Color.Bg" } }] }],
  ["rule without a style", { ...base, rules: [{ select: "chat.message" }] }],
  ["unknown top-level key", { ...base, extra: 1 }],

  // --- Background images ---
  ["unknown key in a background", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", repeat: "x" } } }] }],
  ["unknown fit", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", fit: "fill" } } }] }],
  ["wrong number of position values", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", position: [0.5] } } }] }],
  ["position out of range", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", position: [1.5, 0] } } }] }],
  ["asset reference leaves the directory", { ...base, rules: [{ select: "app.window", style: { background: { image: "../secret.png" } } }] }],
  ["asset reference climbs part way", { ...base, rules: [{ select: "app.window", style: { background: { image: "assets/../../x.png" } } }] }],
  ["absolute asset reference", { ...base, rules: [{ select: "app.window", style: { background: { image: "/etc/passwd.png" } } }] }],
  ["absolute Windows asset reference", { ...base, rules: [{ select: "app.window", style: { background: { image: "C:/x/a.png" } } }] }],
  ["external URL over http", { ...base, rules: [{ select: "app.window", style: { background: { image: "http://e.com/a.png" } } }] }],
  ["external URL with a file scheme", { ...base, rules: [{ select: "app.window", style: { background: { image: "file:///etc/passwd" } } }] }],
  ["unsupported image format", { ...base, rules: [{ select: "app.window", style: { background: { image: "assets/a.bmp" } } }] }],
  ["negative blur", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", blur: -1 } } }] }],
  ["remoteAssets is not a host name", { manifest: { ...base.manifest, remoteAssets: ["https://cdn.example.com/"] } }],
];

for (const [label, data] of shouldFail) {
  if (themeValidate(data)) {
    ng(`${label} — it passed`);
  } else {
    ok(label);
  }
}

// ---------------------------------------------------------------- Must pass
console.log("\nmust be accepted");
const shouldPass = [
  ["the minimum: a manifest alone", base],
  ["an array of states", { ...base, rules: [{ select: "chat.message", when: { state: ["hover", "unread"] }, style: {} }] }],
  ["an array of platforms", { ...base, rules: [{ select: "chat.message", when: { platform: ["ios", "android"] }, style: {} }] }],
  ["a token referencing a token", { ...base, tokens: { "color.a": "#fff", "color.b": "$color.a" } }],
  ["an eight-digit colour", { ...base, tokens: { "color.a": "#ffffff14" } }],
  ["four padding values", { ...base, rules: [{ select: "chat.message", style: { padding: [1, 2, 3, 4] } }] }],
  ["a font without a family", { ...base, tokens: { "font.a": { size: 15, lineHeight: 22 } } }],

  // --- Background images ---
  ["a background as the colour shorthand", { ...base, rules: [{ select: "app.window", style: { background: "#0f0f17" } }] }],
  ["a background as a token reference", { ...base, rules: [{ select: "app.window", style: { background: "$color.bg" } }] }],
  [
    "a full background object",
    {
      ...base,
      rules: [
        {
          select: "app.window",
          style: {
            background: {
              color: "#0f0f17",
              image: "assets/wallpaper.png",
              fit: "cover",
              position: [0.5, 0.35],
              opacity: 0.9,
              blur: 4,
              tint: "#0f0f1766",
            },
          },
        },
      ],
    },
  ],
  ["a background defined as a token", { ...base, tokens: { "bg.main": { image: "assets/a.webp", fit: "tile" } } }],
  ["a nested bundled asset", { ...base, rules: [{ select: "app.window", style: { background: { image: "assets/img/bg.avif" } } }] }],
  ["an image as a data URI", { ...base, rules: [{ select: "app.window", style: { background: { image: "data:image/png;base64,iVBORw0KGgo=" } } }] }],
  [
    "an external URL with remoteAssets",
    {
      manifest: { ...base.manifest, remoteAssets: ["cdn.example.com", "i.imgur.com"] },
      rules: [{ select: "app.window", style: { background: { image: "https://cdn.example.com/bg.png" } } }],
    },
  ],
];

for (const [label, data] of shouldPass) {
  if (themeValidate(data)) {
    ok(label);
  } else {
    ng(label, JSON.stringify(themeValidate.errors?.slice(0, 2)));
  }
}

console.log();
if (failed > 0) {
  console.log(`[31m${failed} failed[0m`);
  process.exit(1);
}
console.log("[32mall as expected[0m");

// Generates the API reference signature blocks from the SDK source, and
// the stable ID catalogs from the spec table.
//
//   node sdk/apidocs/generate.mjs --out <api-docs-dir> [--check]
//
// `sdk/src/*.ts` TSDoc is the English source; `sdk/apidocs/ja.json` holds
// the Japanese API descriptions. The catalog's Japanese roles come from
// `spec/03-uitree.md` (itself generated); `sdk/apidocs/ids-en.json` holds
// the English roles. Only the marked sections are replaced; the
// hand-written guide pages around them are left alone:
//
//   <!-- BEGIN GENERATED: api --> ... <!-- END GENERATED: api -->
//   <!-- BEGIN GENERATED: ids --> ... <!-- END GENERATED: ids -->
//
// Run `cargo xtask api-docs` instead of calling this directly.

import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const sdk = join(here, "..");
const require = createRequire(import.meta.url);
// Bundled through the installed package's JS API, never through its bin
// path (same reason as in sdk/test/run.mjs).
const ts = require("typescript");

const BEGIN = "<!-- BEGIN GENERATED: api -->";
const END = "<!-- END GENERATED: api -->";

const IDS_BEGIN = "<!-- BEGIN GENERATED: ids -->";
const IDS_END = "<!-- END GENERATED: ids -->";

// Gumicord repo root (this script lives at sdk/apidocs/).
const root = join(here, "..", "..");

const GROUPS = [
  { ns: "ui", file: "index.ts" },
  { ns: "log", file: "index.ts" },
  { ns: "storage", file: "index.ts" },
];

const INTERFACES = [
  { name: "UINode", file: "uitree.ts" },
  { name: "NewUINode", file: "uitree.ts" },
  { name: "PatchContext", file: "uitree.ts" },
  { name: "PatchFn", file: "uitree.ts" },
  { name: "UserData", file: "data.ts", fields: true },
  { name: "MessageData", file: "data.ts", fields: true },
  { name: "GuildData", file: "data.ts", fields: true },
  { name: "ChannelData", file: "data.ts", fields: true },
  { name: "CategoryData", file: "data.ts", fields: true },
  { name: "DmData", file: "data.ts", fields: true },
  { name: "MemberData", file: "data.ts", fields: true },
  { name: "AttachmentData", file: "data.ts", fields: true },
  { name: "EmbedData", file: "data.ts", fields: true },
];

const ALIASES = [
  { name: "NodeId", file: "ids.ts", catalog: true, abbrev: true },
  { name: "PluginNodeId", file: "uitree.ts" },
  { name: "CreatableNodeId", file: "uitree.ts" },
  { name: "CoreCreatableNodeId", file: "ids.ts", abbrev: true },
  { name: "NodeState", file: "uitree.ts" },
  { name: "DataByNode", file: "ids.ts" },
];

const HEADINGS = {
  en: {
    interfaces: "## Interfaces",
    aliases: "## Type aliases and enum-like types",
    fields: (list) => `Fields: ${list}`,
  },
  ja: {
    interfaces: "## インターフェース",
    aliases: "## 型エイリアスと列挙的型",
    fields: (list) => `欄: ${list}`,
  },
};

const CATALOG_LINE = {
  en: "For the full list see the [stable ID catalog](en/theme/ids.md).",
  ja: "一覧は[安定 ID カタログ](ja/theme/ids.md)を見ること。",
};

/** Long unions render abbreviated: first two, count, last one. */
function abbreviateUnion(text) {
  const members = [...text.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  if (members.length <= 5) return text;
  const rest = members.length - 3;
  return (
    `type ${unionNameOf(text)} =\n` +
    `  | "${members[0]}"\n` +
    `  | "${members[1]}"\n` +
    `  | /* ... ${rest} more */\n` +
    `  | "${members[members.length - 1]}"\n` +
    `  ;`
  );
}

function unionNameOf(text) {
  const m = text.match(/type\s+(\w+)/);
  return m ? m[1] : "T";
}

function loadSource(file) {
  const path = join(sdk, "src", file);
  // Normalize first: extracted snippets are spliced into LF files, and a
  // stray CR would read as a diff on every run.
  const text = readFileSync(path, "utf8").replace(/\r\n/g, "\n");
  return {
    sf: ts.createSourceFile(path, text, ts.ScriptTarget.ESNext, true),
    path,
  };
}

/** Raw TSDoc comment plus @example bodies for a node. */
function jsdoc(node) {
  const docs = node.jsDoc ?? [];
  const parts = [];
  const examples = [];
  for (const d of docs) {
    const comment = jsDocText(d.comment);
    if (comment) parts.push(comment);
    for (const tag of d.tags ?? []) {
      if (tag.tagName.text === "example") {
        const body = jsDocText(tag.comment).trim();
        if (body) examples.push(body);
      }
    }
  }
  return { comment: parts.join("\n\n"), examples };
}

function jsDocText(comment) {
  if (!comment) return "";
  if (typeof comment === "string") return comment;
  return comment
    .map((c) => {
      if (typeof c === "string") return c;
      // JSDocText.text is already stripped of `*` decoration; getText()
      // would return the decorated source instead.
      if (typeof c.text === "string" && c.text) return c.text;
      if (c.kind === ts.SyntaxKind.JSDocLink && c.name) {
        return `\`${c.name.getText()}\``;
      }
      return "";
    })
    .join("");
}

/** `{@link x}` reads literally in Markdown; render it as code instead. */
function cleanMd(text) {
  return text.replace(/\{@link\s+([^}]+)\}/g, "`$1`").trim();
}

function typeParamsOf(node, sf) {
  const tp = node.typeParameters?.map((t) => t.getText(sf)).join(", ");
  return tp ? `<${tp}>` : "";
}

function paramsOf(node, sf) {
  return node.parameters.map((p) => p.getText(sf)).join(", ");
}

/** `name<...>(...): ret` for methods and arrow-function properties. */
function signatureOf(member, sf) {
  const name = member.name.getText(sf);
  if (ts.isMethodDeclaration(member)) {
    const ret = member.type?.getText(sf) ?? "void";
    return `${name}${typeParamsOf(member, sf)}(${paramsOf(member, sf)}): ${ret}`;
  }
  if (
    ts.isPropertyAssignment(member) &&
    ts.isArrowFunction(member.initializer)
  ) {
    const fn = member.initializer;
    const ret = fn.type?.getText(sf) ?? "void";
    return `${name}${typeParamsOf(fn, sf)}(${paramsOf(fn, sf)}): ${ret}`;
  }
  return null;
}

function constMembers(sf, constName) {
  const out = [];
  const visit = (node) => {
    if (
      ts.isVariableStatement(node) &&
      node.modifiers?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword)
    ) {
      for (const decl of node.declarationList.declarations) {
        if (decl.name.getText(sf) !== constName) continue;
        const init = ts.isAsExpression(decl.initializer)
          ? decl.initializer.expression
          : decl.initializer;
        if (!ts.isObjectLiteralExpression(init)) continue;
        for (const prop of init.properties) {
          if (
            !ts.isMethodDeclaration(prop) &&
            !ts.isPropertyAssignment(prop)
          ) {
            continue;
          }
          const sig = signatureOf(prop, sf);
          if (!sig) continue;
          const { comment, examples } = jsdoc(prop);
          out.push({ name: prop.name.getText(sf), sig, comment, examples });
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return out;
}

function findDecl(sf, kind, name) {
  let found = null;
  const visit = (node) => {
    if (found) return;
    if (
      ((kind === "interface" && ts.isInterfaceDeclaration(node)) ||
        (kind === "alias" && ts.isTypeAliasDeclaration(node))) &&
      node.name.getText(sf) === name
    ) {
      found = node;
      return;
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return found;
}

function fieldList(decl, sf) {
  return decl.members
    .filter((m) => ts.isPropertySignature(m))
    .map((m) => `\`${m.name.getText(sf)}${m.questionToken ? "?" : ""}\``)
    .join("・");
}

function exampleFence(body) {
  const t = body.trim();
  return t.startsWith("```") ? t : `\`\`\`ts\n${t}\n\`\`\``;
}

function render({ lang, ja, sources, missing }) {
  const H = HEADINGS[lang];
  const out = [];
  const desc = (key, enComment) => {
    if (lang === "ja") {
      if (ja[key]) return ja[key];
      missing.push(key);
      return cleanMd(enComment);
    }
    return cleanMd(enComment);
  };

  for (const { ns, file } of GROUPS) {
    out.push(`## \`${ns}\``, "");
    for (const m of constMembers(sources[file].sf, ns)) {
      const key = `${ns}.${m.name}`;
      out.push(`### \`${key}\``, "", "```ts", m.sig, "```", "");
      const d = desc(key, m.comment);
      if (d) out.push(d, "");
      for (const ex of m.examples) out.push(exampleFence(ex), "");
    }
  }

  out.push(H.interfaces, "");
  for (const { name, file, fields } of INTERFACES) {
    const sf = sources[file].sf;
    const decl =
      findDecl(sf, "interface", name) ?? findDecl(sf, "alias", name);
    if (!decl) throw new Error(`${name} not found in ${file}`);
    const { comment } = jsdoc(decl);
    out.push(
      `### \`${name}\``,
      "",
      "```ts",
      decl.getText(sf).replace(/^export\s+/, ""),
      "```",
      "",
    );
    const d = desc(name, comment);
    if (d) out.push(d, "");
    if (fields) out.push(H.fields(fieldList(decl, sf)), "");
  }

  out.push(H.aliases, "");
  for (const { name, file, catalog, abbrev } of ALIASES) {
    const sf = sources[file].sf;
    const decl = findDecl(sf, "alias", name) ?? findDecl(sf, "interface", name);
    if (!decl) throw new Error(`${name} not found in ${file}`);
    const { comment } = jsdoc(decl);
    let text = decl.getText(sf).replace(/^export\s+/, "");
    if (abbrev) text = abbreviateUnion(text);
    out.push(
      `### \`${name}\``,
      "",
      "```ts",
      text,
      "```",
      "",
    );
    const d = desc(name, comment);
    if (d) out.push(d, "");
    if (catalog) out.push(CATALOG_LINE[lang], "");
  }

  return out.join("\n").trimEnd() + "\n";
}

function splice(path, generated, begin = BEGIN, end = END) {
  const raw = readFileSync(path, "utf8").replace(/\r\n/g, "\n");
  const b = raw.indexOf(begin);
  const e = raw.indexOf(end);
  if (b === -1 || e === -1 || b >= e) {
    throw new Error(`${path} has no (or broken) generation markers`);
  }
  return (
    raw.slice(0, b + begin.length) + "\n\n" + generated + "\n" + raw.slice(e)
  );
}

/** Reads the generated node table from spec/03-uitree.md. */
function specIds() {
  const src = readFileSync(join(root, "spec", "03-uitree.md"), "utf8").replace(
    /\r\n/g,
    "\n",
  );
  const gen = src.slice(
    src.indexOf("<!-- BEGIN GENERATED: node-ids -->"),
    src.indexOf("<!-- END GENERATED: node-ids -->"),
  );
  const groups = [];
  let current = null;
  for (const line of gen.split("\n")) {
    const h = line.match(/^### `([^`]+)`$/);
    if (h) {
      current = { ns: h[1], rows: [] };
      groups.push(current);
      continue;
    }
    const r = line.match(/^\| `([^`]+)` \| (`[^`]*`|—) \| (.*) \|$/);
    if (r && current && r[1] !== "ID") {
      // Requirement codes (FR-001 and friends) stay in the spec; author
      // pages use plain words.
      const role = r[3].trim().replace(/ \([A-Z]{2,4}-\d+\)/g, "");
      current.rows.push({ id: r[1], data: r[2], ja: role });
    }
  }
  return groups;
}

function renderIds({ lang, enRoles, missingIds }) {
  const head = lang === "ja" ? "| ID | `data` | 役目 |" : "| ID | `data` | Role |";
  const out = [];
  for (const { ns, rows } of specIds()) {
    out.push(`## \`${ns}\``, "", head, "|---|---|---|");
    for (const { id, data, ja } of rows) {
      let role = ja;
      if (lang === "en") {
        if (enRoles[id]) role = enRoles[id];
        else {
          missingIds.push(id);
          role = ja;
        }
      }
      out.push(`| \`${id}\` | ${data} | ${role} |`);
    }
    out.push("");
  }
  return out.join("\n").trimEnd() + "\n";
}

function main() {
  const args = process.argv.slice(2);
  const check = args.includes("--check");
  const outIdx = args.indexOf("--out");
  // The api-docs checkout lives next to the gumicord checkout.
  const out = resolve(
    outIdx === -1 ? join(sdk, "..", "..", "api-docs") : args[outIdx + 1],
  );

  const sources = {};
  for (const f of ["index.ts", "uitree.ts", "data.ts", "ids.ts"]) {
    sources[f] = loadSource(f);
  }
  const ja = JSON.parse(readFileSync(join(here, "ja.json"), "utf8"));
  const enRoles = JSON.parse(readFileSync(join(here, "ids-en.json"), "utf8"));
  const missing = [];
  const missingIds = [];

  const writeOrCheck = (file, next, check, stale) => {
    const current = readFileSync(file, "utf8").replace(/\r\n/g, "\n");
    if (current === next) return;
    if (check) {
      stale.push(file);
      return;
    }
    writeFileSync(file, next);
    console.log(`  updated: ${file}`);
  };

  let stale = [];
  for (const lang of ["en", "ja"]) {
    const file = join(out, lang, "plugins", "reference.md");
    writeOrCheck(
      file,
      splice(file, render({ lang, ja, sources, missing })),
      check,
      stale,
    );
    const idsFile = join(out, lang, "theme", "ids.md");
    writeOrCheck(
      idsFile,
      splice(idsFile, renderIds({ lang, enRoles, missingIds }), IDS_BEGIN, IDS_END),
      check,
      stale,
    );
  }

  const unseen = [...new Set(missing)].sort();
  if (unseen.length > 0) {
    console.warn(
      `  warning: no Japanese text for ${unseen.join(", ")}; fell back to English.\n       add them to sdk/apidocs/ja.json`,
    );
  }
  const unseenIds = [...new Set(missingIds)].sort();
  if (unseenIds.length > 0) {
    console.warn(
      `  warning: no English role for ${unseenIds.join(", ")}; fell back to Japanese.\n       add them to sdk/apidocs/ids-en.json`,
    );
  }
  if (check && stale.length > 0) {
    throw new Error(
      `stale generated docs: ${stale.join(", ")}\n       run \`cargo xtask api-docs\` and push the result`,
    );
  }
  console.log("  generated docs are current");
}

main();

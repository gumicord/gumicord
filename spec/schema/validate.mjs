// spec/schema/*.schema.json の検証ツール。
//
// 使い方:
//   node spec/schema/validate.mjs
//
// CI では spec/schema/** または examples/** の変更時に実行する。
// 「スキーマが構文として正しい」だけでなく、
// 「公式サンプルがスキーマを通る」「意図的に壊した入力が落ちる」ことまで確かめる。

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

// ---------------------------------------------------------------- スキーマ自体
console.log("スキーマのコンパイル");
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
  console.log("\ntheme.schema.json をコンパイルできなかったため中断します");
  process.exit(1);
}

// ---------------------------------------------------------------- 公式サンプル
console.log("\n公式サンプルテーマ");
const themesDir = join(repo, "examples", "themes");
if (existsSync(themesDir)) {
  for (const name of readdirSync(themesDir)) {
    const p = join(themesDir, name, "theme.json");
    if (!existsSync(p)) continue;
    const data = JSON.parse(readFileSync(p, "utf8"));
    if (themeValidate(data)) {
      ok(`${name} (${data.rules?.length ?? 0} ルール, ${Object.keys(data.tokens ?? {}).length} トークン)`);
    } else {
      ng(name, JSON.stringify(themeValidate.errors?.slice(0, 3), null, 2));
    }
  }
}

// ---------------------------------------------------------------- 異常系
// スキーマが「通してはいけないもの」を通さないことを確かめる。
// これがないと、緩すぎるスキーマが「全部 OK」と言うだけになる。
console.log("\n異常系 (落ちることを確認する)");

const base = {
  manifest: { id: "dev.gumicord.t", name: "T", version: "1.0.0", abi: 1 },
};

const shouldFail = [
  ["manifest なし", { tokens: {} }],
  ["abi なし", { manifest: { id: "dev.gumicord.t", name: "T", version: "1.0.0" } }],
  ["id が逆ドメインでない", { manifest: { ...base.manifest, id: "midnight" } }],
  ["version が semver でない", { manifest: { ...base.manifest, version: "1.0" } }],
  ["色の書式が不正", { ...base, tokens: { "color.a": "rgb(1,2,3)" } }],
  ["長さが負", { ...base, tokens: { "radius.a": -4 } }],
  ["トークン名が大文字を含む", { ...base, tokens: { "Color.Bg": "#fff" } }],
  ["select にワイルドカード", { ...base, rules: [{ select: "chat.*", style: {} }] }],
  ["select が 5 段", { ...base, rules: [{ select: "a.b.c.d.e", style: {} }] }],
  ["未知の状態名", { ...base, rules: [{ select: "chat.message", when: { state: "pressed" }, style: {} }] }],
  ["未知の platform", { ...base, rules: [{ select: "chat.message", when: { platform: "web" }, style: {} }] }],
  ["未知の when キー", { ...base, rules: [{ select: "chat.message", when: { theme: "dark" }, style: {} }] }],
  ["未知のスタイルプロパティ", { ...base, rules: [{ select: "chat.message", style: { boxShadow: 1 } }] }],
  ["opacity が範囲外", { ...base, rules: [{ select: "chat.message", style: { opacity: 2 } }] }],
  ["padding の要素数が不正", { ...base, rules: [{ select: "chat.message", style: { padding: [1, 2] } }] }],
  ["トークン参照の書式が不正", { ...base, rules: [{ select: "chat.message", style: { color: "$Color.Bg" } }] }],
  ["style なしのルール", { ...base, rules: [{ select: "chat.message" }] }],
  ["トップレベルに未知のキー", { ...base, extra: 1 }],
];

for (const [label, data] of shouldFail) {
  if (themeValidate(data)) {
    ng(`${label} — 通ってしまった`);
  } else {
    ok(label);
  }
}

// ---------------------------------------------------------------- 正常系
console.log("\n正常系 (通ることを確認する)");
const shouldPass = [
  ["最小構成 (manifest のみ)", base],
  ["state の配列", { ...base, rules: [{ select: "chat.message", when: { state: ["hover", "unread"] }, style: {} }] }],
  ["platform の配列", { ...base, rules: [{ select: "chat.message", when: { platform: ["ios", "android"] }, style: {} }] }],
  ["トークンがトークンを参照", { ...base, tokens: { "color.a": "#fff", "color.b": "$color.a" } }],
  ["8 桁の色 (アルファ)", { ...base, tokens: { "color.a": "#ffffff14" } }],
  ["padding の 4 要素", { ...base, rules: [{ select: "chat.message", style: { padding: [1, 2, 3, 4] } }] }],
  ["font の family 省略", { ...base, tokens: { "font.a": { size: 15, lineHeight: 22 } } }],
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
  console.log(`[31m${failed} 件失敗[0m`);
  process.exit(1);
}
console.log("[32mすべて期待どおり[0m");

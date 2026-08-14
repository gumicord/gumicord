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
      continue;
    }

    // スキーマは書式しか見ない。同梱アセットが実在するか、
    // 外部 URL が manifest.remoteAssets で宣言されているか (SEC-022) までは
    // スキーマでは表現できないので、ここで確かめる。
    const dir = join(themesDir, name);
    const declared = new Set(data.manifest?.remoteAssets ?? []);
    const refs = collectAssetRefs(data);
    for (const ref of refs) {
      if (ref.startsWith("data:")) continue;
      if (ref.startsWith("https://")) {
        const host = new URL(ref).hostname;
        if (declared.has(host)) ok(`  外部 ${host} — remoteAssets で宣言済み`);
        else ng(`  外部 ${host} — manifest.remoteAssets に宣言がない (SEC-022)`);
        continue;
      }
      if (existsSync(join(dir, ref))) ok(`  同梱 ${ref}`);
      else ng(`  同梱 ${ref} — ファイルが存在しない`);
    }

    // 宣言したのに一度も使っていないホストは、無用な権限要求である
    const usedHosts = new Set(
      refs.filter((r) => r.startsWith("https://")).map((r) => new URL(r).hostname),
    );
    for (const h of declared) {
      if (!usedHosts.has(h)) ng(`  ${h} を remoteAssets に宣言しているが使っていない`);
    }
  }
}

/** テーマ内のすべてのアセット参照を集める */
function collectAssetRefs(theme) {
  const out = [];
  const visit = (v) => {
    if (!v || typeof v !== "object") return;
    if (Array.isArray(v)) return v.forEach(visit);
    for (const [k, val] of Object.entries(v)) {
      if ((k === "image" || k === "family") && typeof val === "string") {
        // family はフォント名かアセット参照のどちらか。拡張子があればアセットとみなす
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

  // --- 背景画像 (EXT-021, SEC-022) ---
  ["背景に未知のキー", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", repeat: "x" } } }] }],
  ["fit が未知の値", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", fit: "fill" } } }] }],
  ["position の要素数が不正", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", position: [0.5] } } }] }],
  ["position が範囲外", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", position: [1.5, 0] } } }] }],
  ["アセット参照が親ディレクトリへ出る", { ...base, rules: [{ select: "app.window", style: { background: { image: "../secret.png" } } }] }],
  ["アセット参照が親を途中に含む", { ...base, rules: [{ select: "app.window", style: { background: { image: "assets/../../x.png" } } }] }],
  ["アセット参照が絶対パス", { ...base, rules: [{ select: "app.window", style: { background: { image: "/etc/passwd.png" } } }] }],
  ["アセット参照が Windows 絶対パス", { ...base, rules: [{ select: "app.window", style: { background: { image: "C:/x/a.png" } } }] }],
  ["外部 URL が http", { ...base, rules: [{ select: "app.window", style: { background: { image: "http://e.com/a.png" } } }] }],
  ["外部 URL が file スキーム", { ...base, rules: [{ select: "app.window", style: { background: { image: "file:///etc/passwd" } } }] }],
  ["未対応の画像形式", { ...base, rules: [{ select: "app.window", style: { background: { image: "assets/a.bmp" } } }] }],
  ["blur が負", { ...base, rules: [{ select: "app.window", style: { background: { image: "a.png", blur: -1 } } }] }],
  ["remoteAssets がホスト名でない", { manifest: { ...base.manifest, remoteAssets: ["https://cdn.example.com/"] } }],
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

  // --- 背景画像 (EXT-021) ---
  ["背景が色の短縮記法", { ...base, rules: [{ select: "app.window", style: { background: "#0f0f17" } }] }],
  ["背景がトークン参照", { ...base, rules: [{ select: "app.window", style: { background: "$color.bg" } }] }],
  [
    "背景オブジェクト一式",
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
  ["背景を token として定義", { ...base, tokens: { "bg.main": { image: "assets/a.webp", fit: "tile" } } }],
  ["ネストした同梱アセット", { ...base, rules: [{ select: "app.window", style: { background: { image: "assets/img/bg.avif" } } }] }],
  ["data URI の画像", { ...base, rules: [{ select: "app.window", style: { background: { image: "data:image/png;base64,iVBORw0KGgo=" } } }] }],
  [
    "外部 URL + remoteAssets 宣言",
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
  console.log(`[31m${failed} 件失敗[0m`);
  process.exit(1);
}
console.log("[32mすべて期待どおり[0m");

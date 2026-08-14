// SDK の型レベルの保証を検証する。
//
//   test/positive.ts        型検査を通らなければならない
//   test/negative/*.ts      型検査を通ってはならない
//
// 後者がないと「緩すぎる型が全部 OK と言うだけ」になる。
// ADR-0004 は「存在しない ID を指定したらビルドが通らない」と主張しており、
// その主張が実際に成立していることをここで確かめる。

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

/** 単一ファイルを型検査する。エラー出力を返す (空文字なら成功) */
function typecheck(file) {
  const cfg = join(here, ".tsconfig.tmp.json");
  writeFileSync(
    cfg,
    JSON.stringify({
      extends: "../tsconfig.json",
      compilerOptions: {
        noEmit: true,
        // 基底は src/ 配下のみを出力対象にしているため、テストファイルが
        // rootDir の外だと TS6059 で落ちる。打ち消す。
        rootDir: "..",
        declaration: false,
      },
      // 基底の exclude が test/** を除外しているため打ち消す。
      // これを忘れると「入力ファイルが無い (TS18003)」で落ち、
      // 異常系が**間違った理由で通ってしまう**。
      exclude: [],
      include: [file.replace(/\\/g, "/")],
    }),
  );
  try {
    // npx / tsc.cmd を経由しない。
    // Windows では .cmd の spawn に shell が要り、shell を使うと引数が
    // エスケープされない。さらに spawn 自体に失敗したとき出力が空になり、
    // **「型検査を通った」と誤認する**。tsc.js を node で直接叩けば起きない。
    execFileSync(process.execPath, [TSC, "-p", cfg], { cwd: sdk, stdio: "pipe" });
    return "";
  } catch (e) {
    const out = String(e.stdout ?? "") + String(e.stderr ?? "");
    if (!out.trim()) {
      // tsc が起動できていない。型の検証になっていないので、
      // 黙って「落ちた」扱いにしてはならない。
      console.error(`\n\x1b[31mtsc を実行できません\x1b[0m: ${e.message}`);
      process.exit(1);
    }
    return out;
  } finally {
    rmSync(cfg, { force: true });
  }
}

console.log("正常系 (型検査を通ることを確認する)");
{
  const err = typecheck(join(here, "positive.ts"));
  if (err) ng("positive.ts", err);
  else ok("positive.ts");
}

console.log("\n異常系 (型検査で落ちることを確認する)");
// 設定ミスによる失敗を「型エラーで落ちた」と誤認しないよう、
// エラーの種類まで確かめる。TS18003 (入力ファイルなし) や TS5xxx (設定エラー) は
// 型の保証を何も証明していない。
const CONFIG_ERRORS = /error TS(18003|5\d{3}|6\d{3})/;

for (const f of readdirSync(join(here, "negative")).filter((f) => f.endsWith(".ts"))) {
  const err = typecheck(join(here, "negative", f));
  if (!err) {
    ng(`${f} — 通ってしまった`);
    continue;
  }
  if (CONFIG_ERRORS.test(err)) {
    ng(`${f} — 設定エラーで落ちている (型の検証になっていない)`, err);
    continue;
  }
  const first = err.split("\n").find((l) => /error TS\d+/.test(l)) ?? "";
  ok(`${f.padEnd(24)} ${first.replace(/^.*error /, "").slice(0, 64)}`);
}

console.log();
if (failed > 0) {
  console.log(`\x1b[31m${failed} 件失敗\x1b[0m`);
  process.exit(1);
}
console.log("\x1b[32mすべて期待どおり\x1b[0m");

# Android エントリポイント

**M1.2 で着手する。** 現時点では空。

Gradle + NDK のラッパを置く。**薄いラッパに留める** — ライフサイクルと
ネイティブハンドルの受け渡し以外のロジックは `app/core` にある。

## 着手前に確認すること

| 項目 | 理由 |
|---|---|
| `android-game-activity` を使う | `accesskit` の Android 実装は **GameActivity のみ**に対応しており、NativeActivity では動かない (S2 の発見) |
| `InputConnection` の JNI 橋渡し | **規模 XL かつ未検証。** ADR-0005 の見直し条件に直結する最大リスク |
| GLES バックエンド | S1 の発見により、コンピュートシェーダに依存しない描画にしてある |

詳細: [`spec/07-roadmap.md`](../../spec/07-roadmap.md) の 6 章

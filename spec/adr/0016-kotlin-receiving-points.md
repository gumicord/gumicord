# ADR-0016: 自前の Kotlin 受け口は増やせる

| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-10-17 |
| 決定日 | 2026-10-17 |
| 関連要件 | `FR-001`, `PLT-040` |
| 関連 | [ADR-0011](0011-mobile-input-bridges.md) の「Java 自前なし」制約、[ADR-0015](0015-mobile-captcha-hosts.md) の受け口 1 枚に限った覆し、`app/android/README.md` |

---

## 背景

`app/android/README.md` は「自前の Java/Kotlin を持たない」を決定として持ち、
[ADR-0011](0011-mobile-input-bridges.md) も同じ制約を「維持する」と書いていた。
[ADR-0015](0015-mobile-captcha-hosts.md) で `CaptchaBridge.kt` が入ったとき、
利用者は「自前 Kotlin 可・Kotlin 優先」を了承した。ただし覆したのは
captcha の受け口 1 枚だけと明記された。

その結果、Android の後続実装 (ログインの自動入力・SAF) は
「Java 自前なし」の制約に縛られたまま読まれ、足すたびに
「決定を覆すのか」から始めることになる。規則の側が古い文書に残っているので、
ここで置き場を決める。

## 決定

**「自前の Java/Kotlin を持たない」は廃止する。代わりに受け口の形で足す。**

| | |
|---|---|
| 形 | Kotlin の受け口 1 枚 = Rust から呼べる薄い宣言。判断・状態はすべて Rust 側に残す |
| 境界 | そこで初めて触るフレームワーク API を、受け口が JNI より先に隠す。既存 API の使い回しでは受け口を増やさない ([ADR-0015](0015-mobile-captcha-hosts.md) と同じ線引き) |
| 優先 | Rust から触るフレームワーク API は、生 JNI より先に受け口へ置く (利用者の了承「自前 Kotlin 可・Kotlin 優先」)。すでに生 JNI で通っている箇所 (システムバー・Keystore) は動かさない |
| 委譲の仕方 | 生成・配置・リスナは Java main thread でしかできないため、受け口は `runOnUiThread` を内包して Rust に渡す。Rust は戻り値とコールバックだけを見る |
| 一覧 | 受け口は `app/android/README.md` に列挙する。増えたらその表を直す |
| 言語 | 宣言と委譲のみ。ロジックは書かない (書いた場所は Rust のテストで検証できない) |

## 却下した選択肢

| | なぜ却下したか |
|---|---|
| 既存文書の一文を書き換えるだけ (ADR を立てない) | 誰がいつでも「覆した覚えがない」と読める。決定には置き場が要る |
| 生 JNI の自作に戻す | [ADR-0015](0015-mobile-captcha-hosts.md) で受け口のほうが触る面が小さいと判断済み |
| 判断を Kotlin へ移す | 判定が Rust のテストから外れる。判断は Rust に残すのが [ADR-0015](0015-mobile-captcha-hosts.md) 以来の形 |

## 引き受けるリスク

| リスク | 対処 |
|---|---|
| 受け口にロジックが漏れて肥大化する | README の一覧に受け口ごとの行数を載せ、委譲以外の実装が増えたら止める |
| minify が JNI 参照を剥がす | `minifyEnabled` が入るときだけ、既存の `-keep` 方針に合わせる |
| 受け口が増えて往復が重くなる | 一度の呼び出しで済む形に寄せる。Rust と Kotlin を往復する設計は受け口の枚数を上げる理由にならない |

## 見直し条件

- 受け口が 1 つの領域で 100 行を超えて判断を持ちはじめたとき → その領域を Rust へ寄せるか、切り分けを再検討する
- iOS / macOS / Windows でも同じ形が要ると分かったとき → 言語ごとの受け口の線引きを ADR で決める (Swift は [ADR-0015](0015-mobile-captcha-hosts.md) 時点では不要)

## 参考

- `app/android/app/src/main/kotlin/dev/gumicord/app/CaptchaBridge.kt`
- [ADR-0015](0015-mobile-captcha-hosts.md) §「自前の Java/Kotlin を持たない」の扱い

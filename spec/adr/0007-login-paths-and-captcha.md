# ADR-0007: ログイン経路を 2 本持ち、captcha は OS に出させる

| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-08-22 |
| 決定日 | 2026-08-22 |
| 関連要件 | `FR-001`, `FR-002`, `FR-003`, `SEC-002`, `PLT-008` |
| 前提となる決定 | [ADR-0001](0001-native-rust-renderer.md) (自前レンダラ) |

---

## 背景

`FR-001` / `FR-002` は「メールアドレスとパスワードでログインし、TOTP に対応する」と定めている。

**しかし Discord のパスワードログインは hCaptcha を返しうる。**

```jsonc
// POST /api/v9/auth/login → 400
{
  "captcha_key": ["captcha-required"],
  "captcha_service": "hcaptcha",
  "captcha_sitekey": "...",
  "captcha_rqdata": "...",   // enterprise hCaptcha のとき
  "captcha_rqtoken": "..."
}
```

hCaptcha のチャレンジは **JavaScript と canvas でできた web ウィジェット**である。[ADR-0001](0001-native-rust-renderer.md) で自前レンダラを選んだ我々には、これを描く手段がない。

**「角丸矩形・テクスチャ付きクアッド・クリップの 3 つしか持たない」レンダラで、第三者の web ウィジェットは描けない。** これは実装の手間の問題ではなく、原理的に無理である。

---

## 決定

### 1. ログイン経路を 2 本持つ

| | 経路 | captcha |
|---|---|---|
| **既定** | **QR コードログイン** (リモート認証) | **出ない** |
| 代替 | メール + パスワード + TOTP | 出る。下の 2 で解く |

#### QR ログインを既定にする理由

Discord には公式デスクトップクライアントが使っている**リモート認証**がある。

```text
wss://remote-auth-gateway.discord.gg/?v=2     Origin: https://discord.com が必須
  │
  ├─ hello               RSA-2048 OAEP の鍵ペアを生成する
  ├─ init                公開鍵 (SPKI, base64) を送る
  ├─ nonce_proof         秘密鍵で復号して返す
  ├─ pending_remote_init fingerprint (公開鍵の SHA-256) が来る
  │                      QR = https://discord.com/ra/<fingerprint>
  ├─ pending_ticket      スキャンされ、暗号化されたユーザ情報が来る
  └─ pending_login       承認されるとチケット
                         POST /users/@me/remote-auth/login
                         → 暗号化されたトークン → 秘密鍵で復号
```

| | パスワード | QR |
|---|---|---|
| hCaptcha | 出る | **無い** |
| パスワード | 我々のプロセスを通る | **一切触らない** (`SEC-002` の精神) |
| TOTP (`FR-002`) | 我々が実装する | スマホがやる |
| プラットフォーム差 | captcha の出し方が OS ごとに違う | **完全に同じ** |
| QR の描画 | — | **自前レンダラで描ける**。角丸矩形の格子である |

**captcha を解くのではなく、captcha が出ない道を選ぶ。**

#### それでもパスワードログインを捨てない理由

- QR は**公式のモバイルアプリでログインしていること**が前提になる。持っていない利用者を締め出せない
- 新しい端末しか手元にない場合、パスワードが唯一の手段になりうる
- `FR-001` / `FR-002` を満たすには要る

### 2. captcha は OS に出させる。**我々は描かない**

`CaptchaHost` という単一の抽象の裏に、プラットフォームごとの実装を置く。[`TextInputHost`](0006-windows-ime-via-winit.md) と同じ形である。

| プラットフォーム | 出し方 |
|---|---|
| **デスクトップ (M1.1)** | **ループバックのページを既定のブラウザで開く** |
| モバイル (M1.2) | OS の webview (`WKWebView` / `android.webkit.WebView`) |

#### デスクトップでブラウザを使う理由

**理由は 2 つある。片方は「依存を増やさない」だが、もう片方のほうが重い。**

| | |
|---|---|
| 依存 | webview を埋め込むと、ログイン経路のためだけにブラウザエンジンの糊が入る。ブラウザなら 0 |
| **通過率** | **hCaptcha は解く側を指紋で測る。** プロファイルも履歴も無い素の webview は、実在の利用者のブラウザより厳しく扱われうる |

2 つ目が効く。captcha は「人間かどうか」を判定する仕組みであり、**人間が普段使っている環境で解かせるほうが素直**である。

#### モバイルで webview を使う理由

デスクトップと逆で、モバイルは OS の webview が**標準の答え**である。[hCaptcha 公式のモバイル SDK](https://docs.hcaptcha.com/mobile_app_sdks/) がまさに webview の実装であり、ドキュメントにも webview 前提の記述がある。

> If using a webview in a native application, you will need to provide a `host` flag to api.js as it can not detect a hostname inside the webview.
> ... **it is treated as untrusted data; it has no security implications.**

つまり**ドメイン制限は webview の障害にならない**。`captcha_rqdata` も [`setData` / verifyParams](https://docs.hcaptcha.com/configuration/) で渡す口がある。

#### ADR-0001 と衝突しない

**webview もブラウザも、UI を描くためには使わない。** 第三者のチャレンジウィジェットを表示するためだけに使う。

これはファイル選択ダイアログ (`PLT-008`) を OS に任せるのと同じ扱いである。「クライアントの見た目を OS 任せにしない」という主張は、**我々が設計した画面**についての主張であって、hCaptcha のウィジェットについてではない。

### 3. トークンはセキュアストレージにしか置かない

**トークンはアカウントそのものである。** パスワードと同じ重さで扱う。

- 平文のファイルに書かない
- `FR-003` のセキュアストレージ (Windows は DPAPI) にだけ置く
- **P4 が無い間は、トークンを永続化しない。** 起動のたびにログインさせるほうがましである
- `SEC-002` のとおり、プラグインからは決して見えない

---

## 未検証。**ここは推測で埋めない**

| # | 分からないこと | なぜ重要か |
|---|---|---|
| 1 | **Discord は hCaptcha のホスト名を検証しているか** | 検証していれば、ループバックのページで解いたトークンは弾かれる。**この決定の 2 が崩れる** |
| 2 | `captcha_rqdata` (enterprise) の有無で手順が変わるか | 変わるなら分岐が要る |
| 3 | QR ログインが `Origin` ヘッダ以外に何を見ているか | 弾かれたら経路ごと使えない |

> ⚠️ **1 について「ドメインが違っても通った気がする」という記憶がある。**
> **記憶は手掛かりであって事実ではない。** 実装したら最初にここを確かめ、結果をこの ADR に追記する。
>
> [ADR-0005](0005-ime-strategy.md) は、コメントに書き残した疑いを潰さずに承認して間違えた。同じことをしない。

### 1 が崩れたときの逃げ道

| 案 | 評価 |
|---|---|
| ブラウザではなく webview にし、`?host=discord.com` を付ける | hCaptcha 的には有効だが、Discord が siteverify のホスト名を見ているなら同じく崩れる |
| QR ログインだけにする | `FR-001` を降格することになる。**最後の手段** |
| 公式のログインページごと webview で開いてトークンを取る | **却下。** 利用者から見て phishing と区別がつかない形は採らない |

---

## 却下した選択肢

| | なぜ却下したか |
|---|---|
| captcha を自前レンダラで描く | **原理的に無理。** hCaptcha は JS と canvas のウィジェットである |
| captcha 解決サービスに投げる | **論外。** 規約違反であり、利用者の資格情報を第三者へ渡すことになる |
| QR ログインだけにする | 公式モバイルアプリを持たない利用者を締め出す。`FR-001` を満たさない |
| パスワードログインだけにする | captcha の穴 ([未検証 1](#未検証-ここは推測で埋めない)) が塞がらなかったとき、誰もログインできない |
| デスクトップにも webview を埋め込む | 依存が増え、かつ素の webview は captcha の通過率で不利になりうる |

---

## 引き受けるリスク

| リスク | 対処 |
|---|---|
| ホスト名の検証で弾かれる | 実装直後に確かめる。崩れたら上の逃げ道 |
| ブラウザへ飛ぶ体験が悪い | 既定は QR なので、ここを通る利用者は少ないはずである |
| Linux で既定のブラウザが開かない環境がある | URL を画面に出して手で開けるようにする |
| リモート認証は非公開 API である | 公式クライアントが使っている経路なので、壊れれば公式も壊れる。追随はできる |
| QR ログインでも**アカウント停止のリスクは消えない** | サードパーティクライアントであること自体が規約違反である ([00-vision.md](../00-vision.md#リスクと前提)) |

---

## `FR-001` / `FR-002` への影響

要件は**変えない**。「メール + パスワード + TOTP でログインできる」は満たす。

**QR ログインを足す**だけである。`FR-001` に代替経路が増えたと読む。

---

## 参考

- [Remote Authentication (Desktop) — Discord Userdoccers](https://docs.discord.food/remote-authentication/desktop)
- [Remote Authentication Overview — Discord Userdoccers](https://docs.discord.food/remote-authentication/overview)
- [Mobile App SDKs — hCaptcha](https://docs.hcaptcha.com/mobile_app_sdks/)
- [Configuration — hCaptcha](https://docs.hcaptcha.com/configuration/)

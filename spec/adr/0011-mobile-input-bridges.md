# ADR-0011: モバイルのテキスト入力は自前橋渡しで実装する

| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-09-11 |
| 決定日 | 2026-09-11 |
| 関連要件 | `PLT-001`, `PLT-002`, `PLT-040` |
| 関連 | [ADR-0005](0005-ime-strategy.md), [ADR-0006](0006-windows-ime-via-winit.md)、A2 / I2 |

---

## 背景

実機 (Android / iOS) でテキスト入力に 3 つの症状が出た。

| 症状 | 原因 |
|---|---|
| 入力欄を押しても入力できない | 橋渡しが存在しない。Android の `InputConnection` も iOS の `UITextInput` も未実装 (v0.0.5 の A2 / I2) |
| iOS で欄外を押してもキーボードが閉じない | first responder を辞めさせるコードがどこにもない |
| iOS でキーボード周りが重い | `sync_ime_proxy` が表示中ずっと毎フレーム `request_redraw` するビジースピン |

隠しネイティブ編集欄の一般化 (ログインプロキシの拡張) を提案したが、**利用者の選択により仕様通り (自前橋渡し) に進める。**

## 決定

**[`TextInputHost`](0006-windows-ime-via-winit.md) の抽象と [`TextDocument`] は変えない。** 文書が唯一の真実であり、位置は UTF-8 のバイト位置で通す。変わるのは文書を操作する層の中身だけである。

### A2: 生 JNI の `InputConnection` は書かない

GameActivity は GameTextInput を内蔵している (`onCreateInputConnection` が gametextinput の接続を返し、状態は `textInputState` フラグで届く)。`android-activity` 0.6 が錆びない API で公開している。

| 使うもの | 用途 |
|---|---|
| `text_input_state()` | IME 側の本文・選択・変換域の取り出し (ポーリングが公式の使い方) |
| `set_text_input_state()` | 自前の編集 (ハードキー等) の IME への反映 |
| `set_ime_editor_info()` | 欄種別 (本文・メール・パスワード・数字)。複数行もここで |

公開 API に take 系はないため、新着の検出は前回スナップとの比較で行う。自前の反映もスナップを更新するので、その反響を新着と誤認しない。

アクションボタンの取得口も公開 API にないため、アクションは `None` に固定する。単一行欄でのリターンは本文中の改行として届くので、末尾の改行を検出して前進・送信に使う (iOS のログインプロキシと同じ形)。複数行の改行はそのまま残す。

`winit` は `TextEvent` を転送しない (0.30 のソースで確認) ため、platform がイベントループスレッドで直接ポーリングする。GameTextInput の取得はスレッドセーフではないと `android-activity` 自身が警告しているので、他スレッドからは触らない。

索引の単位に注意する。GameTextInput の選択・変換域は **UTF-16 単位** (Java の `String` 偏移) であり、`TextDocument` の UTF-8 バイト位置へ両方向の変換が要る。日本語は 1 文字 3 バイトのため、そのまま通すとずれる。

フォーカス中は 16ms のタイマーで起こしてポーリングする (`NFR-006`)。打鍵は `winit` のイベントを起こさないため、待ちだけではエコーが遅れる。

「Java 自前なし」の制約は維持する。`games-activity` は 4.x 系に pin したまま (`android-activity` 0.6 が話せる相手)。

### I2: `UITextInput` を `objc2` で実装する

`winit` の iOS ビューは `UIKeyInput` しか話さないため、プロトコル実装は自前になる。`winit` のビューの兄弟として不可視のエディタビューを置き、そこに `UITextInput` (+ `UIKeyInput` + `UITextInputTraits`) を実装する。ログインプロキシ (`proxy.rs`) と違い、**本物の `UITextInput`** なので変換・候補・自動修正は OS の標準動作になり、候補ウィンドウの位置指定 (`PLT-001`) も正しく効く。

あわせて殻を直す。欄外タップで blur＋resign (キーボードを閉じる)、プロキシの毎フレームスピンをやめてイベント＋タイマー駆動にする。キーボードの高さ追従 (`PLT-040`) は iOS のキーボード通知で取る。

### 却下した選択肢

| | なぜ却下したか |
|---|---|
| 隠し欄の一般化 (不可視 `UITextField` / `EditText` の使い回し) | 候補ウィンドウの位置が 1px 欄に引きずられる、変換域の忠実度が落ちる、不可視ビューの細工が残り続ける。変換の正しさを取る |
| `winit` の上流修正を待つ | `TextEvent` の転送も `UITextInput` も上流の計画にない。待つ側の工程が読めない |

## 引き受けるリスク

| リスク | 対処 |
|---|---|
| GameTextInput の取得がスレッドセーフでない | イベントループスレッドからのみ呼ぶ。タイマーポーリングも同スレッド |
| UTF-16 と UTF-8 の取り違え | 橋渡しの境界で両方向に変換し、日本語で検証する (`PLT-002`) |
| `games-activity` の更新で Java 側の振る舞いが変わる | 4.x に pin 済み。上げるときは実機で入力から確認する |
| `UITextInput` プロトコルが大きい (位置・範囲オブジェクトを含む) | 機械的な実装。不可視ビューのため描画の干渉はない |
| Android のキーボード高さ取得が別途 JNI を要する | `PLT-040` として v0.0.5 の範囲。可視領域の高さポーリングで取る |

## 見直し条件

- GameTextInput では変換が成立しないと分かったとき → 生 JNI の `InputConnection` へ (Java 自前なしの制約ごと再検討)
- `UITextInput` の実装が不可能と分かったとき → [ADR-0001](0001-native-rust-renderer.md) ごと再検討する (仕様書の条件どおり)

## 参考

- [`TextDocument`](../../render/platform/src/text_input/document.rs)
- GameActivity のテキスト入力: https://developer.android.com/games/agdk/game-activity/use-text-input
- GameTextInput: https://developer.android.com/games/agdk/add-support-for-text-input
- `android-activity` の警告 (スレッド安全性、modified UTF-8): `android-activity-0.6.1/src/game_activity/mod.rs`

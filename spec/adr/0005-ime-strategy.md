# ADR-0005: IME テキスト入力層をどう実装するか

| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-08-14 |
| 決定日 | 2026-08-14 |
| 関連要件 | `PLT-001`, `PLT-002` |
| 発端 | [ADR-0001 スパイク S2 の測定結果](0001-native-rust-renderer.md#スパイク-s2-の測定結果-2026-08-14) |

## 背景

スパイク S2 で、`winit` を使った日本語入力は以下の状態であることが分かった。

| | 状態 |
|---|---|
| 未確定文字列 (preedit) の受信と表示 | ✅ 動く |
| 文節の部分変換 | ✅ 動く |
| 変換確定 (`Commit`) | ✅ 動く |
| **変換候補ウィンドウ** | ❌ **画面のどこにも表示されない** |

## 原因の切り分け (進行中)

### 検証済みで否定された仮説

| # | 仮説 | 検証 | 結果 |
|---|---|---|---|
| H1 | `set_ime_cursor_area` を毎フレーム 60 回呼んでいるため IME が追従できない | 位置が変化したときのみ呼ぶよう変更 | ❌ 改善せず |
| H2 | アプリが渡す座標が間違っている | ログでキャレット追従を確認 (`x=188→204→220→...`) | ❌ 座標は正しい |
| H3 | `winit` が候補ウィンドウを抑制している | `WM_IME_SETCONTEXT` の処理を確認 | ❌ **抑制していない** |

H3 について。`winit` は以下のように `ISC_SHOWUICOMPOSITIONWINDOW` のみを落としている。

```rust
WM_IME_SETCONTEXT => {
    // IME UI visibility flags are in lparam.
    let lparam = lparam & !(ISC_SHOWUICOMPOSITIONWINDOW as isize);
    result = ProcResult::Value(unsafe { DefWindowProcW(window, msg, wparam, lparam) });
},
```

これは**インライン表示 (未確定文字列の OS 側描画) の抑制**であり、アプリが preedit を自前描画する以上は正しい。**候補ウィンドウのフラグ `ISC_SHOWUICANDIDATEWINDOW` は落としていない。**

| # | 仮説 | 検証 | 結果 |
|---|---|---|---|
| H4 | そもそも候補リストを開く操作をしていない (Microsoft IME はスペース 1 回では変換するだけ) | 実機で確認 | ❌ **候補は巡回している** (`こんにちは` → `今日は` → `コンニチハ`) のに候補ウィンドウは出ない |
| H7 | `set_ime_cursor_area` を呼ぶこと自体が候補ウィンドウを消している (winit は `CFS_EXCLUDE` で矩形を指定する) | 一切呼ばない設定で比較 | ❌ **呼ばなくても出ない** |

### 特定された原因 ✅

**`winit` が `WM_IME_COMPOSITION` を握り潰し、`DefWindowProc` に渡していない。**

```rust
// winit-0.30.13 src/platform_impl/windows/event_loop.rs
WM_IME_COMPOSITION => {
    // ... preedit を取り出して Ime::Preedit として送出 ...

    // Not calling DefWindowProc to hide composing text drawn by IME.
    result = ProcResult::Value(0);
},
```

IMM32 では **`DefWindowProc` が IME の UI ウィンドウへメッセージを転送する経路**であり、その UI ウィンドウが**インライン表示と変換候補ウィンドウの両方**を描く。

つまり `winit` は「インライン表示を消すため」にメッセージを握り潰し、**巻き添えで候補ウィンドウも殺している**。

#### 実証

`winit` をローカルにフォークし、この 1 行だけを変更して比較した。

```diff
- result = ProcResult::Value(0);
+ result = ProcResult::DefWindowProc(wparam);
```

**結果: OS の IME UI が表示されるようになった。** 診断は正しかった。

### しかし単純な転送では解決しない ❌

転送に切り替えた結果、以下の 2 つの問題が起きた。

1. **未確定文字列が二重に表示される。** 自前描画の下線つきテキストと、OS 側が描くボックスが同時に出る
2. **表示されるのが利用者の IME の UI ではない。** 検証環境は **Google 日本語入力** を使っているが、出てきたのは旧来の IMM32 の UI だった

`winit` は `WM_IME_SETCONTEXT` で `ISC_SHOWUICOMPOSITIONWINDOW` を落としているにもかかわらず、インライン表示が抑制されていない。

### 結論: 原因は最終的に TSF だった ✅

**Google 日本語入力は TSF ベースの IME である。** ATOK など主要な日本語 IME も同様である。

アプリが TSF のテキストストアを実装していない場合、これらの IME は **TSF → IMM32 の互換ブリッジ**経由で動作する。その状態では:

- IMM32 のメッセージを握り潰すと **UI が一切出ない** (`winit` の現状)
- IMM32 のメッセージを転送すると **退化した旧来の UI が二重に出る** (フォークして試した状態)

**どちらも実用にならない。** 利用者が普段使っている IME の UI を正しい位置に出すには、**アプリ側が TSF のテキストストア (`ITextStoreACP`) を実装する必要がある。**

> 当初 H5 (TSF) を有力とし、その後「TSF なら変な場所に出るはずで、どこにも出ないのは説明できない」として H4 に傾いた。
> **結論としては H5 が正しかったが、当初の理由付けは誤っていた。**
> 実際の因果は「TSF IME が IMM32 ブリッジで動作 → `winit` が IMM32 メッセージを握り潰す → UI が出ない」という 2 段階だった。
> 握り潰しをやめて初めて「TSF ブリッジの退化した UI」が姿を現し、真の原因が見えた。

### 検証環境について

| | |
|---|---|
| IME | **Google 日本語入力** (TSF ベース) |
| OS | Windows 10 Pro 22H2 |

Microsoft IME (これも TSF ベース) でも同じ結果になると考えられるが、**未確認**。ATOK も同様と推測されるが未確認。

## 問題の一般化

Windows だけの話ではない。**`winit` はウィンドウと生の入力イベントの面倒は見るが、テキスト入力の面倒は見ない。**

| プラットフォーム | 必要な実装 | 見込み |
|---|---|---|
| Windows | TSF テキストストア (`ITextStoreACP`) | 自前 |
| macOS | `NSTextInputClient` | winit が一部対応。要確認 |
| Linux | ibus / fcitx5 (Wayland: `text-input-v3`) | winit が一部対応。要確認 |
| **Android** | `InputConnection` の JNI 橋渡し | **自前** |
| **iOS** | `UITextInput` プロトコル | **自前** |

つまり **S2 の本当の結論は「テキスト入力層を最大 5 プラットフォーム分書く必要がある」** である。これは Chromium・Firefox・Flutter がそれぞれ数千行かけている領域であり、軽くはない。

## 選択肢

### A. テキスト入力層を自前実装する (ADR-0001 継続)

- ○ `PLT-001` を完全に満たせる。候補ウィンドウの位置も、将来的には**候補リストの自前描画**まで到達できる
- ○ 参照実装が豊富 (Chromium / Firefox / Flutter がすべてオープンソース)
- × プラットフォームごとに数千行規模の COM / JNI / Obj-C 相互運用コード
- × このプロジェクトで最も面白くない部分に、最も長い時間がかかる

### B. 候補ウィンドウの位置を諦める (`PLT-001` を降格)

- ○ 実装コストがゼロ
- × 変換候補が入力欄と無関係な場所に出る。**日本語話者にとって日常的な劣化**であり、Gumicord の対象ユーザーがまさに日本語話者である
- × 「公式より良い体験」というビジョンの目標 4 と正面から矛盾する

### C. `winit` に TSF 対応を実装して上流へ送る

- ○ 作業量は A とほぼ同じで、エコシステムに還元される
- ○ Gumicord 以外の Rust GUI アプリも救われる
- × 上流のレビューとマージに要する時間が読めない。フォークで先行する運用が必要

### D. ADR-0001 を見直して Flutter へ切り替える

- ○ **この問題は Flutter では解決済みである。** Flutter の Windows エンベッダは TSF を実装しており、Android / iOS のテキスト入力も面倒を見る
- ○ ADR-0004 (セマンティック UITree を唯一の ABI とする) は Flutter 上でも維持できる
- × S1 で実測した優位 (バイナリ 4.66MB / 常駐 69.7MB / 起動 332ms) を手放す
- × **プラグインの自由度が Flutter のウィジェットモデルに制約される。** 描画パスの差し替えやカスタムシェーダの注入といった「無限のカスタム度」は諦めることになる
- × S1・S2 の成果 (SDF バッチャ・グリフアトラス) を捨てる

> 公平を期すために記す。[ADR-0001](0001-native-rust-renderer.md) の検討時、選択肢 A (Flutter) の利点として「IME が堅い」を挙げていた。**S2 はその評価が正しかったことを具体的に示した。**
> 同時に、Flutter を却下した理由 (プラグインの自由度がフレームワークに制約される) は S2 によって何も変わっていない。

### E. A + C の併用 (自前実装しつつ上流へ還元する)

- Gumicord 側では `winit` のフォークで先行し、実装が固まった段階で上流へ PR を送る
- 上流の判断を待たずに進められ、かつ還元もできる

## 選択肢 B の再評価 (原因特定を受けて)

原因特定の前は、B は「候補ウィンドウの位置がずれるのを我慢する」という話だと思われていた。**実際にはもっと悪い。**

`winit` の現状のままだと、**候補ウィンドウが一切出ない**。日本語入力において候補リストが見えないというのは、変換対象を目で確認できないということであり、**チャットクライアントとして成立しない**。

したがって **B (諦める) は選択肢から外れる。**

## 判断に必要な追加情報

**Android と iOS の IME は未検証のままである。** Windows で TSF を実装しても、モバイルで同等の壁に当たる可能性がある。
選択肢 A / C / E を採る場合、**先に Android の `InputConnection` を検証してから本格着手する**のが順序として正しい。Windows に数週間かけた後にモバイルで詰むのが最悪の展開である。

## 工数の見積もり材料

TSF テキストストアの実装で必要になる主なもの:

| インタフェース | 役割 |
|---|---|
| `ITextStoreACP` | テキストの読み書き・選択範囲・ロック管理。中核 |
| `ITextStoreACPSink` (受け側) | TSF からの通知を受ける |
| `ITfContextOwnerCompositionSink` | 合成の開始・更新・終了 |
| `ITfThreadMgr` / `ITfDocumentMgr` | スレッドと文書の管理 |
| `GetTextExt` | **候補ウィンドウの位置決めに使われる。今回の問題の直接の解** |

参照実装 (いずれもオープンソース):

- Chromium `ui/base/ime/win/tsf_text_store.cc`
- Firefox `widget/windows/TSFTextStore.cpp`
- Flutter `windows/text_input_plugin.cc` (ただし Flutter は IMM32 と TSF を併用)

いずれも数千行規模だが、**Gumicord が必要とするのは単一行入力欄と複数行入力欄のみ**であり、リッチテキストや文書全体の編集は不要なため、削れる余地は大きい。**実際にどこまで削れるかは書いてみないと分からない。**

## 決定

**テキスト入力層をプラットフォームごとに自前実装する。ADR-0001 (Rust 自前レンダラ) を継続する。**

| プラットフォーム | 実装するもの |
|---|---|
| Windows | TSF テキストストア (`ITextStoreACP` ほか) |
| Android | `InputConnection` の JNI 橋渡し |
| iOS | `UITextInput` プロトコル |
| macOS | `NSTextInputClient`。`winit` の実装で足りるか要確認 |
| Linux | ibus / fcitx5 / Wayland `text-input-v3`。`winit` の実装で足りるか要確認 |

`winit` への上流還元は**行わない**。フォークまたは `winit` の外側で自前のウィンドウプロシージャを持つ形で実装する。

### 理由

1. **選択肢 B (諦める) は脱落した。** 候補ウィンドウが一切出ないのは日本語入力として成立しない
2. **選択肢 D (Flutter) は要件と引き換えになる。** IME 層のコストは消えるが、S1 で実測した性能優位 (バイナリ 4.66MB / 常駐 69.7MB / 起動 332ms) と、描画パスの差し替えレベルのプラグイン自由度を失う。**ADR-0001 の決め手だった「× のコストは時間だが、他案の × は要件を満たせないこと」という判断は、S2 の結果によって変わっていない**
3. **コストは既知になった。** S2 の前は「何が必要かも分からない」状態だったが、いまは「TSF テキストストアを書く」と特定できている。参照実装もすべてオープンソースで存在する
4. Gumicord が必要とするのは**単一行と複数行の入力欄のみ**であり、リッチテキスト編集や文書全体の操作は不要。Chromium や Firefox の実装から削れる余地は大きい

### 設計方針

**プラットフォーム固有のテキスト入力コードは `gumicord-platform` の 1 つの抽象の裏に閉じ込める。**

```
gumicord-render
      │  TextInputHost (共通の抽象)
      │    - preedit の変更を通知する
      │    - 確定文字列を通知する
      │    - キャレット矩形を問い合わせられる  ← TSF の GetTextExt はここに対応
      │    - 選択範囲を読み書きできる
      ▼
gumicord-platform
      ├─ windows/tsf.rs        ITextStoreACP
      ├─ android/ime.rs        InputConnection (JNI)
      ├─ ios/text_input.rs     UITextInput
      ├─ macos/text_input.rs   NSTextInputClient
      └─ linux/text_input.rs   ibus / fcitx5 / text-input-v3
```

TSF の `GetTextExt` (候補ウィンドウの位置決めに使われる) が、Android の `InputConnection` や iOS の `UITextInput` にも同種の要求として現れる。**「キャレット矩形を問い合わせられる」という形で抽象化すれば 5 プラットフォームに共通の形に収まる**見込みである。

### 引き受けるリスク

| リスク | 影響 | 緩和 |
|---|---|---|
| **Android / iOS の IME が未検証** | 想定より遥かに重い、あるいは不可能と判明する | Windows TSF の実装で得た知見と工数実績をもって、早期に Android を検証する |
| TSF 実装が想定の数倍かかる | M1 が遅延する | 最小実装で「候補ウィンドウが正しい位置に出る」を先に達成し、機能を段階的に足す |
| `winit` のフォーク維持コスト | 追従が負担になる | ウィンドウプロシージャの差し込みだけで済むなら `winit` 本体は素のまま使う。可能性を実装時に探る |

## 見直し条件

- **Android または iOS の IME が実装不可能と判明したとき** → ADR-0001 ごと再検討する (選択肢 D)
- Windows TSF の実装工数が当初見積もりの 3 倍を超えたとき → 残り 4 プラットフォーム分の見積もりを引き直し、ADR-0001 を再検討する

## 参考

- [Mozilla Bug 1081993 — TSF モードで IMM32 の候補位置指定が効かない](https://bugzilla.mozilla.org/show_bug.cgi?id=1081993)
- [winit #2886 — set_ime_position のカーソル領域指定](https://github.com/rust-windowing/winit/issues/2886)

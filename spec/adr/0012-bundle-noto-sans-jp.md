# ADR-0012: CJK は Noto Sans JP のサブセット可変フォントを同梱する
| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-09-12 |
| 決定日 | 2026-09-12 |
| 関連要件 | `EXT-020`, `FR-021`, `NFR-001` |
| 関連 | [spec/06-renderer.md](../06-renderer.md) 6.4、`assets/fonts/README.md` |

---

## 背景

日本語・全角文字の書体が機種依存だった。

- 全角英字・全角数字・長音は Latin / Common スクリプトに分類され、cosmic-text はそのスクリプトを収集しない。`script_fallback` をどう書いても届かず、共通表と全体走査の気まぐれで決まっていた
- 共通表に日本語表を足して解決はしたが、表の先にある実フォントが機種で違う (Yu Gothic / BIZ / Meiryo でメトリクスが違う)。`EXT-020` (同一の描画結果) は表では成立しない
- `assets/fonts/README.md` が見込んでいた 5.7MB に対し、現行の Subset Variable TTF は実測 9.1MB ある

## 決定

**[Noto Sans JP のサブセット可変フォント](https://github.com/googlefonts/noto-cjk/raw/main/Sans/Variable/TTF/Subset/NotoSansJP-VF.ttf) (`notofonts/noto-cjk`) を同梱し、CJK フォールバックの先頭に置く。**

- 可変 (wght) 1 本で Inter と同じ運用になる。テーマの 400 / 600 をはじめ任意の重さに対応する
- OFL-1.1。ライセンス全文を `assets/fonts/NotoSansJP-OFL.txt` に同梱する
- 非日本語スクリプトは従来どおりシステムフォントへ落ちる
- フォント表自体は残す。収録外の字形と他言語は引き続きそれで拾う

## 却下した案

| 案 | 却下理由 |
|---|---|
| BIZ UDPGothic 静的×2 (Regular 4.5MB + Bold 4.4MB) | 合計で重く、重さも 2 通りだけ。UI 向きの書体ではあるが可変に劣る |
| M PLUS 2 可変 | 小さいが、字形網羅と UI 実績で Noto に劣る |
| IBM Plex Sans JP | 可変がなく静的 7 ファイルになる |
| OTF (CFF2) 版のサブセット可変 | 小さいが、ラスタライザの CFF2 対応が未検証 |
| 自前サブセット生成 | 生成道具がなく再現性が取れない。バイナリ肥大が問題化したら再検討する |

## 見直す条件

- バイナリ肥大が配布の支障になったら、自前サブセット (JIS X 0208・かな・全角・記号に絞る) を再検討する。その際は Ext.B 以降の人名漢字がシステム頼みに戻ることを受け入れる
- 日本語以外の CJK 言語を同レベルで確定させる必要が出たら、その言語の表と書体を追加する

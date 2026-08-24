//! Placeholder data, to be deleted once the store is real.
//!
//! Driving the renderer and the theme end to end needs something on screen
//! before the Gateway and REST exist. This is scaffolding, not a feature.
//!
//! The Japanese is deliberate: CJK glyphs and wrapping both want watching at
//! this stage.

use std::borrow::Cow;

pub struct Guild {
    pub id: u64,
    pub name: &'static str,
    pub unread: bool,
    pub mentions: u32,
}

pub struct Channel {
    pub id: u64,
    /// The icon naming the channel's kind.
    pub icon: &'static str,
    pub name: &'static str,
    pub unread: bool,
    pub mentions: u32,
}

pub struct Message {
    pub id: u64,
    pub author: Cow<'static, str>,
    pub time: Cow<'static, str>,
    pub body: Cow<'static, str>,
    /// Whether it mentions you.
    pub mentioned: bool,
}

pub const GUILDS: &[Guild] = &[
    Guild {
        id: 1,
        name: "Gumicord",
        unread: false,
        mentions: 0,
    },
    Guild {
        id: 2,
        name: "Rust 日本語",
        unread: true,
        mentions: 3,
    },
    Guild {
        id: 3,
        name: "wgpu",
        unread: true,
        mentions: 0,
    },
    Guild {
        id: 4,
        name: "個人メモ",
        unread: false,
        mentions: 0,
    },
];

pub const CHANNELS: &[Channel] = &[
    Channel {
        id: 10,
        icon: "channel.text",
        name: "はじめに",
        unread: false,
        mentions: 0,
    },
    Channel {
        id: 11,
        icon: "channel.text",
        name: "雑談",
        unread: true,
        mentions: 0,
    },
    Channel {
        id: 12,
        icon: "channel.text",
        name: "開発",
        unread: true,
        mentions: 2,
    },
    Channel {
        id: 13,
        icon: "channel.text",
        name: "テーマ作成",
        unread: false,
        mentions: 0,
    },
    Channel {
        id: 14,
        icon: "channel.text",
        name: "プラグイン",
        unread: false,
        mentions: 0,
    },
    Channel {
        id: 15,
        icon: "channel.text",
        name: "バグ報告",
        unread: false,
        mentions: 0,
    },
];

pub static MESSAGES: &[Message] = &[
    Message {
        id: 100,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("昨日 21:04"),
        body: Cow::Borrowed(
            "レンダラを縦に通した。UITree → テーマ解決 → レイアウト → 描画 が全部つながっている。",
        ),
        mentioned: false,
    },
    Message {
        id: 101,
        author: Cow::Borrowed("みどり"),
        time: Cow::Borrowed("昨日 21:07"),
        body: Cow::Borrowed(
            "おお、ということはこの画面自体が examples/themes/midnight の適用結果ということ？",
        ),
        mentioned: false,
    },
    Message {
        id: 102,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("昨日 21:08"),
        body: Cow::Borrowed(
            "そう。テーマの JSON を書き換えれば、この見た目はそのまま変わる。ハードコードした色はひとつもない。",
        ),
        mentioned: false,
    },
    Message {
        id: 103,
        author: Cow::Borrowed("みどり"),
        time: Cow::Borrowed("昨日 21:12"),
        body: Cow::Borrowed(
            "長い行の折り返しも見ておきたいな。cosmic-text で整形しているなら、日本語の途中でも自然な位置で折り返せるはずで、英語と混ざったときに variable font のフォールバックがどう効くかも気になる。",
        ),
        mentioned: false,
    },
    Message {
        id: 104,
        author: Cow::Borrowed("sururu"),
        time: Cow::Borrowed("今日 09:31"),
        body: Cow::Borrowed("@ねんねこ メンションの当たったメッセージだけ枠が付くのも確認できる？"),
        mentioned: true,
    },
    Message {
        id: 105,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("今日 09:33"),
        body: Cow::Borrowed(
            "付いてる。これは when.state = mentioned のルールがそのまま効いている。",
        ),
        mentioned: false,
    },
    Message {
        id: 106,
        author: Cow::Borrowed("みどり"),
        time: Cow::Borrowed("今日 09:40"),
        body: Cow::Borrowed("次は日本語入力 (TSF) だね。変換候補ウィンドウが出ないと使えない。"),
        mentioned: false,
    },
    Message {
        id: 107,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("今日 09:41"),
        body: Cow::Borrowed(
            "そこが M1.1 のクリティカルパス。ADR-0005 のとおり自前で ITextStoreACP を持つ。",
        ),
        mentioned: false,
    },
    // Below is a run from one author, to watch the header not repeat.
    Message {
        id: 108,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("今日 09:42"),
        body: Cow::Borrowed(
            "ついでにスクロールバーも入れた。摘みの大きさと位置ははみ出し量から決まるので、レンダラが計算している。",
        ),
        mentioned: false,
    },
    Message {
        id: 109,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("今日 09:43"),
        body: Cow::Borrowed(
            "テーマが決めるのは幅と余白と色だけ。layout.scrollbar と layout.scrollbar.thumb を足した。",
        ),
        mentioned: false,
    },
    Message {
        id: 110,
        author: Cow::Borrowed("みどり"),
        time: Cow::Borrowed("今日 09:50"),
        body: Cow::Borrowed("連投したときに送信者行が消えるのは when.state = grouped でやってる？"),
        mentioned: false,
    },
    Message {
        id: 111,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("今日 09:51"),
        body: Cow::Borrowed(
            "そう。字下げの量もテーマの padding で決まる。クライアント側に焼き付けると、テーマごとに揃えられなくなる。",
        ),
        mentioned: false,
    },
    Message {
        id: 112,
        author: Cow::Borrowed("ねんねこ"),
        time: Cow::Borrowed("今日 09:52"),
        body: Cow::Borrowed("次は通信 (C1〜C4) と日本語入力 (P2)。ここまでは全部ダミーデータ。"),
        mentioned: false,
    },
];

/// The first letter of the name, standing in until avatars load.
pub fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

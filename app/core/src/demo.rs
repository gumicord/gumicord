//! 仮のデータ。**Store (C5) ができたら丸ごと消える。**
//!
//! レンダラとテーマを縦に通すために、Gateway も REST もない状態で
//! 画面へ流し込むものが要る。ここにあるのはその足場であり、
//! **クライアントの機能ではない。**
//!
//! 日本語を混ぜてあるのは意図的である。CJK のグリフが出ること、
//! 折り返しが効くことをこの段階で見ておきたい。

pub struct Guild {
    pub id: u64,
    pub name: &'static str,
    pub unread: bool,
    pub mentions: u32,
}

pub struct Channel {
    pub id: u64,
    /// 種別を表すアイコンの名前 (`gumicord_render::icon`)
    pub icon: &'static str,
    pub name: &'static str,
    pub unread: bool,
    pub mentions: u32,
}

pub struct Message {
    pub id: u64,
    pub author: &'static str,
    pub time: &'static str,
    pub body: &'static str,
    /// 自分宛てのメンションを含むか
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

pub const MESSAGES: &[Message] = &[
    Message {
        id: 100,
        author: "ねんねこ",
        time: "昨日 21:04",
        body: "レンダラを縦に通した。UITree → テーマ解決 → レイアウト → 描画 が全部つながっている。",
        mentioned: false,
    },
    Message {
        id: 101,
        author: "みどり",
        time: "昨日 21:07",
        body: "おお、ということはこの画面自体が examples/themes/midnight の適用結果ということ？",
        mentioned: false,
    },
    Message {
        id: 102,
        author: "ねんねこ",
        time: "昨日 21:08",
        body: "そう。テーマの JSON を書き換えれば、この見た目はそのまま変わる。ハードコードした色はひとつもない。",
        mentioned: false,
    },
    Message {
        id: 103,
        author: "みどり",
        time: "昨日 21:12",
        body: "長い行の折り返しも見ておきたいな。cosmic-text で整形しているなら、日本語の途中でも自然な位置で折り返せるはずで、英語と混ざったときに variable font のフォールバックがどう効くかも気になる。",
        mentioned: false,
    },
    Message {
        id: 104,
        author: "sururu",
        time: "今日 09:31",
        body: "@ねんねこ メンションの当たったメッセージだけ枠が付くのも確認できる？",
        mentioned: true,
    },
    Message {
        id: 105,
        author: "ねんねこ",
        time: "今日 09:33",
        body: "付いてる。これは when.state = mentioned のルールがそのまま効いている。",
        mentioned: false,
    },
    Message {
        id: 106,
        author: "みどり",
        time: "今日 09:40",
        body: "次は日本語入力 (TSF) だね。変換候補ウィンドウが出ないと使えない。",
        mentioned: false,
    },
    Message {
        id: 107,
        author: "ねんねこ",
        time: "今日 09:41",
        body: "そこが M1.1 のクリティカルパス。ADR-0005 のとおり自前で ITextStoreACP を持つ。",
        mentioned: false,
    },
];

/// 名前の 1 文字目。アイコン画像 (R5) ができるまでの代用
pub fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

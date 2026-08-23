//! 正規化された状態のインメモリ保持と SQLite への永続化。
//!
//! 責務: Gateway イベントの状態への反映 / ローカルキャッシュ / 全文検索 (FTS5) / 暗号化。
//!
//! ⚠️ **起動時はここから先に描画する。** S4 の実測で Gateway の READY 到達に
//! 672〜1120 ms かかることが分かっており、`NFR-001` (コールドスタート 500ms) の
//! 「操作可能」の定義に READY を含めることはできない。
//!
//! # まだ無いもの
//!
//! | | いつ |
//! |---|---|
//! | 全文検索 (FTS5) | M2 (`FR-035`) |
//! | キャッシュの暗号化 | M2 (`SEC-020`)。⚠️ **本文が平文でディスクに残る** |
//! | 送信キューのオフライン退避 | M2 (`NFR-012`) |
//! | 未読のミュート | 通知設定 (`FR-041`, M2) を読んでいないので、**黙らせたチャンネルも光る** |
//! | 未読の保存 | 読んだ印は READY が毎回持ってくるので残していない |
//!
//! 要件: `FR-035`, `NFR-011`, `NFR-012`, `SEC-020`, `SEC-021`
//! 仕様: [`spec/02-architecture.md`]

pub mod db;

pub use db::{Db, DbError, Snapshot, default_path};

use std::collections::{HashMap, HashSet};

use gumicord_model::{
    Asset, Channel, ChannelId, Guild, GuildId, Message, MessageId, Role, RoleId, UserId,
};

/// 正規化された状態。
///
/// # 「正規化」が意味すること
///
/// **同じものを 2 箇所に持たない。** チャンネルはギルドの中の配列ではなく
/// 識別子で引ける表にあり、ギルドは所属するチャンネルの識別子だけを持つ。
///
/// ```text
///   guilds:         GuildId   → 名前・アイコン
///   guild_channels: GuildId   → [ChannelId]      並び順つき
///   channels:       ChannelId → 種別・名前・話題
///   messages:       ChannelId → [Message]        古い順
/// ```
///
/// ⚠️ **これをしないと同じチャンネルが 2 つの形で存在する。** READY が
/// 持ってきたものと GUILD_UPDATE が持ってきたものが食い違ったとき、
/// どちらが正しいかを決める場所が無くなる。
///
/// # 画面より先にここが埋まる (`NFR-011`)
///
/// 起動時、Gateway に繋ぐより先に [`Db`] から読み込む。S4 の実測では
/// READY まで 672〜1120 ms かかり、**`NFR-001` (500 ms) に入らない**。
/// キャッシュから先に描いて、READY は後から差し替える。
#[derive(Debug, Default)]
pub struct Store {
    guilds: HashMap<GuildId, GuildRow>,
    /// ギルドごとのチャンネルの識別子。**並び順つき**
    guild_channels: HashMap<GuildId, Vec<ChannelId>>,
    channels: HashMap<ChannelId, Channel>,
    /// チャンネルごとのメッセージ。**古い順**
    messages: HashMap<ChannelId, Vec<Message>>,
    /// 画面に出す順。**毎フレーム並べ替えないため、ここに持つ**
    order: Vec<GuildId>,
    /// 利用者が Discord で並べた順 (`user_settings_proto` 由来)
    preferred: Vec<GuildId>,
    /// 届いた順。**並び順が分からないものはこの順で後ろに続く**
    arrival: Vec<GuildId>,
    /// サーバ一覧の並び。**フォルダも単独のサーバも同じ列に入る**
    sidebar: Vec<FolderRow>,
    /// 閉じているフォルダ。**残す** — 開き直すのは利用者の仕事ではない
    collapsed: std::collections::HashSet<u64>,
    /// ギルドごとの役職。**メンバー一覧の見出しを名前にするために要る**
    roles: HashMap<GuildId, Vec<Role>>,
    /// どこまで読んだか (`FR-042`)。**チャンネルごと**
    read: HashMap<ChannelId, ReadMark>,
}

/// 1 チャンネルぶんの「どこまで読んだか」。
///
/// # ⚠️ 未読は差ではなく大小で決まる
///
/// スノーフレークは時刻を含むので、**「読んだ印より大きい発言があるか」**
/// を見れば未読かどうかが決まる。件数は数えない — 数えるには全部の発言を
/// 持っている必要があり、開いていないチャンネルの分は持っていない。
///
/// 名指しの数だけはサーバが数えて寄越すので、そのまま持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadMark {
    /// ここまで読んだ。`None` は**一度も読んでいない**
    pub seen: Option<MessageId>,
    /// 自分宛ての未読の数
    pub mentions: u32,
}

/// サーバ一覧の 1 行。**フォルダか、サーバか。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildEntry<'a> {
    /// フォルダの見出し。**押すと開閉する**
    Folder { id: u64, row: &'a FolderRow },
    Guild {
        row: &'a GuildRow,
        /// どのフォルダの中にいるか。外にいれば `None`
        folder: Option<u64>,
    },
}

/// サーバ一覧のフォルダ 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FolderRow {
    /// ⚠️ **無ければフォルダではない。** フォルダに入れていないサーバも
    /// 「識別子の無い入れ物」として同じ列に入る
    pub id: Option<u64>,
    /// 付けていなければ `None`。**画面が中身の名前を並べて出す**
    pub name: Option<String>,
    pub guilds: Vec<GuildId>,
    /// 利用者が付けた色 (`0xRRGGBB`)。**付けていなければ `None`**
    #[serde(default)]
    pub color: Option<u32>,
}

/// チャンネル一覧の 1 行。**見出しか、開けるものか。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEntry<'a> {
    /// カテゴリの見出し。**押しても開かない**
    Category(&'a Channel),
    Channel(&'a Channel),
}

/// ギルドのうち、チャンネル以外の部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildRow {
    pub id: GuildId,
    pub name: String,
    /// アイコンの印。**URL ではない** (`Store::guild_icon`)
    pub icon_hash: Option<String>,
}

impl Store {
    pub fn new() -> Self {
        Store::default()
    }

    /// 画面に出す順のギルド。**利用者が並べた順が先、残りは届いた順**
    pub fn guilds(&self) -> impl Iterator<Item = &GuildRow> {
        self.order.iter().filter_map(|id| self.guilds.get(id))
    }

    /// サーバ一覧に出す順の、フォルダとサーバ。
    ///
    /// ```text
    ///   サーバ            フォルダの外。**位置はここ**
    ///   [フォルダ]        押すと開閉する
    ///     サーバ          開いていれば
    ///   サーバ
    /// ```
    ///
    /// ⚠️ **フォルダに入れていないサーバを末尾にまとめてはいけない。**
    /// Discord は並び順の一覧に、フォルダも単独のサーバも**同じ列**として
    /// 入れてくる。フォルダだけを抜き出して先に出すと、**利用者が並べた
    /// 位置が失われる。** 実際にそうなった。
    ///
    /// ⚠️ **閉じているフォルダの中身は出さない。** 出すと折り畳む意味がない。
    pub fn guild_entries(&self) -> Vec<GuildEntry<'_>> {
        // 並び順を知らないなら、届いた順のまま平らに出す
        if self.sidebar.is_empty() {
            return self
                .guilds()
                .map(|row| GuildEntry::Guild { row, folder: None })
                .collect();
        }

        let mut out = Vec::new();
        let mut placed = std::collections::HashSet::new();

        for row in &self.sidebar {
            match row.id {
                // 本当のフォルダ
                Some(id) => {
                    out.push(GuildEntry::Folder { id, row });
                    placed.extend(row.guilds.iter().copied());
                    if self.collapsed.contains(&id) {
                        continue;
                    }
                    out.extend(row.guilds.iter().filter_map(|g| {
                        self.guilds.get(g).map(|row| GuildEntry::Guild {
                            row,
                            folder: Some(id),
                        })
                    }));
                }
                // フォルダではない。**中身をそのまま並びへ置く**
                None => {
                    placed.extend(row.guilds.iter().copied());
                    out.extend(row.guilds.iter().filter_map(|g| {
                        self.guilds
                            .get(g)
                            .map(|row| GuildEntry::Guild { row, folder: None })
                    }));
                }
            }
        }

        // 並び順に載っていないもの。**新しく入ったサーバがここに来る**
        out.extend(
            self.guilds()
                .filter(|row| !placed.contains(&row.id))
                .map(|row| GuildEntry::Guild { row, folder: None }),
        );
        out
    }

    /// サーバ一覧の並びを教える。
    ///
    /// ⚠️ **フォルダだけを渡さないこと。** フォルダに入れていないサーバも
    /// 「識別子の無い入れ物」として、順のまま渡す
    pub fn set_sidebar(&mut self, rows: Vec<FolderRow>) {
        self.sidebar = rows;
    }

    pub fn sidebar(&self) -> &[FolderRow] {
        &self.sidebar
    }

    /// 閉じているフォルダ。**保存して次の起動で戻す**
    pub fn collapsed(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.collapsed.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn is_collapsed(&self, id: u64) -> bool {
        self.collapsed.contains(&id)
    }

    pub fn set_collapsed(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.collapsed = ids.into_iter().collect();
    }

    /// 開閉を切り替える。**開いたか閉じたかを返す**
    pub fn toggle_folder(&mut self, id: u64) -> bool {
        if !self.collapsed.insert(id) {
            self.collapsed.remove(&id);
            return true;
        }
        false
    }

    /// そのサーバのアイコン。**設定していなければ `None`**
    ///
    /// ⚠️ **サーバには既定のアイコンが無い。** 人と違って Discord は絵を
    /// 配っていないので、無いときは頭文字を出すしかない
    pub fn guild_icon(&self, id: GuildId) -> Option<Asset> {
        let hash = self.guilds.get(&id)?.icon_hash.as_ref()?;
        Some(Asset::guild_icon(id, hash))
    }

    pub fn guild(&self, id: GuildId) -> Option<&GuildRow> {
        self.guilds.get(&id)
    }

    /// その役職の名前。**知らなければ `None`**。
    ///
    /// ⚠️ **識別子を名前の代わりに出さない。** メンバー一覧の見出しに
    /// 18 桁の数字が並んでも、利用者にできることは何も増えない
    pub fn role_name(&self, guild: GuildId, role: RoleId) -> Option<&str> {
        self.roles
            .get(&guild)?
            .iter()
            .find(|r| r.id == role)
            .map(|r| &*r.name)
    }

    /// その人の名前に出す色 (`0xRRGGBB`)。**無ければ `None`**。
    ///
    /// ⚠️ **一番上の「色を付けている」役職が勝つ。** 一番上の役職ではない。
    /// 色を付けていない役職が上にあっても、名前の色は下の色付き役職から
    /// 来る。Discord もそうしている。
    ///
    /// ⚠️ **知らない役職は飛ばす。** ギルドの中身がまだ届いていない
    /// ときに、順を勝手に決めない
    pub fn member_tint(&self, guild: GuildId, roles: &[RoleId]) -> Option<u32> {
        let table = self.roles.get(&guild)?;
        table
            .iter()
            .filter(|r| roles.contains(&r.id))
            .filter_map(|r| Some((r.position, r.tint()?)))
            .max_by_key(|(position, _)| *position)
            .map(|(_, tint)| tint)
    }

    // ─────────────────────────────────────────── 未読 (`FR-042`)

    /// 読んだ印を置く。READY から来たものをそのまま入れる。
    ///
    /// ⚠️ **知らないチャンネルの分も入れる。** `read_state` には
    /// ギルドのイベントなど、チャンネルでないものの印も混ざっている。
    /// 選り分けずに持っておいて、引くときにチャンネルとして扱う
    pub fn set_read_marks(&mut self, marks: impl IntoIterator<Item = (ChannelId, ReadMark)>) {
        self.read = marks.into_iter().collect();
    }

    /// そのチャンネルに、まだ読んでいない発言があるか。
    ///
    /// ⚠️ **一度も読んでいないチャンネルを未読にしない。**
    ///
    /// 入ったばかりのサーバは、全チャンネルが「読んだ印なし」で来る。
    /// それを未読にすると**入った瞬間に全部が光る**。Discord は入った
    /// 時刻より前の発言を未読として扱わないので、こちらも印が無ければ
    /// 未読としない。
    ///
    /// ⚠️ **ミュートを見ていない。** 通知設定 (`FR-041`, M2) を読んで
    /// いないので、**黙らせたチャンネルも光る**
    pub fn is_unread(&self, channel: ChannelId) -> bool {
        let Some(last) = self.channels.get(&channel).and_then(|c| c.last_message_id) else {
            return false;
        };
        match self.read.get(&channel).and_then(|m| m.seen) {
            Some(seen) => last > seen,
            None => false,
        }
    }

    /// そのチャンネルの、自分宛ての未読の数
    pub fn mentions(&self, channel: ChannelId) -> u32 {
        self.read.get(&channel).map_or(0, |m| m.mentions)
    }

    /// そのサーバに未読があるか、名指しが何件あるか。
    ///
    /// **中のチャンネルを畳んだもの**である。サーバ一覧の印はこれで決まる
    pub fn guild_unread(&self, guild: GuildId) -> (bool, u32) {
        let mut unread = false;
        let mut mentions = 0;
        for c in self.channels_of(guild) {
            unread |= self.is_unread(c.id);
            mentions += self.mentions(c.id);
        }
        (unread, mentions)
    }

    /// そこまで読んだことにする。**変わったら真**。
    ///
    /// ⚠️ **サーバへ伝えるのは呼び出し側の仕事である。** ここは手元の
    /// 見た目を先に直すだけで、往復を待たない
    pub fn mark_read(&mut self, channel: ChannelId) -> bool {
        let last = self.channels.get(&channel).and_then(|c| c.last_message_id);
        let mark = self.read.entry(channel).or_default();
        // ⚠️ **一度も読んでいないチャンネルにも印を置く。** 置かないと
        // 開いても未読のままになる
        let changed = mark.seen != last || mark.mentions != 0;
        mark.seen = last;
        mark.mentions = 0;
        changed
    }

    /// 新しい発言が来た。**チャンネルの一番新しい発言を進める**。
    ///
    /// `me` は自分。自分宛てなら名指しの数を増やす。
    /// **変わったら真** (画面を描き直すかの判断に使う)
    pub fn note_arrival(&mut self, message: &Message, me: Option<UserId>) -> bool {
        let Some(channel) = self.channels.get_mut(&message.channel_id) else {
            return false;
        };
        // ⚠️ **戻さない。** 遅れて届いた古いものが混ざることがある
        if channel
            .last_message_id
            .is_some_and(|last| last >= message.id)
        {
            return false;
        }
        channel.last_message_id = Some(message.id);

        if me.is_some_and(|me| message.mentions_me(me)) {
            self.read.entry(message.channel_id).or_default().mentions += 1;
        }
        true
    }

    /// そのフォルダに利用者が付けた色。**無ければ `None`**
    pub fn folder_tint(&self, folder: u64) -> Option<u32> {
        self.sidebar
            .iter()
            .find(|f| f.id == Some(folder))
            .and_then(|f| f.color)
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.get(&id)
    }

    /// そのギルドの、**開けるチャンネルだけ**。並び順つき。
    ///
    /// カテゴリは含まない。選択の対象になるものだけが要るときに使う
    pub fn channels_of(&self, guild: GuildId) -> impl Iterator<Item = &Channel> {
        self.entries_of(guild).filter_map(|e| match e {
            ChannelEntry::Channel(c) => Some(c),
            ChannelEntry::Category(_) => None,
        })
    }

    /// 一覧に出す順の、カテゴリとチャンネル。
    ///
    /// # 並び順は Discord の規則に従う
    ///
    /// ```text
    ///   カテゴリに属さないチャンネル   position 順
    ///   カテゴリ A                      position 順
    ///     └ その中のチャンネル          position 順
    ///   カテゴリ B
    ///     └ …
    /// ```
    ///
    /// ⚠️ **position は重複する。** 同じ値なら識別子順 — つまり作られた順に
    /// 倒す。ここが崩れると並びが毎フレーム変わって読めない
    pub fn entries_of(&self, guild: GuildId) -> impl Iterator<Item = ChannelEntry<'_>> {
        let ids = self.guild_channels.get(&guild).cloned().unwrap_or_default();
        let all: Vec<&Channel> = ids.iter().filter_map(|id| self.channels.get(id)).collect();

        fn sorted(mut v: Vec<&Channel>) -> Vec<&Channel> {
            v.sort_by_key(|c| (c.position, c.id.get()));
            v
        }

        let mut out: Vec<ChannelEntry<'_>> = Vec::with_capacity(all.len());

        // [1] カテゴリの外にあるもの
        out.extend(
            sorted(
                all.iter()
                    .copied()
                    .filter(|c| !c.kind.is_category() && c.parent_id.is_none())
                    .collect(),
            )
            .into_iter()
            .map(ChannelEntry::Channel),
        );

        // [2] カテゴリと、その中身
        for cat in sorted(
            all.iter()
                .copied()
                .filter(|c| c.kind.is_category())
                .collect(),
        ) {
            let children = sorted(
                all.iter()
                    .copied()
                    .filter(|c| !c.kind.is_category() && c.parent_id == Some(cat.id))
                    .collect(),
            );
            // ⚠️ **空のカテゴリも出す。** Discord がそうしている。
            // 見えているのに一覧に無いと、設定を間違えたのかと思わせる
            out.push(ChannelEntry::Category(cat));
            out.extend(children.into_iter().map(ChannelEntry::Channel));
        }
        out.into_iter()
    }

    /// そのチャンネルのメッセージ。**まだ読んでいなければ空**
    pub fn messages(&self, channel: ChannelId) -> &[Message] {
        self.messages.get(&channel).map_or(&[], Vec::as_slice)
    }

    /// 中身を持っているか。**「空」と「まだ読んでいない」の区別である**
    pub fn has_messages(&self, channel: ChannelId) -> bool {
        self.messages.contains_key(&channel)
    }

    pub fn is_empty(&self) -> bool {
        self.guilds.is_empty()
    }

    /// ギルドを丸ごと入れ替える。READY と、キャッシュからの読み込みで使う。
    pub fn replace_guilds(&mut self, guilds: Vec<Guild>) {
        self.guilds.clear();
        self.guild_channels.clear();
        self.channels.clear();
        self.arrival.clear();
        for g in guilds {
            self.upsert_guild(g);
        }
    }

    /// ギルドを 1 つ入れる・更新する。
    ///
    /// ⚠️ **殻だけのギルドは採らない。** 名前もチャンネルも無いので、
    /// 一覧に中身の無い丸が並ぶだけになる。GUILD_CREATE で届いたら入る
    pub fn upsert_guild(&mut self, guild: Guild) {
        if guild.unavailable || guild.name.is_empty() {
            return;
        }
        let id = guild.id;

        // ⚠️ **並び順は position、同じなら識別子順。** position が同じ
        // チャンネルは実在するので、そこで崩れると並びが毎フレーム変わる
        let channels: Vec<Channel> = guild
            .channels
            .into_iter()
            // ⚠️ **カテゴリも保つ。** 見出しに要る。並べ替えは entries_of が行う
            .filter(|c| c.kind.is_text() || c.kind.is_category())
            .collect();

        // ⚠️ 更新のときにチャンネルが空なら、**前のものを残す**。
        // GUILD_UPDATE は名前だけを持ってくることがある
        if !channels.is_empty() || !self.guild_channels.contains_key(&id) {
            let ids: Vec<ChannelId> = channels.iter().map(|c| c.id).collect();
            for c in channels {
                self.channels.insert(c.id, c);
            }
            self.guild_channels.insert(id, ids);
        }

        // ⚠️ **空の役職で上書きしない。** `GUILD_UPDATE` は名前だけを
        // 持ってくることがあり、そのたびにメンバー一覧の見出しが
        // 識別子に戻ってしまう
        if !guild.roles.is_empty() {
            // ⚠️ **数だけを出す。** 識別子も名前も、どのサーバに入って
            // いるかを語ってしまう
            tracing::debug!(
                roles = guild.roles.len(),
                colored = guild.roles.iter().filter(|r| r.tint().is_some()).count(),
                "役職を受け取った"
            );
            self.roles.insert(id, guild.roles);
        }

        // 届いた順を覚える。**並び順が分からないものはこの順に落とす**
        if !self.arrival.contains(&id) {
            self.arrival.push(id);
        }
        self.guilds.insert(
            id,
            GuildRow {
                id,
                name: guild.name,
                icon_hash: guild.icon_hash,
            },
        );
        self.resort();
    }

    /// 過去分を丸ごと置く。**REST から取ってきたもので上書きする**
    pub fn set_backlog(&mut self, channel: ChannelId, list: Vec<Message>) {
        self.messages.insert(channel, list);
    }

    /// 一番古いもの。**続きを頼む起点である**
    pub fn oldest_message(&self, channel: ChannelId) -> Option<MessageId> {
        self.messages.get(&channel)?.first().map(|m| m.id)
    }

    /// 古いほうを前へ継ぎ足す。**足した件数を返す。**
    ///
    /// `list` は**古い順**で渡すこと。Discord は新しい順で返すので、
    /// 反転するのは呼び出し側の仕事である ([`gumicord_rest`] と同じ約束)。
    ///
    /// ⚠️ **既にあるものは足さない。** 遡っている最中に同じ範囲を二度
    /// 頼むことはあり、そのとき重なった分をそのまま入れると**同じ行が
    /// 二度出る**。
    ///
    /// ⚠️ **まだ開いていないチャンネルには足さない。** 途中だけある
    /// 歯抜けの履歴になり、間に何があったのかを言えなくなる
    pub fn prepend_messages(&mut self, channel: ChannelId, list: Vec<Message>) -> usize {
        let Some(existing) = self.messages.get_mut(&channel) else {
            return 0;
        };
        let known: HashSet<MessageId> = existing.iter().map(|m| m.id).collect();
        let mut older: Vec<Message> = list
            .into_iter()
            .filter(|m| !known.contains(&m.id))
            .collect();
        if older.is_empty() {
            return 0;
        }
        let added = older.len();
        older.append(existing);
        *existing = older;
        added
    }

    /// 新着を末尾に足す。**足したら真。**
    ///
    /// ⚠️ 開いていないチャンネルの分は溜めない。開いたときに取り直すので、
    /// ここで持つと二重になる。
    ///
    /// ⚠️ 同じものを二度足さない。送った直後は REST の応答と
    /// `MESSAGE_CREATE` が競る
    pub fn push_message(&mut self, message: Message) -> bool {
        let Some(list) = self.messages.get_mut(&message.channel_id) else {
            return false;
        };
        if list.iter().any(|m| m.id == message.id) {
            return false;
        }
        list.push(message);
        true
    }

    /// 利用者が Discord で並べ替えた順を教える。
    ///
    /// ⚠️ **名前順ではない。** 自分で並べた順以外で出すと、
    /// 「自分のサーバ一覧ではない」ものになる。
    ///
    /// ここに無いギルドは、届いた順で後ろに続く
    pub fn set_preferred_order(&mut self, order: Vec<GuildId>) {
        self.preferred = order;
        self.resort();
    }

    /// いまの並び順。**そのまま保存して次の起動で戻せる**
    pub fn order(&self) -> &[GuildId] {
        &self.order
    }

    /// 並べ直す。
    ///
    /// **利用者が並べた順が先、残りは届いた順。** どちらにも名前は使わない
    fn resort(&mut self) {
        let rank: HashMap<GuildId, usize> = self
            .preferred
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();

        let mut ids: Vec<GuildId> = self.arrival.to_vec();
        ids.retain(|id| self.guilds.contains_key(id));

        // ⚠️ **安定な並べ替えを使う。** 順が決まっていないもの同士は
        // 届いた順のままでいてほしい
        ids.sort_by_key(|id| rank.get(id).copied().unwrap_or(usize::MAX));
        self.order = ids;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_model::{ChannelKind, MessageId, User, UserId};

    fn guild(id: u64, name: &str, channels: &[(u64, &str, i32)]) -> Guild {
        Guild {
            id: id.into(),
            name: name.to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: channels
                .iter()
                .map(|(cid, cname, pos)| Channel {
                    id: (*cid).into(),
                    kind: ChannelKind::GuildText,
                    name: Some((*cname).to_owned()),
                    guild_id: Some(id.into()),
                    parent_id: None,
                    position: *pos,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                })
                .collect(),
            roles: Vec::new(),
        }
    }

    fn message(id: u64, channel: u64) -> Message {
        Message {
            id: MessageId::from(id),
            channel_id: ChannelId::from(channel),
            guild_id: None,
            author: User {
                id: UserId::from(1u64),
                username: "ねんねこ".to_owned(),
                global_name: None,
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: format!("その {id}"),
            timestamp: "2026-08-22T00:00:00+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: None,
            referenced_message: None,
            mentions: Vec::new(),
            mention_everyone: false,
        }
    }

    /// ⚠️ **ギルドは名前順ではない。** 届いた順のまま出す。
    /// チャンネルは position 順
    #[test]
    fn guilds_keep_their_arrival_order_and_channels_sort_by_position() {
        let mut s = Store::new();
        s.replace_guilds(vec![
            guild(2, "ばなな", &[(20, "ろ", 1), (21, "い", 0)]),
            guild(1, "あんず", &[]),
        ]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["ばなな", "あんず"], "勝手に名前で並べている");

        let chans: Vec<_> = s
            .channels_of(GuildId::from(2u64))
            .map(|c| c.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(chans, vec!["い", "ろ"], "position 順になっていない");
    }

    /// 利用者が並べた順が勝つ。**ここに無いものは届いた順で後ろに続く**
    #[test]
    fn the_users_own_order_wins() {
        let mut s = Store::new();
        s.replace_guilds(vec![
            guild(1, "いち", &[]),
            guild(2, "に", &[]),
            guild(3, "さん", &[]),
        ]);

        // 3 と 1 だけを並べた。2 は指定が無い
        s.set_preferred_order(vec![GuildId::from(3u64), GuildId::from(1u64)]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["さん", "いち", "に"]);
    }

    /// 並び順に、もう居ないギルドが混ざっていても壊れない。
    /// **保存した順を次の起動で使うので、抜けたサーバが残りうる**
    #[test]
    fn a_stale_order_does_not_resurrect_guilds() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "いち", &[])]);
        s.set_preferred_order(vec![GuildId::from(9u64), GuildId::from(1u64)]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["いち"]);
        assert_eq!(s.order().len(), 1);
    }

    /// カテゴリは見出しとして出て、その中身が続く
    #[test]
    fn categories_come_out_as_headings_with_their_channels() {
        use gumicord_model::ChannelKind;

        let mut g = guild(1, "テスト", &[(10, "そとがわ", 0)]);
        g.channels.push(Channel {
            id: 20u64.into(),
            kind: ChannelKind::GuildCategory,
            name: Some("カテゴリ".to_owned()),
            guild_id: Some(1u64.into()),
            parent_id: None,
            position: 1,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: None,
        });
        g.channels.push(Channel {
            id: 21u64.into(),
            kind: ChannelKind::GuildText,
            name: Some("なかみ".to_owned()),
            guild_id: Some(1u64.into()),
            parent_id: Some(20u64.into()),
            position: 0,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: None,
        });

        let mut s = Store::new();
        s.replace_guilds(vec![g]);

        let got: Vec<String> = s
            .entries_of(GuildId::from(1u64))
            .map(|e| match e {
                ChannelEntry::Category(c) => format!("[{}]", c.display_name()),
                ChannelEntry::Channel(c) => c.display_name(),
            })
            .collect();
        assert_eq!(got, vec!["そとがわ", "[カテゴリ]", "なかみ"]);

        // 見出しは開けるものではない
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 2);
    }

    /// ⚠️ **空のカテゴリも出す。** Discord がそうしている。
    /// 見えているのに一覧に無いと、設定を間違えたのかと思わせる
    #[test]
    fn an_empty_category_is_still_shown() {
        use gumicord_model::ChannelKind;

        let mut g = guild(1, "テスト", &[]);
        g.channels.push(Channel {
            id: 20u64.into(),
            kind: ChannelKind::GuildCategory,
            name: Some("からっぽ".to_owned()),
            guild_id: Some(1u64.into()),
            parent_id: None,
            position: 0,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: None,
        });

        let mut s = Store::new();
        s.replace_guilds(vec![g]);

        let got: Vec<_> = s.entries_of(GuildId::from(1u64)).collect();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], ChannelEntry::Category(_)));
        // **見出しは開けるものではない**
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 0);
    }

    /// **チャンネルは 1 箇所にしかない。** 同じものが 2 つの形で存在すると、
    /// 食い違ったときにどちらが正しいか決められなくなる
    #[test]
    fn a_channel_lives_in_exactly_one_place() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "あ", &[(10, "い", 0)])]);

        let c = s.channel(ChannelId::from(10u64)).expect("引けない");
        assert_eq!(c.name.as_deref(), Some("い"));
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 1);
    }

    /// GUILD_UPDATE がチャンネルを持ってこなくても、前のものを失わない
    #[test]
    fn an_update_without_channels_keeps_the_old_ones() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "まえ", &[(10, "い", 0)])]);
        s.upsert_guild(guild(1, "あと", &[]));

        assert_eq!(s.guild(GuildId::from(1u64)).unwrap().name, "あと");
        assert_eq!(
            s.channels_of(GuildId::from(1u64)).count(),
            1,
            "チャンネルが消えた"
        );
    }

    /// 殻だけのギルドは採らない
    #[test]
    fn a_shell_guild_is_not_shown() {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: String::new(),
            icon_hash: None,
            unavailable: true,
            channels: Vec::new(),
            roles: Vec::new(),
        });
        assert_eq!(s.guilds().count(), 0);
    }

    /// 「空」と「まだ読んでいない」は別のものである
    #[test]
    fn empty_and_unread_are_different() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        assert!(!s.has_messages(ch));

        s.set_backlog(ch, Vec::new());
        assert!(s.has_messages(ch), "取り終えたことが分からない");
        assert!(s.messages(ch).is_empty());
    }

    /// 開いていないチャンネルの新着は溜めない
    #[test]
    fn a_message_for_an_unopened_channel_is_dropped() {
        let mut s = Store::new();
        assert!(!s.push_message(message(1, 99)));
        assert!(s.messages(ChannelId::from(99u64)).is_empty());
    }

    /// 同じものを二度足さない
    #[test]
    fn the_same_message_is_not_added_twice() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, Vec::new());

        assert!(s.push_message(message(1, 10)));
        assert!(!s.push_message(message(1, 10)));
        assert_eq!(s.messages(ch).len(), 1);
    }

    /// 遡った分は**前へ**付く。順は古いままである
    #[test]
    fn older_messages_go_in_front() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, vec![message(5, 10), message(6, 10)]);

        assert_eq!(s.oldest_message(ch), Some(MessageId::from(5u64)));
        assert_eq!(
            s.prepend_messages(ch, vec![message(3, 10), message(4, 10)]),
            2
        );

        let ids: Vec<u64> = s.messages(ch).iter().map(|m| m.id.get()).collect();
        assert_eq!(ids, vec![3, 4, 5, 6]);
        assert_eq!(s.oldest_message(ch), Some(MessageId::from(3u64)));
    }

    /// ⚠️ **重なった分は捨てる。** 同じ範囲を二度頼むことはあり、
    /// そのまま入れると同じ行が二度出る
    #[test]
    fn an_overlapping_page_does_not_duplicate_rows() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, vec![message(4, 10), message(5, 10)]);

        assert_eq!(
            s.prepend_messages(ch, vec![message(3, 10), message(4, 10)]),
            1
        );
        let ids: Vec<u64> = s.messages(ch).iter().map(|m| m.id.get()).collect();
        assert_eq!(ids, vec![3, 4, 5]);
    }

    /// ⚠️ **開いていないチャンネルには足さない。** 歯抜けの履歴になる
    #[test]
    fn a_channel_that_was_never_opened_gets_nothing() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        assert_eq!(s.prepend_messages(ch, vec![message(1, 10)]), 0);
        assert!(!s.has_messages(ch));
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    fn store() -> Store {
        let mut s = Store::new();
        s.replace_guilds(vec![
            Guild {
                id: 1u64.into(),
                name: "いち".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: Vec::new(),
                roles: Vec::new(),
            },
            Guild {
                id: 2u64.into(),
                name: "に".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: Vec::new(),
                roles: Vec::new(),
            },
            Guild {
                id: 3u64.into(),
                name: "さん".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: Vec::new(),
                roles: Vec::new(),
            },
        ]);
        s
    }

    fn folder(id: u64, guilds: &[u64]) -> FolderRow {
        FolderRow {
            id: Some(id),
            name: Some(format!("フォルダ{id}")),
            color: None,
            guilds: guilds.iter().map(|g| GuildId::from(*g)).collect(),
        }
    }

    /// フォルダに入れていないサーバ。**同じ列に入る**
    fn bare(guild: u64) -> FolderRow {
        FolderRow {
            id: None,
            name: None,
            color: None,
            guilds: vec![GuildId::from(guild)],
        }
    }

    fn shape(s: &Store) -> Vec<String> {
        s.guild_entries()
            .into_iter()
            .map(|e| match e {
                GuildEntry::Folder { row, .. } => {
                    format!("[{}]", row.name.as_deref().unwrap_or(""))
                }
                GuildEntry::Guild {
                    row,
                    folder: Some(_),
                } => format!("  {}", row.name),
                GuildEntry::Guild { row, folder: None } => row.name.clone(),
            })
            .collect()
    }

    /// 並びを知らないなら、届いた順のただの列である
    #[test]
    fn without_a_sidebar_it_is_a_flat_list() {
        let s = store();
        assert_eq!(shape(&s), vec!["いち", "に", "さん"]);
    }

    /// ⚠️ **フォルダの位置が保たれる。**
    ///
    /// 単独のサーバを末尾へ寄せていたせいで、利用者が並べた位置が
    /// 失われていた
    #[test]
    fn a_folder_keeps_its_place_in_the_list() {
        let mut s = store();
        // 「いち」→ フォルダ →「さん」の順に並べてある
        s.set_sidebar(vec![bare(1), folder(10, &[2]), bare(3)]);

        assert_eq!(shape(&s), vec!["いち", "[フォルダ10]", "  に", "さん"]);
    }

    /// ⚠️ **閉じたら中身は出さない。** 出すと折り畳む意味がない
    #[test]
    fn a_collapsed_folder_hides_its_guilds() {
        let mut s = store();
        s.set_sidebar(vec![bare(1), folder(10, &[2, 3])]);

        assert!(!s.toggle_folder(10), "閉じたはず");
        assert_eq!(shape(&s), vec!["いち", "[フォルダ10]"]);

        assert!(s.toggle_folder(10), "開いたはず");
        assert_eq!(shape(&s).len(), 4);
    }

    /// 抜けたサーバが並びに残っていても、一覧には出さない
    #[test]
    fn a_sidebar_referring_to_a_missing_guild_skips_it() {
        let mut s = store();
        s.set_sidebar(vec![folder(10, &[2, 999]), bare(1), bare(3)]);
        assert_eq!(shape(&s), vec!["[フォルダ10]", "  に", "いち", "さん"]);
    }

    /// **新しく入ったサーバは末尾に出る。** 並びにはまだ載っていない
    #[test]
    fn a_guild_missing_from_the_sidebar_still_appears() {
        let mut s = store();
        s.set_sidebar(vec![bare(1), bare(2)]);
        assert_eq!(shape(&s), vec!["いち", "に", "さん"]);
    }

    /// 閉じている印は**保存して戻せる**
    #[test]
    fn the_collapsed_set_round_trips() {
        let mut s = store();
        s.set_sidebar(vec![folder(10, &[2]), folder(20, &[3]), bare(1)]);
        s.toggle_folder(20);

        let saved = s.collapsed();
        assert_eq!(saved, vec![20]);

        let mut again = store();
        again.set_sidebar(vec![folder(10, &[2]), folder(20, &[3]), bare(1)]);
        again.set_collapsed(saved);
        assert_eq!(shape(&again), shape(&s));
    }
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    fn role(id: u64, position: i64, color: u32) -> Role {
        Role {
            id: RoleId::from(id),
            name: format!("役職{id}"),
            position,
            hoist: false,
            color: Some(color),
        }
    }

    fn store(roles: Vec<Role>) -> Store {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles,
        });
        s
    }

    /// ⚠️ **一番上の「色を付けている」役職が勝つ。**
    ///
    /// 色を付けていない役職が上にあっても、名前の色は下の色付き役職から
    /// 来る。Discord もそうしている
    #[test]
    fn the_highest_coloured_role_wins_not_the_highest_role() {
        let s = store(vec![
            role(10, 1, 0x0000_ff00),
            // 上にあるが色を付けていない
            role(20, 5, 0),
        ]);
        let mine = [RoleId::from(10u64), RoleId::from(20u64)];
        assert_eq!(s.member_tint(1u64.into(), &mine), Some(0x0000_ff00));
    }

    /// ⚠️ **0 は黒ではない。** 黒く塗ると全員の名前が読めなくなる
    #[test]
    fn a_role_without_a_colour_gives_nothing() {
        let s = store(vec![role(10, 1, 0)]);
        assert_eq!(s.member_tint(1u64.into(), &[RoleId::from(10u64)]), None);
    }

    /// ⚠️ **知らない役職は飛ばす。** ギルドがまだ届いていないときに
    /// 順を勝手に決めない
    #[test]
    fn an_unknown_role_is_skipped() {
        let s = store(vec![role(10, 1, 0x0000_ff00)]);
        assert_eq!(s.member_tint(1u64.into(), &[RoleId::from(99u64)]), None);
        assert_eq!(s.member_tint(2u64.into(), &[RoleId::from(10u64)]), None);
    }

    /// ⚠️ **空の役職で上書きしない。** `GUILD_UPDATE` は名前だけを持って
    /// くることがあり、そのたびに色と見出しが消えてしまう
    #[test]
    fn an_update_without_roles_keeps_the_old_ones() {
        let mut s = store(vec![role(10, 1, 0x0000_ff00)]);
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: "名前だけ変えた".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: Vec::new(),
        });
        assert_eq!(
            s.member_tint(1u64.into(), &[RoleId::from(10u64)]),
            Some(0x0000_ff00)
        );
    }

    /// フォルダの色は識別子で引ける
    #[test]
    fn a_folder_carries_its_colour() {
        let mut s = Store::new();
        s.set_sidebar(vec![
            FolderRow {
                id: Some(100),
                name: None,
                guilds: Vec::new(),
                color: Some(0x007c_6cf0),
            },
            FolderRow {
                id: Some(200),
                name: None,
                guilds: Vec::new(),
                color: None,
            },
        ]);
        assert_eq!(s.folder_tint(100), Some(0x007c_6cf0));
        assert_eq!(s.folder_tint(200), None);
        assert_eq!(s.folder_tint(999), None);
    }
}

#[cfg(test)]
mod unread_tests {
    use super::*;
    use gumicord_model::{ChannelKind, User};

    fn channel(id: u64, last: Option<u64>) -> Channel {
        Channel {
            id: ChannelId::from(id),
            kind: ChannelKind::GuildText,
            name: Some(format!("ch{id}")),
            guild_id: Some(GuildId::from(1u64)),
            parent_id: None,
            position: 0,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: last.map(MessageId::from),
        }
    }

    fn store(channels: Vec<Channel>) -> Store {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels,
            roles: Vec::new(),
        });
        s
    }

    fn user(id: u64) -> User {
        User {
            id: gumicord_model::UserId::from(id),
            username: format!("u{id}"),
            global_name: None,
            discriminator: "0".to_owned(),
            avatar_hash: None,
            bot: false,
        }
    }

    fn message(id: u64, channel: u64, from: u64, to: &[u64]) -> Message {
        Message {
            id: MessageId::from(id),
            channel_id: ChannelId::from(channel),
            guild_id: Some(GuildId::from(1u64)),
            author: user(from),
            content: String::new(),
            timestamp: String::new(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: None,
            referenced_message: None,
            mentions: to.iter().map(|u| user(*u)).collect(),
            mention_everyone: false,
        }
    }

    /// 読んだ印より新しい発言があれば未読
    #[test]
    fn newer_than_the_mark_is_unread() {
        let mut s = store(vec![channel(10, Some(100))]);
        let ch = ChannelId::from(10u64);

        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(99u64)),
                mentions: 0,
            },
        )]);
        assert!(s.is_unread(ch));

        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(100u64)),
                mentions: 0,
            },
        )]);
        assert!(!s.is_unread(ch), "同じところまで読んでいる");
    }

    /// ⚠️ **一度も読んでいないチャンネルを未読にしない。**
    ///
    /// 入ったばかりのサーバは全チャンネルが印なしで来る。未読にすると
    /// **入った瞬間に全部が光る**
    #[test]
    fn a_channel_we_never_read_is_not_unread() {
        let s = store(vec![channel(10, Some(100))]);
        assert!(!s.is_unread(ChannelId::from(10u64)));
    }

    /// サーバの印は**中のチャンネルを畳んだもの**である
    #[test]
    fn a_guild_folds_up_its_channels() {
        let mut s = store(vec![channel(10, Some(100)), channel(11, Some(200))]);
        s.set_read_marks([
            (
                ChannelId::from(10u64),
                ReadMark {
                    seen: Some(MessageId::from(100u64)),
                    mentions: 0,
                },
            ),
            (
                ChannelId::from(11u64),
                ReadMark {
                    seen: Some(MessageId::from(150u64)),
                    mentions: 3,
                },
            ),
        ]);

        assert_eq!(s.guild_unread(GuildId::from(1u64)), (true, 3));
    }

    /// 開いたら既読になる。**名指しの数も消える**
    #[test]
    fn opening_a_channel_clears_it() {
        let mut s = store(vec![channel(10, Some(100))]);
        let ch = ChannelId::from(10u64);
        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(50u64)),
                mentions: 2,
            },
        )]);

        assert!(s.mark_read(ch));
        assert!(!s.is_unread(ch));
        assert_eq!(s.mentions(ch), 0);
        assert!(!s.mark_read(ch), "2 度目は何も変わらない");
    }

    /// 新しい発言が来たらチャンネルの先端が進み、自分宛てなら数が増える
    #[test]
    fn an_arrival_moves_the_head_and_counts_mentions() {
        let mut s = store(vec![channel(10, Some(100))]);
        let ch = ChannelId::from(10u64);
        let me = gumicord_model::UserId::from(7u64);
        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(100u64)),
                mentions: 0,
            },
        )]);
        assert!(!s.is_unread(ch));

        assert!(s.note_arrival(&message(101, 10, 8, &[]), Some(me)));
        assert!(s.is_unread(ch));
        assert_eq!(s.mentions(ch), 0, "名指しではない");

        assert!(s.note_arrival(&message(102, 10, 8, &[7]), Some(me)));
        assert_eq!(s.mentions(ch), 1);
    }

    /// ⚠️ **自分の発言は自分宛てに数えない。**
    /// 返信に自分を含めることはよくある
    #[test]
    fn my_own_message_never_mentions_me() {
        let mut s = store(vec![channel(10, Some(100))]);
        let me = gumicord_model::UserId::from(7u64);

        s.note_arrival(&message(101, 10, 7, &[7]), Some(me));
        assert_eq!(s.mentions(ChannelId::from(10u64)), 0);
    }

    /// ⚠️ **先端を戻さない。** 遅れて届いた古いものが混ざることがある
    #[test]
    fn a_late_old_message_does_not_move_the_head_back() {
        let mut s = store(vec![channel(10, Some(100))]);
        assert!(!s.note_arrival(&message(50, 10, 8, &[]), None));
        assert!(!s.note_arrival(&message(100, 10, 8, &[]), None));
    }
}

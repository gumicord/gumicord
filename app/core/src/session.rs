//! ログインの状態と、それを進める背景の仕事 (`FR-001`)。
//!
//! # 二本ある経路のうち、ここは QR のほうである
//!
//! [ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md) が既定に
//! 選んだのは QR ログインである。**captcha が出ない**からで、hCaptcha を
//! 自前レンダラで描く方法がないという一点で決まった。
//!
//! パスワード経路 (C4b) は OS の WebView を借りて captcha を出す。
//! そちらが入っても、既定は QR のままである。
//!
//! # 主スレッドは止めない
//!
//! やりとりは丸ごと [`tokio`] の上で進み、結果だけがチャネルで戻る。
//! イベントループは寝ているので ([`ControlFlow::Wait`]) 、**知らせを入れた
//! 側が [`Waker`] で起こす**。
//!
//! ```text
//!   背景 (tokio)                       主スレッド (winit)
//!   ─────────────                      ──────────────────
//!   RemoteAuth::next()
//!        │
//!        ├── tx.send(LoginEvent) ──▶  (チャネルに溜まる)
//!        └── waker.wake()        ──▶  Application::wake()
//!                                          │
//!                                          ├── try_recv を空になるまで
//!                                          └── 再描画
//! ```
//!
//! ⚠️ **起こされる回数は約束されない。** 何度かの `wake` が 1 回にまとまる
//! ことがあるので、取り込みは必ず空になるまで回す。
//!
//! # トークンは OS の鍵束に預ける (`FR-003`)
//!
//! 起動したらまず保存されたトークンを試し、通れば QR を出さない。
//!
//! ```text
//!   起動
//!    ├─ 鍵束にトークンがある ──▶ GET /users/@me ──┬─ 通った ──▶ そのまま入る
//!    │                                            └─ 弾かれた ─▶ 捨てて ↓
//!    └─ 無い ─────────────────────────────────────────────────▶ QR を出す
//! ```
//!
//! ⚠️ **通らなかったトークンはその場で捨てる。** 残しておくと、次の起動でも
//! 同じ失敗を繰り返したうえで結局 QR を出すことになる。
//!
//! ⚠️ **暗号化できない環境では保存しない** ([`gumicord_platform::SecretStore`])。
//! 起動のたびに聞くほうが、平文をディスクに置くよりましである。

use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use gumicord_gateway::{RemoteAuth, RemoteAuthEvent, ScannedUser};
use gumicord_model::{CurrentUser, Token};
use gumicord_platform::{SecretStore, Waker};
use gumicord_rest::RestClient;

/// ログインを飛ばして画面だけ見るための環境変数。
///
/// レンダラやテーマを触るのに毎回スマホを出すのは馬鹿らしい。
/// **本物のデータは出ない。** [`crate::demo`] の固定データが出る
const SKIP_ENV: &str = "GUMICORD_SKIP_LOGIN";

/// 鍵束の中でトークンを指す名前 (`FR-003`)
const TOKEN_KEY: &str = "token";

/// いまどこまで進んでいるか。**画面はこれだけを見て決まる。**
#[derive(Debug, Clone)]
pub enum Session {
    /// 繋ぎに行っている。QR はまだ出せない
    Connecting,
    /// QR を出して、読まれるのを待っている
    WaitingForScan {
        /// QR に載せる URL
        url: String,
        /// 読んだ人。**読まれただけで、まだ承認されていない**
        scanned: Option<ScannedUser>,
    },
    /// 承認された。チケットをトークンへ換えている
    Exchanging,
    /// 入れた
    LoggedIn(Box<LoggedIn>),
    /// 失敗した。**やり直せる**ので、原因を出して待つ
    Failed(String),
}

/// ログインできた後に手元へ残るもの。
///
/// ⚠️ **トークンがここにある。** REST は [`RestClient`] が中に持っているが、
/// Gateway は identify に生のトークンが要るので、別に取り出せる形で置く。
/// [`Token`] は表示しても中身が出ない型である (`SEC-001`)
#[derive(Debug, Clone)]
pub struct LoggedIn {
    pub me: CurrentUser,
    pub client: RestClient,
    pub token: Token,
}

impl Session {
    /// 画面に出す一行。**状態そのものより、この文が利用者の見るものである**
    pub fn hint(&self) -> String {
        match self {
            Session::Connecting => "Discord に接続しています…".to_owned(),
            Session::WaitingForScan { scanned: None, .. } => {
                "スマホの Discord でカメラを開き、この QR を読み取ってください".to_owned()
            }
            Session::WaitingForScan {
                scanned: Some(u), ..
            } => format!("{} として続けますか？ スマホで承認してください", u.username),
            Session::Exchanging => "承認されました。ログインしています…".to_owned(),
            Session::LoggedIn(l) => format!("{} でログインしました", l.me.user.display_name()),
            Session::Failed(e) => format!("失敗しました: {e}"),
        }
    }

    /// QR に載せる URL。出せる状態でなければ `None`
    pub fn qr(&self) -> Option<&str> {
        match self {
            Session::WaitingForScan { url, .. } => Some(url),
            _ => None,
        }
    }

    pub fn logged_in(&self) -> Option<&LoggedIn> {
        match self {
            Session::LoggedIn(l) => Some(l),
            _ => None,
        }
    }
}

/// 背景から主スレッドへ流れる知らせ。
///
/// **状態そのものではなく出来事を送る。** 状態を送ると、遅れて着いた古い
/// 状態が新しい状態を上書きしうる
#[derive(Debug)]
pub enum LoginEvent {
    /// QR を出せるようになった
    Qr(String),
    Scanned(ScannedUser),
    /// 承認された。トークンを取りに行く
    Approved,
    Done(Box<LoggedIn>),
    Failed(String),
    /// 期限切れなどでやり直しになった。QR が出し直される
    Restarted,
}

/// ログインの進行役。**アプリはこれを持ち、[`Self::poll`] で取り込む。**
pub struct Login {
    session: Session,
    rx: Receiver<LoginEvent>,
    tx: Sender<LoginEvent>,
    /// ログインを飛ばして画面だけ見る。**起動時に一度決まり、途中で変わらない**
    skipped: bool,
}

impl Login {
    /// ⚠️ **環境変数をここで一度だけ読む。** 毎フレーム読むと、画面が出るか
    /// どうかが実行中に変わりうる
    pub fn new() -> Self {
        if std::env::var(SKIP_ENV).is_ok_and(|v| v != "0") {
            tracing::warn!(
                "{SKIP_ENV} が指定されている。ログインを飛ばし、demo の固定データを出す"
            );
            return Self::skipped();
        }
        Self::fresh(false)
    }

    /// ログインを飛ばして画面だけ見る。**本物のデータは出ない。**
    ///
    /// レンダラやテーマを触るときと、試験のためにある
    pub fn skipped() -> Self {
        Self::fresh(true)
    }

    fn fresh(skipped: bool) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Login {
            session: Session::Connecting,
            rx,
            tx,
            skipped,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// メイン画面を出してよいか。**ログイン画面との分かれ目はここだけである**
    pub fn shows_main(&self) -> bool {
        self.skipped || self.session.logged_in().is_some()
    }

    /// 背景の仕事を始める。**ウィンドウが出る前に呼んでよい。**
    ///
    /// 鍵の生成に 1 秒前後かかるので、早く始めたぶんだけ QR が早く出る。
    pub fn start(&mut self, rt: &tokio::runtime::Handle, waker: Waker) {
        if self.skipped {
            return;
        }

        // 鍵束が開けなくても**ログインはできる**。保存だけを諦める
        let store = match SecretStore::new() {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(%e, "鍵束を開けない。トークンを保存せず、起動のたびに聞く");
                None
            }
        };

        let tx = self.tx.clone();
        rt.spawn(async move {
            run(tx, waker, store).await;
        });
    }

    /// 溜まっている知らせを**空になるまで**取り込む。変わったら `true`。
    ///
    /// ⚠️ 1 回の `wake` に複数の知らせが乗りうるので、1 件だけ読んではいけない
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.apply(event);
                    changed = true;
                }
                // 送り手が消えていても、状態はそのまま残す。
                // 「繋ぎ直せない」のは Failed が既に知らせている
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return changed,
            }
        }
    }

    fn apply(&mut self, event: LoginEvent) {
        self.session = match event {
            LoginEvent::Qr(url) => Session::WaitingForScan { url, scanned: None },
            // 読まれた。**URL は保ったまま**にする。承認前に取り消されたら
            // 同じ QR がまだ生きている
            LoginEvent::Scanned(user) => {
                match std::mem::replace(&mut self.session, Session::Connecting) {
                    Session::WaitingForScan { url, .. } => Session::WaitingForScan {
                        url,
                        scanned: Some(user),
                    },
                    other => other,
                }
            }
            LoginEvent::Approved => Session::Exchanging,
            LoginEvent::Done(l) => Session::LoggedIn(l),
            LoginEvent::Failed(e) => Session::Failed(e),
            LoginEvent::Restarted => Session::Connecting,
        };
    }
}

impl Default for Login {
    fn default() -> Self {
        Self::new()
    }
}

/// 失敗した後に繋ぎ直すまでの待ち時間。**倍々に伸びて上限で止まる**
const RETRY_MIN: std::time::Duration = std::time::Duration::from_secs(2);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// ログインする。**まず保存されたトークンを試し、駄目なら QR を出す。**
///
/// # 諦めない
///
/// QR には寿命があり、**公式クライアントでも 2 分ほどで消える**。置きっぱなしの
/// 画面の前に戻ってきた利用者が死んだ QR を読んで無反応に困らないよう、
/// 期限が切れたら黙って出し直す。
///
/// ⚠️ **失敗しても諦めない。** ここで止まると、画面に「失敗しました」と
/// 出たまま二度と QR が出ず、**アプリを起動し直すしか道がなくなる**。
/// 網が落ちているだけかもしれないので、待ち時間を伸ばしながら繋ぎ続ける。
/// 理由は画面に出るので、黙って隠しているわけではない。
async fn run(tx: Sender<LoginEvent>, waker: Waker, store: Option<SecretStore>) {
    // ⚠️ **一番先に測る。** [`RestClient`] も Gateway の identify も、
    // ここより後で名乗りを組み立てる。後から測ると片方だけ古い番号を名乗り、
    // **経路の間で食い違う** ([`gumicord_model::identity`])。
    //
    // 画面は既にキャッシュから出ているので (C6)、ここで数百 ms 待っても
    // 利用者は待たされない。取れなくても埋め込みで進む
    gumicord_rest::build_number::measure().await;

    if let Some(l) = restore(store.as_ref()).await {
        let _ = tx.send(LoginEvent::Done(Box::new(l)));
        waker.wake();
        return;
    }

    let mut wait = RETRY_MIN;
    loop {
        match attempt(&tx, &waker, store.as_ref()).await {
            // 入れた。もう繰り返さない
            Ok(true) => return,
            // 期限切れ。**すぐに**出し直す。待たせる理由がない
            Ok(false) => {
                tracing::debug!("QR の期限が切れた。出し直す");
                wait = RETRY_MIN;
                let _ = tx.send(LoginEvent::Restarted);
                waker.wake();
            }
            Err(e) => {
                tracing::warn!(error = %e, wait_s = wait.as_secs(), "リモート認証が失敗した");
                let _ = tx.send(LoginEvent::Failed(e));
                waker.wake();
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RETRY_MAX);
            }
        }
    }
}

/// 保存されたトークンで入り直す (`FR-003`)。
///
/// ⚠️ **通らなかったトークンはその場で捨てる。** 残しておくと、次の起動でも
/// 同じ失敗を繰り返したうえで結局 QR を出すことになる。パスワードを変えた、
/// 端末を無効にした、期限が切れた — どれも普通に起こる。
async fn restore(store: Option<&SecretStore>) -> Option<LoggedIn> {
    let store = store?;
    let raw = match store.load(TOKEN_KEY) {
        Ok(Some(raw)) => raw,
        Ok(None) => return None,
        Err(e) => {
            // 開けないのは異常ではない。別の利用者としてログオンした等
            tracing::warn!(%e, "保存されたトークンを開けない。捨てる");
            let _ = store.clear(TOKEN_KEY);
            return None;
        }
    };

    let token = Token::new(String::from_utf8(raw).ok()?);
    let rest = RestClient::anonymous().ok()?;

    match rest.authenticate(token.clone()).await {
        Ok((client, me)) => {
            tracing::info!(user = %me.user.display_name(), "保存されたトークンで入った");
            Some(LoggedIn { me, client, token })
        }
        Err(e) => {
            tracing::warn!(%e, "保存されたトークンが通らない。捨ててログインし直す");
            let _ = store.clear(TOKEN_KEY);
            None
        }
    }
}

/// 1 回ぶんのやりとり。入れたら `Ok(true)`、期限切れなら `Ok(false)`
async fn attempt(
    tx: &Sender<LoginEvent>,
    waker: &Waker,
    store: Option<&SecretStore>,
) -> Result<bool, String> {
    let mut auth = RemoteAuth::connect().await.map_err(|e| e.to_string())?;
    let rest = RestClient::anonymous().map_err(|e| e.to_string())?;

    loop {
        let event = match auth.next().await {
            Ok(e) => e,
            // ⚠️ **QR の期限切れはこの形で来る。**「取り消し」の合図が来る
            // とは限らず、多くの場合は黙って接続を閉じられるだけである。
            // これを誤りとして扱うと、2 分ごとに「失敗しました」が出る
            Err(gumicord_gateway::RemoteAuthError::Closed) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
        match event {
            RemoteAuthEvent::Ready { url, fingerprint } => {
                tracing::info!(%fingerprint, "QR を出せる");
                let _ = tx.send(LoginEvent::Qr(url));
            }
            RemoteAuthEvent::Scanned(user) => {
                tracing::info!(user = %user.username, "読み取られた");
                let _ = tx.send(LoginEvent::Scanned(user));
            }
            RemoteAuthEvent::Approved { ticket } => {
                let _ = tx.send(LoginEvent::Approved);
                waker.wake();

                // ⚠️ このチケットは 1 回しか使えない。失敗しても再送しない
                let encrypted = rest
                    .remote_auth_login(&ticket)
                    .await
                    .map_err(|e| e.to_string())?;
                let token = auth.decrypt_token(&encrypted).map_err(|e| e.to_string())?;

                // 復号できただけでは、使えるトークンである証拠にならない。
                // `GET /users/@me` が通って初めてログインしたと見なす
                let (client, me) = rest
                    .authenticate(token.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(user = %me.user.display_name(), "ログインした");

                // ⚠️ **通ることを確かめてから預ける** (`FR-003`)。
                // 保存に失敗してもログインは成功である。次の起動で
                // もう一度聞くだけなので、ここで倒れる理由がない
                if let Some(store) = store
                    && let Err(e) = store.store(TOKEN_KEY, token.expose().as_bytes())
                {
                    tracing::warn!(%e, "トークンを保存できなかった。次回もログインが要る");
                }

                let _ = tx.send(LoginEvent::Done(Box::new(LoggedIn { me, client, token })));
                waker.wake();
                return Ok(true);
            }
            RemoteAuthEvent::Cancelled => return Ok(false),
        }
        waker.wake();
    }
}

/// 試験のための入り口。**背景の仕事は動かない。**
///
/// 網を叩かずに状態遷移だけを確かめるためにある
#[cfg(test)]
impl Login {
    pub(crate) fn fresh_for_test() -> Self {
        Self::fresh(false)
    }

    pub(crate) fn apply_for_test(&mut self, event: LoginEvent) {
        self.apply(event);
    }
}

impl Login {
    /// トークンが無効になった (`FR-004`)。**鍵束から捨ててやり直す。**
    ///
    /// ⚠️ 捨てないと、次の起動でも同じ死んだトークンで入ろうとして、
    /// 同じところで弾かれる。
    pub fn forget(&mut self, rt: &tokio::runtime::Handle, waker: Waker) {
        if let Ok(store) = SecretStore::new() {
            let _ = store.clear(TOKEN_KEY);
        }
        self.session = Session::Connecting;
        self.start(rt, waker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_model::UserId;

    fn user(name: &str) -> ScannedUser {
        ScannedUser {
            id: UserId::from(1u64),
            username: name.to_owned(),
            discriminator: "0".to_owned(),
            avatar: None,
        }
    }

    #[test]
    fn the_qr_only_appears_once_it_is_ready() {
        let mut login = Login::fresh(false);
        assert!(login.session().qr().is_none(), "繋ぐ前に QR は出ない");

        login.apply(LoginEvent::Qr("https://example/1".to_owned()));
        assert_eq!(login.session().qr(), Some("https://example/1"));
    }

    /// 読まれても QR は消えない。**承認前に取り消されたらまだ使える**
    #[test]
    fn scanning_keeps_the_qr_alive() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Qr("https://example/1".to_owned()));
        login.apply(LoginEvent::Scanned(user("ねんねこ")));

        assert_eq!(login.session().qr(), Some("https://example/1"));
        assert!(login.session().hint().contains("ねんねこ"));
    }

    /// QR を出す前に読まれた知らせが来ても壊れない。
    /// **順番は Discord 側の都合で入れ替わりうる**
    #[test]
    fn a_scan_without_a_qr_is_ignored() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Scanned(user("ねんねこ")));
        assert!(matches!(login.session(), Session::Connecting));
    }

    /// 溜まった知らせを 1 回で全部取り込む (`Waker` はまとめられる)
    #[test]
    fn polling_drains_everything_that_arrived() {
        let mut login = Login::fresh(false);
        login.tx.send(LoginEvent::Qr("a".to_owned())).unwrap();
        login.tx.send(LoginEvent::Qr("b".to_owned())).unwrap();
        login
            .tx
            .send(LoginEvent::Scanned(user("ねんねこ")))
            .unwrap();

        assert!(login.poll());
        assert_eq!(login.session().qr(), Some("b"), "最後の知らせまで進む");
        assert!(login.session().hint().contains("ねんねこ"));
        assert!(!login.poll(), "空なら再描画を要求しない");
    }

    #[test]
    fn failure_is_shown_not_swallowed() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Failed("接続できない".to_owned()));
        assert!(login.session().hint().contains("接続できない"));
        assert!(login.session().qr().is_none());
    }
}

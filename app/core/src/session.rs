//! Login state and the background work that advances it.
//!
//! QR login is the default because it never raises a captcha, and there is no
//! way to draw hCaptcha in this renderer. A password path would borrow the
//! OS WebView for that; QR stays the default either way.
//!
//! Everything happens on tokio and comes back over a channel. The event loop
//! is asleep, so whoever posts a message wakes it. Wakes coalesce, so always
//! drain the channel rather than reading one message.
//!
//! On start a stored token is tried first, and discarded the moment it fails
//! — keeping it means repeating the same failure on every later start before
//! falling back to the QR anyway. Where the OS cannot encrypt it, nothing is
//! stored: asking each time beats a plaintext token on disk.

use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use gumicord_gateway::{RemoteAuth, RemoteAuthEvent, ScannedUser};
use gumicord_model::{CurrentUser, Token};
use gumicord_platform::{SecretStore, Waker};
use gumicord_rest::RestClient;

/// Skips login to look at the UI. Shows fixed demo data, never real data.
const SKIP_ENV: &str = "GUMICORD_SKIP_LOGIN";

/// The token's name in the OS keychain.
const TOKEN_KEY: &str = "token";

/// How far login has got. The only thing the screen consults.
#[derive(Debug, Clone)]
pub enum Session {
    /// Connecting; no QR yet.
    Connecting,
    /// Showing the QR, waiting for a scan.
    WaitingForScan {
        /// The URL the QR encodes.
        url: String,
        /// Who scanned it. Scanned is not yet approved.
        scanned: Option<ScannedUser>,
    },
    /// Approved; exchanging the ticket for a token.
    Exchanging,
    LoggedIn(Box<LoggedIn>),
    /// Failed, and retryable, so the reason is shown while waiting.
    Failed(String),
}

/// What remains after a successful login.
///
/// The token is here separately because the gateway's identify needs it raw,
/// while REST keeps its own copy inside the client.
#[derive(Debug, Clone)]
pub struct LoggedIn {
    pub me: CurrentUser,
    pub client: RestClient,
    pub token: Token,
}

impl Session {
    /// The line shown on screen; what the user actually reads.
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

    /// The URL to encode, or `None` if there is nothing to show yet.
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

/// What the background reports to the main thread.
///
/// Events, not states: a state arriving late would overwrite a newer one.
#[derive(Debug)]
pub enum LoginEvent {
    /// The QR is ready to show.
    Qr(String),
    Scanned(ScannedUser),
    /// Approved; fetching the token.
    Approved,
    Done(Box<LoggedIn>),
    Failed(String),
    /// No session remains: the stored token was refused outright and
    /// discarded, or none was stored. What is on screen came from a cache
    /// nothing can refresh anymore, so the receiver lets it go.
    Ended,
    /// Restarted, usually after the QR expired.
    Restarted,
}

/// Drives login. The app holds one and drains it with [`Self::poll`].
pub struct Login {
    session: Session,
    rx: Receiver<LoginEvent>,
    tx: Sender<LoginEvent>,
    /// Decided once at startup and never changes.
    skipped: bool,
    /// Why the previous session ended, shown until the next sign-in. Set when
    /// the client signs out on its own, which otherwise reads as a crash.
    notice: Option<String>,
    /// A [`LoginEvent::Ended`] arrived and nobody has read it yet. Not a
    /// session state: the screen changes on the app side, which drops the
    /// cache.
    ended: bool,
}

impl Login {
    /// Reads the environment once: per frame, the screen shown could change
    /// mid-run.
    pub fn new() -> Self {
        if std::env::var(SKIP_ENV).is_ok_and(|v| v != "0") {
            tracing::warn!("{SKIP_ENV} is set; skipping login and showing demo data");
            return Self::skipped();
        }
        Self::fresh(false)
    }

    /// Skips login and shows demo data. For renderer and theme work, and for
    /// tests.
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
            notice: None,
            ended: false,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The line shown on screen; the notice leads when there is one.
    pub fn hint(&self) -> String {
        match &self.notice {
            Some(n) => format!("{n}。{}", self.session.hint()),
            None => self.session.hint().clone(),
        }
    }

    /// Says why the last session ended. Kept until signing in succeeds.
    pub fn set_notice(&mut self, reason: &str) {
        self.notice = Some(reason.to_owned());
    }

    /// Whether the session ended on its own since the last call.
    pub fn take_ended(&mut self) -> bool {
        std::mem::take(&mut self.ended)
    }

    /// The only thing that decides between the login and main screens.
    pub fn shows_main(&self) -> bool {
        self.skipped || self.session.logged_in().is_some()
    }

    /// Starts the background work. Safe to call before the window exists,
    /// and worth doing: key generation takes about a second.
    pub fn start(&mut self, rt: &tokio::runtime::Handle, waker: Waker) {
        if self.skipped {
            return;
        }

        // Login still works without a keychain; only storing is lost.
        let store = match SecretStore::new() {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(%e, "no keychain; the token will not be stored");
                None
            }
        };

        let tx = self.tx.clone();
        rt.spawn(async move {
            run(tx, waker, store).await;
        });
    }

    /// Drains every pending event. One wake can carry several.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.apply(event);
                    changed = true;
                }
                // Keep the state even if the sender is gone; Failed has
                // already reported the disconnection.
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return changed,
            }
        }
    }

    fn apply(&mut self, event: LoginEvent) {
        self.session = match event {
            LoginEvent::Qr(url) => Session::WaitingForScan { url, scanned: None },
            // Keep the URL: if approval is cancelled the same QR still works.
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
            // In again; whatever ended the last session no longer matters.
            LoginEvent::Done(l) => {
                self.notice = None;
                Session::LoggedIn(l)
            }
            LoginEvent::Failed(e) => Session::Failed(e),
            LoginEvent::Ended => {
                self.ended = true;
                self.session.clone()
            }
            LoginEvent::Restarted => Session::Connecting,
        };
    }
}

impl Default for Login {
    fn default() -> Self {
        Self::new()
    }
}

/// Reconnect backoff after a failure; doubles up to the maximum.
const RETRY_MIN: std::time::Duration = std::time::Duration::from_secs(2);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// Logs in: stored token first, QR otherwise.
///
/// Never gives up. A QR expires after about two minutes, so an expired one is
/// quietly replaced rather than left dead in front of someone who walked away.
/// A failure retries with growing backoff too: stopping here would leave
/// "failed" on screen with no way forward but restarting the app.
///
/// Every turn starts by trying the stored token again, so a start made
/// offline comes alive by itself once the network is back.
async fn run(tx: Sender<LoginEvent>, waker: Waker, store: Option<SecretStore>) {
    // Measure first: both REST and the gateway build their claim after this,
    // and measuring later would leave one of them stale. The screen is
    // already up from cache, so this wait is not felt.
    gumicord_rest::build_number::measure().await;

    // Said once per run: after the first turn either the keychain holds no
    // token anymore or it never did, and repeating it changes nothing.
    let mut said_ended = false;
    let mut wait = RETRY_MIN;
    loop {
        match restore(store.as_ref()).await {
            RestoreOutcome::LoggedIn(l) => {
                let _ = tx.send(LoginEvent::Done(l));
                waker.wake();
                return;
            }
            RestoreOutcome::Gone => {
                if !said_ended {
                    said_ended = true;
                    let _ = tx.send(LoginEvent::Ended);
                    waker.wake();
                }
            }
            RestoreOutcome::Unchecked => {}
        }

        match attempt(&tx, &waker, store.as_ref()).await {
            // In; stop looping.
            Ok(true) => return,
            // Expired; reissue at once.
            Ok(false) => {
                tracing::debug!("the QR expired; reissuing");
                wait = RETRY_MIN;
                let _ = tx.send(LoginEvent::Restarted);
                waker.wake();
            }
            Err(e) => {
                tracing::warn!(error = %e, wait_s = wait.as_secs(), "remote auth failed");
                let _ = tx.send(LoginEvent::Failed(e));
                waker.wake();
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RETRY_MAX);
            }
        }
    }
}

/// What became of the stored token.
enum RestoreOutcome {
    /// It is still good.
    LoggedIn(Box<LoggedIn>),
    /// There is nothing left to restore: the token was refused outright and
    /// discarded, or none was ever stored. Only a fresh sign-in can follow.
    Gone,
    /// It could not be checked just now, so nothing was decided. It stays
    /// stored, and a later try settles it.
    Unchecked,
}

/// Signs in with the stored token, discarding it the moment Discord refuses
/// it.
///
/// A changed password, a revoked device and an expired token are all
/// ordinary; keeping a dead token repeats the failure on every later start.
/// Anything short of a refusal — the network, mostly — decides nothing, and
/// the token stays for a later try.
async fn restore(store: Option<&SecretStore>) -> RestoreOutcome {
    let Some(store) = store else {
        return RestoreOutcome::Gone;
    };
    let raw = match store.load(TOKEN_KEY) {
        Ok(Some(raw)) => raw,
        // Nothing stored is the ordinary first start.
        Ok(None) => return RestoreOutcome::Gone,
        Err(e) => {
            // Not exceptional: a different OS user, for instance. Unreadable
            // from here on, so the entry goes rather than failing in
            // silence every start.
            tracing::warn!(%e, "cannot read the stored token; discarding it");
            let _ = store.clear(TOKEN_KEY);
            return RestoreOutcome::Gone;
        }
    };

    let token = Token::new(String::from_utf8_lossy(&raw).into_owned());
    let rest = match RestClient::anonymous() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%e, "cannot build a client to check the stored token");
            return RestoreOutcome::Unchecked;
        }
    };

    match rest.authenticate(token.clone()).await {
        Ok((client, me)) => {
            tracing::info!(user = %me.user.display_name(), "signed in with the stored token");
            RestoreOutcome::LoggedIn(Box::new(LoggedIn { me, client, token }))
        }
        Err(e) if e.is_unauthorized() => {
            tracing::warn!(%e, "the stored token was rejected; discarding it");
            let _ = store.clear(TOKEN_KEY);
            RestoreOutcome::Gone
        }
        Err(e) => {
            tracing::warn!(%e, "could not check the stored token; keeping it for a later try");
            RestoreOutcome::Unchecked
        }
    }
}

/// One exchange. `Ok(true)` on success, `Ok(false)` if the QR expired.
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
            // Expiry usually arrives as a silent close rather than a cancel.
            // Treating that as an error puts "failed" on screen every two
            // minutes.
            Err(gumicord_gateway::RemoteAuthError::Closed) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
        match event {
            RemoteAuthEvent::Ready { url, fingerprint } => {
                tracing::info!(%fingerprint, "the QR is ready");
                let _ = tx.send(LoginEvent::Qr(url));
            }
            RemoteAuthEvent::Scanned(user) => {
                tracing::info!(user = %user.username, "the QR was scanned");
                let _ = tx.send(LoginEvent::Scanned(user));
            }
            RemoteAuthEvent::Approved { ticket } => {
                let _ = tx.send(LoginEvent::Approved);
                waker.wake();

                // Single-use; never resend after a failure.
                let encrypted = rest
                    .remote_auth_login(&ticket)
                    .await
                    .map_err(|e| e.to_string())?;
                let token = auth.decrypt_token(&encrypted).map_err(|e| e.to_string())?;

                // Decrypting proves nothing about validity; only a successful
                // `GET /users/@me` counts as logged in.
                let (client, me) = rest
                    .authenticate(token.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(user = %me.user.display_name(), "signed in");

                // Stored only after it is known to work. A failure to store
                // does not fail the login; the next start just asks again.
                if let Some(store) = store
                    && let Err(e) = store.store(TOKEN_KEY, token.expose().as_bytes())
                {
                    tracing::warn!(%e, "could not store the token; the next start will ask again");
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

/// Test entry point; runs no background work.
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
    /// Drops the stored token and starts over.
    ///
    /// Called both when the gateway rejects it and when the user signs out.
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
        assert!(login.session().qr().is_none(), "a QR before connecting");

        login.apply(LoginEvent::Qr("https://example/1".to_owned()));
        assert_eq!(login.session().qr(), Some("https://example/1"));
    }

    /// A scan does not clear the QR: it still works if approval is cancelled.
    #[test]
    fn scanning_keeps_the_qr_alive() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Qr("https://example/1".to_owned()));
        login.apply(LoginEvent::Scanned(user("ねんねこ")));

        assert_eq!(login.session().qr(), Some("https://example/1"));
        assert!(login.session().hint().contains("ねんねこ"));
    }

    /// Discord may reorder these, so a scan before the QR must not break.
    #[test]
    fn a_scan_without_a_qr_is_ignored() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Scanned(user("ねんねこ")));
        assert!(matches!(login.session(), Session::Connecting));
    }

    /// Wakes coalesce, so one poll must drain everything.
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
        assert_eq!(
            login.session().qr(),
            Some("b"),
            "did not advance to the last event"
        );
        assert!(login.session().hint().contains("ねんねこ"));
        assert!(!login.poll(), "an empty poll asked for a redraw");
    }

    #[test]
    fn failure_is_shown_not_swallowed() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Failed("接続できない".to_owned()));
        assert!(login.session().hint().contains("接続できない"));
        assert!(login.session().qr().is_none());
    }

    /// Being signed out on its own must say why, or the QR screen reads as a
    /// crash. The notice leads and leaves once signing in succeeds.
    #[test]
    fn a_notice_leads_until_signing_in_succeeds() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Qr("https://example/1".to_owned()));
        login.set_notice("セッションが無効になったため、ログアウトしました");
        assert!(login.hint().contains("セッションが無効"));
        assert!(
            login.hint().contains("QR を読み取ってください"),
            "the how-to line must survive"
        );

        login.apply(LoginEvent::Done(Box::new(LoggedIn {
            me: serde_json::from_str(r#"{"id":"1","username":"ねんねこ"}"#).unwrap(),
            client: RestClient::anonymous().unwrap(),
            token: Token::new("t"),
        })));
        assert_eq!(
            login.hint(),
            login.session().hint(),
            "notice outlived login"
        );
    }

    #[test]
    fn without_a_notice_the_hint_is_unchanged() {
        let mut login = Login::fresh(false);
        login.apply(LoginEvent::Qr("https://example/1".to_owned()));
        assert_eq!(login.hint(), login.session().hint());
    }

    /// The end is picked up exactly once, and it does not stop the QR flow:
    /// a fresh sign-in is the only way forward after it.
    #[test]
    fn the_end_is_reported_once_and_the_qr_keeps_running() {
        let mut login = Login::fresh_for_test();
        assert!(!login.take_ended());

        login.apply(LoginEvent::Ended);
        login.apply(LoginEvent::Qr("https://example/1".to_owned()));

        assert!(login.take_ended());
        assert!(!login.take_ended(), "read twice");
        assert_eq!(
            login.session().qr(),
            Some("https://example/1"),
            "the QR flow stopped"
        );
    }
}

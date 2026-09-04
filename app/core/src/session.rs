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
use gumicord_rest::{CaptchaChallenge, LoginOutcome, RestClient, RestError, SolvedCaptcha};

use crate::account::{AccountKey, AccountsIndex, LEGACY_BOT_TOKEN_KEY, LEGACY_USER_TOKEN_KEY};

/// Skips login to look at the UI. Shows fixed demo data, never real data.
const SKIP_ENV: &str = "GUMICORD_SKIP_LOGIN";

const TOKEN_KEY: &str = LEGACY_USER_TOKEN_KEY;
const BOT_TOKEN_KEY: &str = LEGACY_BOT_TOKEN_KEY;

fn remember_account(store: Option<&SecretStore>, me: &CurrentUser, token: &Token) {
    let Some(store) = store else {
        return;
    };
    let mut index = AccountsIndex::load(store).unwrap_or_default();
    let key = AccountKey::new(me.user.id, token.is_bot());
    if let Err(e) = index.remember(store, key, me.user.display_name().to_owned(), token) {
        tracing::warn!(%e, "could not store the account credentials; next start will ask again");
    }
}

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
    /// Asking for email and password.
    Password,
    /// Asking for a TOTP code to finish a password login.
    PasswordTotp,
    /// Asking for a bot token, opened by a hidden code on the QR screen.
    Token,
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
            Session::Password => "メールアドレスとパスワードでログインします".to_owned(),
            Session::Token => "ボットトークンでログインします".to_owned(),
            Session::PasswordTotp => "認証コードを入力してください".to_owned(),
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
    /// The background needs a second factor to finish a login.
    TotpNeeded {
        email: String,
    },
    /// The background needs a captcha solved before login can continue.
    CaptchaNeeded(CaptchaChallenge),
}

/// A user-driven instruction, sent from the UI thread into the background
/// login loop. These are what turn the QR screen into a password login.
#[derive(Debug)]
pub enum LoginCommand {
    /// Start a password login with these credentials.
    Password { email: String, password: String },
    /// Supply the TOTP (or backup) code for an ongoing login.
    Totp { code: String },
    /// A solved captcha, retrying the call Discord challenged.
    Captcha(SolvedCaptcha),
    /// Start a bot-token login with this token. Hidden path for development:
    /// the token is not stored, so it must be pasted each run.
    BotToken { token: String },
    /// Abandon the password login and go back to the QR.
    CancelPassword,
}

/// Drives login. The app holds one and drains it with [`Self::poll`].
pub struct Login {
    session: Session,
    rx: Receiver<LoginEvent>,
    tx: Sender<LoginEvent>,
    /// Commands run by the background login loop. Unbounded, and the loop is
    /// the only receiver.
    cmd_tx: tokio::sync::mpsc::UnboundedSender<LoginCommand>,
    /// Decided once at startup and never changes.
    skipped: bool,
    /// Why the previous session ended, shown until the next sign-in. Set when
    /// the client signs out on its own, which otherwise reads as a crash.
    notice: Option<String>,
    /// The last login failure, which the app shows on the login form. Set
    /// when a `Failed` event applies; cleared by a successful [`Done`].
    ///
    /// [`Done`]: LoginEvent::Done
    last_error: Option<String>,
    /// A [`LoginEvent::Ended`] arrived and nobody has read it yet. Not a
    /// session state: the screen changes on the app side, which drops the
    /// cache.
    ended: bool,
    /// A captcha awaiting a solution, if the API challenged a login. The app
    /// hands it to the platform, which shows the modal, then reads it back as
    /// the solved token.
    pending: Option<gumicord_rest::CaptchaChallenge>,
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
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        Login {
            session: Session::Connecting,
            rx,
            tx,
            cmd_tx,
            skipped,
            notice: None,
            last_error: None,
            ended: false,
            pending: None,
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
        self.start_with_restore(rt, waker, true);
    }

    /// Starts the background work, optionally skipping the stored token.
    /// Adding an account must not restore: the current account is still
    /// saved, so restoring would sign straight back into it and the QR
    /// would never get a chance.
    fn start_with_restore(&mut self, rt: &tokio::runtime::Handle, waker: Waker, restore: bool) {
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
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        self.cmd_tx = cmd_tx;
        rt.spawn(async move {
            run(tx, waker, store, cmd_rx, restore).await;
        });
    }

    /// Switches the screen to the password form.
    pub fn start_password(&mut self) {
        self.notice = None;
        self.session = Session::Password;
    }

    /// Switches the screen to the bot-token form.
    pub fn start_token(&mut self) {
        self.notice = None;
        self.session = Session::Token;
    }

    /// Asks the background to log in with these credentials.
    pub fn submit_password(&self, email: String, password: String) {
        let _ = self.cmd_tx.send(LoginCommand::Password { email, password });
    }

    /// Asks the background to finish login with a TOTP code.
    pub fn submit_totp(&self, code: String) {
        let _ = self.cmd_tx.send(LoginCommand::Totp { code });
    }

    /// Hands a solved captcha to the background login.
    pub fn submit_captcha(&self, solved: SolvedCaptcha) {
        let _ = self.cmd_tx.send(LoginCommand::Captcha(solved));
    }

    /// Asks the background to log in with a bot token.
    pub fn submit_bot_token(&self, token: String) {
        let _ = self.cmd_tx.send(LoginCommand::BotToken { token });
    }

    /// Goes back to the QR screen, abandoning a password login.
    pub fn cancel_password(&self) {
        let _ = self.cmd_tx.send(LoginCommand::CancelPassword);
    }

    /// The challenge awaiting a solution, if the login is captcha-blocked.
    /// Consumed once: the app hands it to the platform's modal.
    pub fn take_pending(&mut self) -> Option<gumicord_rest::CaptchaChallenge> {
        self.pending.take()
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
                self.pending = None;
                self.last_error = None;
                Session::LoggedIn(l)
            }
            LoginEvent::Failed(e) => {
                self.last_error = Some(e.clone());
                Session::Failed(e)
            }
            LoginEvent::TotpNeeded { .. } => Session::PasswordTotp,
            // The form stays put while the challenge is solved elsewhere.
            LoginEvent::CaptchaNeeded(ch) => {
                self.pending = Some(ch);
                Session::Password
            }
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

/// Logs in: stored token first, QR by default, password when asked.
///
/// Never gives up. A QR expires after about two minutes, so an expired one is
/// quietly replaced rather than left dead in front of someone who walked away.
/// A failure retries with growing backoff too: stopping here would leave
/// "failed" on screen with no way forward but restarting the app.
///
/// Every turn starts by trying the stored token again, so a start made
/// offline comes alive by itself once the network is back.
///
/// The QR and a password login share the loop. The QR runs in the background
/// and a `Password` command supersedes it; once that login finishes, is
/// cancelled or fails, the QR comes back. Whatever eventually succeeds ends
/// the run.
async fn run(
    tx: Sender<LoginEvent>,
    waker: Waker,
    store: Option<SecretStore>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<LoginCommand>,
    restore_enabled: bool,
) {
    // Measure first: both REST and the gateway build their claim after this,
    // and measuring later would leave one of them stale. The screen is
    // already up from cache, so this wait is not felt.
    gumicord_rest::build_number::measure().await;

    // Said once per run: after the first turn either the keychain holds no
    // token anymore or it never did, and repeating it changes nothing.
    let mut said_ended = false;
    let mut wait = RETRY_MIN;
    loop {
        // Adding an account skips this entirely, every turn: the current
        // account is still saved, and restoring it would sign straight back
        // into it — even after the QR expired and the loop came back around.
        if restore_enabled {
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
        }

        // The QR runs in the background so a password command can interrupt
        // its wait.
        let mut qr = tokio::spawn(attempt(tx.clone(), waker.clone(), store.clone()));

        let outcome = tokio::select! {
            r = &mut qr => match r {
                // In; the attempt already sent Done.
                Ok(Ok(true)) => return,
                // Expired; reissue at once.
                Ok(Ok(false)) => {
                    tracing::debug!("the QR expired; reissuing");
                    QrOutcome::Reset
                }
                Ok(Err(e)) => QrOutcome::Failed(e),
                Err(e) => {
                    tracing::error!(%e, "the QR task panicked");
                    QrOutcome::Reset
                }
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(LoginCommand::Password { email, password }) => {
                    qr.abort();
                    let rest = match RestClient::anonymous() {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = tx.send(LoginEvent::Failed(e.to_string()));
                            waker.wake();
                            continue;
                        }
                    };
                    match run_password(&tx, &waker, store.as_ref(), rest, email, password, &mut cmd_rx).await {
                        // In; the password path already sent Done.
                        PasswordRun::LoggedIn => return,
                        PasswordRun::Error(e) => {
                            let _ = tx.send(LoginEvent::Failed(e));
                            waker.wake();
                        }
                        PasswordRun::Cancelled => {}
                    }
                    wait = RETRY_MIN;
                    let _ = tx.send(LoginEvent::Restarted);
                    waker.wake();
                    continue;
                }
                Some(LoginCommand::BotToken { token }) => {
                    qr.abort();
                    match run_bot_token(&tx, &waker, store.as_ref(), token).await {
                        // In; the bot path already sent Done.
                        true => return,
                        false => {
                            let _ = tx.send(LoginEvent::Restarted);
                            waker.wake();
                            continue;
                        }
                    }
                }
                // A stray captcha or totp command with no password in flight.
                Some(_) | None => continue,
            }
        };

        match outcome {
            QrOutcome::Reset => {
                let _ = tx.send(LoginEvent::Restarted);
                waker.wake();
            }
            QrOutcome::Failed(e) => {
                tracing::warn!(error = %e, wait_s = wait.as_secs(), "remote auth failed");
                let _ = tx.send(LoginEvent::Failed(e));
                waker.wake();
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RETRY_MAX);
            }
        }
    }
}

/// What happened to the QR attempt.
enum QrOutcome {
    /// Expired, or otherwise needs a fresh round.
    Reset,
    /// The attempt failed; back off and try again.
    Failed(String),
}

/// What became of a password login.
enum PasswordRun {
    /// Signed in. [`LoginEvent::Done`] was already sent.
    LoggedIn,
    /// The user abandoned it; go back to the QR.
    Cancelled,
    /// It failed; show the reason and return to the QR.
    Error(String),
}

/// Drives a password login to completion, interrupting the QR.
///
/// Walks the steps: password check, a captcha if Discord challenges it, then
/// a TOTP code if the account uses a second factor. Each wait reads commands
/// off `cmd_rx` (the UI sends the code or the solved captcha), and a
/// `CancelPassword` at any point abandons the whole thing.
async fn run_password(
    tx: &Sender<LoginEvent>,
    waker: &Waker,
    store: Option<&SecretStore>,
    rest: RestClient,
    email: String,
    password: String,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<LoginCommand>,
) -> PasswordRun {
    let mut password = Some(password);
    let mut solved: Option<SolvedCaptcha> = None;
    let mut ticket: Option<String> = None;

    loop {
        let token = if let Some(t) = ticket.take() {
            // A second factor finishes the account the password opened.
            let code = match await_totp(cmd_rx).await {
                Some(c) => c,
                None => return PasswordRun::Cancelled,
            };
            match rest.mfa_totp(&t, &code).await {
                Ok(tok) => Some(tok),
                // A wrong code is just another chance to ask.
                Err(e) => {
                    let _ = tx.send(LoginEvent::TotpNeeded {
                        email: email.clone(),
                    });
                    waker.wake();
                    tracing::debug!(%e, "totp rejected; asking again");
                    continue;
                }
            }
        } else {
            let pass = password.as_deref().unwrap_or_default();
            match rest.login(&email, pass, solved.as_ref()).await {
                Ok(gumicord_rest::LoginOutcome::Token(tok)) => {
                    password = None;
                    solved = None;
                    Some(tok)
                }
                Ok(LoginOutcome::MfaRequired { ticket: t }) => {
                    let _ = tx.send(LoginEvent::TotpNeeded {
                        email: email.clone(),
                    });
                    waker.wake();
                    ticket = Some(t);
                    continue;
                }
                Err(RestError::CaptchaRequired(ch)) => {
                    let _ = tx.send(LoginEvent::CaptchaNeeded(*ch));
                    waker.wake();
                    match await_captcha(cmd_rx).await {
                        Some(s) => {
                            solved = Some(s);
                            continue;
                        }
                        None => return PasswordRun::Cancelled,
                    }
                }
                Err(e) => return PasswordRun::Error(e.to_string()),
            }
        };

        if let Some(tok) = token {
            // A returned token is not yet proof; only `GET /users/@me` counts.
            match rest.authenticate(tok.clone()).await {
                Ok((client, me)) => {
                    tracing::info!(user = %me.user.display_name(), "signed in with password");
                    remember_account(store, &me, &tok);
                    let _ = tx.send(LoginEvent::Done(Box::new(LoggedIn {
                        me,
                        client,
                        token: tok,
                    })));
                    waker.wake();
                    return PasswordRun::LoggedIn;
                }
                Err(e) => return PasswordRun::Error(e.to_string()),
            }
        }
    }
}

/// Logs in with a bot token.
///
/// Hidden path: the token rides straight into REST with a `Bot ` prefix and
/// is validated by `GET /users/@me`. It is stored separately from user tokens
/// so restoring it retains the bot kind.
///
/// Returns `true` on success; [`LoginEvent::Done`] was already sent.
async fn run_bot_token(
    tx: &Sender<LoginEvent>,
    waker: &Waker,
    store: Option<&SecretStore>,
    token: String,
) -> bool {
    let token = Token::bot(token);
    let rest = match RestClient::anonymous() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(LoginEvent::Failed(e.to_string()));
            waker.wake();
            return false;
        }
    };

    match rest.authenticate(token.clone()).await {
        Ok((client, me)) => {
            tracing::info!(user = %me.user.display_name(), "signed in with a bot token");
            remember_account(store, &me, &token);
            let _ = tx.send(LoginEvent::Done(Box::new(LoggedIn { me, client, token })));
            waker.wake();
            true
        }
        Err(e) => {
            let _ = tx.send(LoginEvent::Failed(e.to_string()));
            waker.wake();
            false
        }
    }
}

/// Reads commands until a TOTP code arrives, or a cancel/close ends the flow.
async fn await_totp(
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<LoginCommand>,
) -> Option<String> {
    while let Some(c) = cmd_rx.recv().await {
        match c {
            LoginCommand::Totp { code } => return Some(code),
            LoginCommand::CancelPassword => return None,
            // A captcha or a fresh password is out of place while we wait.
            _ => {}
        }
    }
    None
}

/// Reads commands until a solved captcha arrives, or a cancel/close ends it.
async fn await_captcha(
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<LoginCommand>,
) -> Option<SolvedCaptcha> {
    while let Some(c) = cmd_rx.recv().await {
        match c {
            LoginCommand::Captcha(s) => return Some(s),
            LoginCommand::CancelPassword => return None,
            _ => {}
        }
    }
    None
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
    let mut index = match AccountsIndex::load(store) {
        Ok(idx) => idx,
        Err(e) => {
            tracing::warn!(%e, "cannot read accounts index");
            return RestoreOutcome::Gone;
        }
    };

    let candidate = index
        .active
        .or_else(|| index.accounts.first().map(|a| a.key));
    let (account_key, token, is_legacy) = if let Some(key) = candidate {
        match index.load_token(store, key) {
            Ok(Some(tok)) => (Some(key), tok, false),
            Ok(None) => {
                tracing::warn!("stored account has no token; clearing entry");
                let _ = index.remove(store, key);
                return RestoreOutcome::Gone;
            }
            Err(e) => {
                tracing::warn!(%e, "cannot read stored account token");
                return RestoreOutcome::Gone;
            }
        }
    } else {
        match store.load(TOKEN_KEY) {
            Ok(Some(raw)) => (
                None,
                Token::new(String::from_utf8_lossy(&raw).into_owned()),
                true,
            ),
            Ok(None) => match store.load(BOT_TOKEN_KEY) {
                Ok(Some(raw)) => (
                    None,
                    Token::bot(String::from_utf8_lossy(&raw).into_owned()),
                    true,
                ),
                Ok(None) => return RestoreOutcome::Gone,
                Err(e) => {
                    tracing::warn!(%e, "cannot read legacy bot token");
                    let _ = store.clear(BOT_TOKEN_KEY);
                    return RestoreOutcome::Gone;
                }
            },
            Err(e) => {
                tracing::warn!(%e, "cannot read legacy token");
                let _ = store.clear(TOKEN_KEY);
                return RestoreOutcome::Gone;
            }
        }
    };

    let rest = match RestClient::anonymous() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%e, "cannot build client to restore token");
            return RestoreOutcome::Unchecked;
        }
    };

    match rest.authenticate(token.clone()).await {
        Ok((client, me)) => {
            tracing::info!(user = %me.user.display_name(), "restored session");
            if is_legacy {
                let key = AccountKey::new(me.user.id, token.is_bot());
                let _ = index.remember(store, key, me.user.display_name().to_owned(), &token);
            }
            RestoreOutcome::LoggedIn(Box::new(LoggedIn { me, client, token }))
        }
        Err(e) if e.is_unauthorized() => {
            tracing::warn!(%e, "stored credentials were rejected; clearing");
            if let Some(key) = account_key {
                let _ = index.remove(store, key);
            } else {
                let _ = store.clear(TOKEN_KEY);
                let _ = store.clear(BOT_TOKEN_KEY);
            }
            RestoreOutcome::Gone
        }
        Err(e) => {
            tracing::warn!(%e, "could not verify stored token; retaining for later");
            RestoreOutcome::Unchecked
        }
    }
}

/// One exchange. `Ok(true)` on success, `Ok(false)` if the QR expired.
///
/// Takes ownership so it can run behind `tokio::spawn`; the run loop holds
/// shared clones. Never panics: QR expiry and cancellation are ordinary.
async fn attempt(
    tx: Sender<LoginEvent>,
    waker: Waker,
    store: Option<SecretStore>,
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
                remember_account(store.as_ref(), &me, &token);

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
    /// The last login failure, cleared as it is handed over. The app shows it
    /// on the form; a failure that reaches the screen must reach the user, not
    /// vanish when the background restarts.
    pub(crate) fn take_last_error(&mut self) -> Option<String> {
        self.last_error.take()
    }
}

impl Login {
    /// Drops the stored token for the current account and starts over.
    ///
    /// Called both when the gateway rejects it and when the user signs out.
    pub fn forget(&mut self, rt: &tokio::runtime::Handle, waker: Waker) {
        if let Ok(store) = SecretStore::new() {
            if let Some(logged_in) = self.session.logged_in() {
                let key = AccountKey::new(logged_in.me.user.id, logged_in.token.is_bot());
                if let Ok(mut idx) = AccountsIndex::load(&store) {
                    let _ = idx.remove(&store, key);
                }
            }
            let _ = store.clear(TOKEN_KEY);
            let _ = store.clear(BOT_TOKEN_KEY);
        }
        self.session = Session::Connecting;
        self.start(rt, waker);
    }

    /// Resets session to connecting and starts QR login without wiping accounts.
    pub fn start_add_account(&mut self, rt: &tokio::runtime::Handle, waker: Waker) {
        self.session = Session::Connecting;
        self.ended = false;
        self.notice = None;
        self.last_error = None;
        self.pending = None;
        self.start_with_restore(rt, waker, false);
    }

    /// Sets the session directly to logged in.
    pub fn set_logged_in(&mut self, logged_in: LoggedIn) {
        self.session = Session::LoggedIn(Box::new(logged_in));
        self.ended = false;
        self.notice = None;
        self.last_error = None;
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

    /// Starting the password form puts the session in the password state.
    #[test]
    fn starting_the_password_form_switches_to_it() {
        let mut login = Login::fresh_for_test();
        login.start_password();
        assert!(matches!(login.session(), Session::Password));
        assert!(login.session().hint().contains("パスワード"));
    }

    /// A captcha challenge keeps the password form up and is captured for the
    /// platform, once.
    #[test]
    fn a_captcha_challenge_keeps_the_password_form() {
        let mut login = Login::fresh_for_test();
        login.apply(LoginEvent::CaptchaNeeded(gumicord_rest::CaptchaChallenge {
            sitekey: Some("abc".to_owned()),
            service: None,
            rqdata: Some("def".to_owned()),
            rqtoken: Some("ghi".to_owned()),
            session_id: Some("s".to_owned()),
        }));
        assert!(matches!(login.session(), Session::Password));

        let pending = login.take_pending().expect("the challenge is captured");
        assert_eq!(pending.sitekey.as_deref(), Some("abc"));
        assert_eq!(pending.rqtoken.as_deref(), Some("ghi"));
        assert!(login.take_pending().is_none(), "read once");
    }

    /// Needing a TOTP code advances the session to the second factor step.
    #[test]
    fn needing_a_totp_code_advances_to_it() {
        let mut login = Login::fresh_for_test();
        login.apply(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
        });
        assert!(matches!(login.session(), Session::PasswordTotp));
        assert!(login.session().hint().contains("コード"));
    }

    /// The password command senders place exactly one command on the channel.
    #[test]
    fn password_commands_reach_the_background() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let login = Login {
            cmd_tx: tx,
            ..Login::fresh_for_test()
        };

        login.submit_password("a@b.c".to_owned(), "secret".to_owned());
        login.submit_totp("123456".to_owned());
        login.submit_captcha(SolvedCaptcha {
            key: "k".to_owned(),
            rqtoken: Some("r".to_owned()),
            session_id: None,
        });
        login.submit_bot_token("bot-token".to_owned());
        login.cancel_password();

        assert!(matches!(rx.try_recv(), Ok(LoginCommand::Password { .. })));
        assert!(matches!(rx.try_recv(), Ok(LoginCommand::Totp { .. })));
        assert!(matches!(rx.try_recv(), Ok(LoginCommand::Captcha(_))));
        assert!(
            matches!(rx.try_recv(), Ok(LoginCommand::BotToken { token }) if token == "bot-token")
        );
        assert!(matches!(rx.try_recv(), Ok(LoginCommand::CancelPassword)));
        assert!(rx.try_recv().is_err(), "an extra command leaked");
    }
}

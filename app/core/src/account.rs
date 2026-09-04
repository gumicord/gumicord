//! Account storage and multi-account state.
//!
//! Credentials live in the OS secure store under distinct opaque keys. Account
//! metadata (ID, kind, and display name) is tracked in an index without storing
//! the raw token alongside it.

use gumicord_model::{Token, UserId};
use gumicord_platform::{SecretError, SecretStore};
use serde::{Deserialize, Serialize};

/// Legacy single-token keys for backward compatibility.
pub const LEGACY_USER_TOKEN_KEY: &str = "token";
pub const LEGACY_BOT_TOKEN_KEY: &str = "bot_token";

/// SecretStore key used for the encrypted accounts index metadata.
const ACCOUNTS_INDEX_KEY: &str = "accounts_index";

/// Uniquely identifies an account by user ID and token kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountKey {
    pub id: UserId,
    pub is_bot: bool,
}

impl AccountKey {
    pub fn new(id: UserId, is_bot: bool) -> Self {
        AccountKey { id, is_bot }
    }

    /// The key used in `SecretStore` to hold this account's token.
    pub fn secret_key(&self) -> String {
        let prefix = if self.is_bot {
            "account_bot"
        } else {
            "account_user"
        };
        format!("{prefix}_{}", self.id)
    }
}

/// Metadata for a saved account, kept without token contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAccount {
    pub key: AccountKey,
    pub display_name: String,
}

/// The collection of saved accounts and current selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsIndex {
    pub active: Option<AccountKey>,
    pub accounts: Vec<StoredAccount>,
}

impl AccountsIndex {
    /// Loads the stored index, or creates an empty one if not yet initialized.
    pub fn load(store: &SecretStore) -> Result<Self, SecretError> {
        let raw = match store.load(ACCOUNTS_INDEX_KEY)? {
            Some(b) => b,
            None => return Ok(Self::default()),
        };
        match serde_json::from_slice::<AccountsIndex>(&raw) {
            Ok(idx) => Ok(idx),
            Err(e) => {
                tracing::warn!(%e, "failed to parse accounts index; starting fresh");
                Ok(Self::default())
            }
        }
    }

    /// Saves the accounts index to the secure store.
    pub fn save(&self, store: &SecretStore) -> Result<(), SecretError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        store.store(ACCOUNTS_INDEX_KEY, &bytes)
    }

    /// Remembers or updates an account and sets it as the active account.
    /// Also migrates any legacy single-token entries that are still present.
    pub fn remember(
        &mut self,
        store: &SecretStore,
        key: AccountKey,
        display_name: String,
        token: &Token,
    ) -> Result<(), SecretError> {
        // Store the token in the account-specific key.
        store.store(&key.secret_key(), token.expose().as_bytes())?;

        if let Some(existing) = self.accounts.iter_mut().find(|a| a.key == key) {
            existing.display_name = display_name;
        } else {
            self.accounts.push(StoredAccount { key, display_name });
        }
        self.active = Some(key);

        // Remove legacy entries once an account is securely saved under its own key.
        let _ = store.clear(LEGACY_USER_TOKEN_KEY);
        let _ = store.clear(LEGACY_BOT_TOKEN_KEY);

        self.save(store)
    }

    /// Retrieves an account's token from the secure store.
    pub fn load_token(
        &self,
        store: &SecretStore,
        key: AccountKey,
    ) -> Result<Option<Token>, SecretError> {
        let raw = match store.load(&key.secret_key())? {
            Some(b) => b,
            None => return Ok(None),
        };
        let secret = String::from_utf8_lossy(&raw).into_owned();
        let token = if key.is_bot {
            Token::bot(secret)
        } else {
            Token::new(secret)
        };
        Ok(Some(token))
    }

    /// Removes an account and deletes its secret key.
    pub fn remove(&mut self, store: &SecretStore, key: AccountKey) -> Result<(), SecretError> {
        let _ = store.clear(&key.secret_key());
        self.accounts.retain(|a| a.key != key);
        if self.active == Some(key) {
            self.active = self.accounts.first().map(|a| a.key);
        }
        self.save(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> SecretStore {
        let dir = std::env::temp_dir().join(format!("gumicord-account-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        SecretStore::in_dir(dir).unwrap()
    }

    #[test]
    fn account_key_generates_expected_secret_key() {
        let user = AccountKey::new(UserId::from(1001u64), false);
        assert_eq!(user.secret_key(), "account_user_1001");

        let bot = AccountKey::new(UserId::from(2002u64), true);
        assert_eq!(bot.secret_key(), "account_bot_2002");
    }

    #[test]
    fn accounts_remember_and_load_retains_token_kinds() {
        let store = scratch("multi_account");
        let mut index = AccountsIndex::default();

        let user_key = AccountKey::new(UserId::from(1001u64), false);
        let user_tok = Token::new("user_secret_token");
        index
            .remember(&store, user_key, "Alice".to_owned(), &user_tok)
            .unwrap();

        let bot_key = AccountKey::new(UserId::from(2002u64), true);
        let bot_tok = Token::bot("bot_secret_token");
        index
            .remember(&store, bot_key, "BotBob".to_owned(), &bot_tok)
            .unwrap();

        // Loaded index retains both accounts and active is the latest.
        let loaded = AccountsIndex::load(&store).unwrap();
        assert_eq!(loaded.accounts.len(), 2);
        assert_eq!(loaded.active, Some(bot_key));

        // Tokens are restored with their respective kinds.
        let loaded_user = loaded.load_token(&store, user_key).unwrap().unwrap();
        assert_eq!(loaded_user.expose(), "user_secret_token");
        assert!(!loaded_user.is_bot());

        let loaded_bot = loaded.load_token(&store, bot_key).unwrap().unwrap();
        assert_eq!(loaded_bot.expose(), "bot_secret_token");
        assert!(loaded_bot.is_bot());
    }

    #[test]
    fn remember_cleans_up_legacy_tokens_on_migration() {
        let store = scratch("migration");
        store
            .store(LEGACY_USER_TOKEN_KEY, b"old_user_token")
            .unwrap();
        store.store(LEGACY_BOT_TOKEN_KEY, b"old_bot_token").unwrap();

        let mut index = AccountsIndex::default();
        let key = AccountKey::new(UserId::from(42u64), false);
        index
            .remember(
                &store,
                key,
                "MigratedUser".to_owned(),
                &Token::new("new_token"),
            )
            .unwrap();

        // Legacy keys must be cleared.
        assert!(store.load(LEGACY_USER_TOKEN_KEY).unwrap().is_none());
        assert!(store.load(LEGACY_BOT_TOKEN_KEY).unwrap().is_none());

        // Account token exists under the new account key.
        let tok = index.load_token(&store, key).unwrap().unwrap();
        assert_eq!(tok.expose(), "new_token");
    }

    #[test]
    fn removing_account_deletes_secret_and_updates_active() {
        let store = scratch("remove_account");
        let mut index = AccountsIndex::default();

        let a1 = AccountKey::new(UserId::from(1u64), false);
        let a2 = AccountKey::new(UserId::from(2u64), false);
        index
            .remember(&store, a1, "One".to_owned(), &Token::new("t1"))
            .unwrap();
        index
            .remember(&store, a2, "Two".to_owned(), &Token::new("t2"))
            .unwrap();

        assert_eq!(index.active, Some(a2));
        index.remove(&store, a2).unwrap();

        assert_eq!(index.accounts.len(), 1);
        assert_eq!(index.active, Some(a1));
        assert!(index.load_token(&store, a2).unwrap().is_none());
    }
}

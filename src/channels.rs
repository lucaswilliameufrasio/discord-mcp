//! Local channel storage: SQLite metadata and authenticated-encrypted webhooks.
use std::{fmt, path::Path, time::Duration};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305,
    aead::{Aead, Payload},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::{bot::BotChannel, webhook::WebhookUrl};

pub enum Channel {
    Webhook(WebhookUrl),
    Bot(BotChannel),
}

/// An authenticated Discord identity may be authorized independently for each action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelAction {
    Read,
    Send,
}

#[derive(Debug)]
pub enum StoreError {
    InvalidInput,
    NotFound,
    Storage,
    Decryption,
    PermissionDenied,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid channel or encryption key",
            Self::NotFound => "channel not found",
            Self::Storage => "channel storage error",
            Self::Decryption => "unable to decrypt channel credential",
            Self::PermissionDenied => "channel access denied",
        })
    }
}

impl std::error::Error for StoreError {}

pub struct ChannelStore {
    db: Connection,
    cipher: XChaCha20Poly1305,
}

impl ChannelStore {
    /// The key is supplied independently of the database and must be backed up separately.
    pub fn open(path: &Path, key: &[u8; 32]) -> Result<Self, StoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            match options.open(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(StoreError::Storage),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::symlink_metadata(path).map_err(|_| StoreError::Storage)?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(StoreError::Storage);
            }
        }
        let db = Connection::open(path).map_err(|_| StoreError::Storage)?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|_| StoreError::Storage)?;
        db.pragma_update(None, "foreign_keys", "ON")
            .map_err(|_| StoreError::Storage)?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS channels (name TEXT PRIMARY KEY NOT NULL, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL); CREATE TABLE IF NOT EXISTS key_check (id INTEGER PRIMARY KEY CHECK(id=1), nonce BLOB NOT NULL, ciphertext BLOB NOT NULL);")
            .map_err(|_| StoreError::Storage)?;
        let store = Self {
            db,
            cipher: XChaCha20Poly1305::new(key.into()),
        };
        let mut nonce = [0_u8; 24];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Storage)?;
        let ciphertext = store
            .cipher
            .encrypt((&nonce).into(), b"discord-mcp-key-check".as_slice())
            .map_err(|_| StoreError::Storage)?;
        store
            .db
            .execute(
                "INSERT OR IGNORE INTO key_check(id, nonce, ciphertext) VALUES (1, ?1, ?2)",
                params![nonce.as_slice(), ciphertext],
            )
            .map_err(|_| StoreError::Storage)?;
        let (stored_nonce, stored_ciphertext): (Vec<u8>, Vec<u8>) = store
            .db
            .query_row(
                "SELECT nonce, ciphertext FROM key_check WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| StoreError::Storage)?;
        let stored_nonce: &[u8; 24] = stored_nonce
            .as_slice()
            .try_into()
            .map_err(|_| StoreError::Decryption)?;
        let plaintext = store
            .cipher
            .decrypt(stored_nonce.into(), stored_ciphertext.as_slice())
            .map_err(|_| StoreError::Decryption)?;
        if plaintext != b"discord-mcp-key-check" {
            return Err(StoreError::Decryption);
        }
        store
            .db
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS channel_permissions (
            user_id TEXT NOT NULL,
            channel_name TEXT NOT NULL REFERENCES channels(name) ON DELETE CASCADE,
            can_read INTEGER NOT NULL CHECK(can_read IN (0, 1)),
            can_send INTEGER NOT NULL CHECK(can_send IN (0, 1)),
            PRIMARY KEY (user_id, channel_name)
        );",
            )
            .map_err(|_| StoreError::Storage)?;
        store
            .db
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mcp_access_tokens (
            token_hash BLOB PRIMARY KEY NOT NULL,
            user_id TEXT NOT NULL,
            resource TEXT NOT NULL,
            expires_at INTEGER NOT NULL
        );",
            )
            .map_err(|_| StoreError::Storage)?;
        Ok(store)
    }

    pub fn put_webhook(&self, name: &str, url: &str) -> Result<(), StoreError> {
        WebhookUrl::parse(url).map_err(|_| StoreError::InvalidInput)?;
        self.put_secret(name, url)
    }

    pub fn put_bot(&self, name: &str, channel_id: &str, token: &str) -> Result<(), StoreError> {
        let bot = BotChannel::new(channel_id, token).ok_or(StoreError::InvalidInput)?;
        self.put_secret(name, &bot.encode())
    }

    fn put_secret(&self, name: &str, url: &str) -> Result<(), StoreError> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(StoreError::InvalidInput);
        }
        let mut nonce = [0_u8; 24];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Storage)?;
        let encrypted = self
            .cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: url.as_bytes(),
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| StoreError::Storage)?;
        let transaction = self
            .db
            .unchecked_transaction()
            .map_err(|_| StoreError::Storage)?;
        // A name can be repointed to a different Discord resource. Never carry
        // old grants over to a newly configured destination.
        transaction
            .execute(
                "DELETE FROM channel_permissions WHERE channel_name=?1",
                [name],
            )
            .map_err(|_| StoreError::Storage)?;
        transaction.execute("INSERT INTO channels(name, nonce, ciphertext) VALUES (?1, ?2, ?3) ON CONFLICT(name) DO UPDATE SET nonce = excluded.nonce, ciphertext = excluded.ciphertext", params![name, nonce.as_slice(), encrypted]).map_err(|_| StoreError::Storage)?;
        transaction.commit().map_err(|_| StoreError::Storage)?;
        Ok(())
    }

    pub fn get_webhook(&self, name: &str) -> Result<WebhookUrl, StoreError> {
        match self.get_channel(name)? {
            Channel::Webhook(url) => Ok(url),
            Channel::Bot(_) => Err(StoreError::InvalidInput),
        }
    }

    pub fn get_channel(&self, name: &str) -> Result<Channel, StoreError> {
        let record: Option<(Vec<u8>, Vec<u8>)> = self
            .db
            .query_row(
                "SELECT nonce, ciphertext FROM channels WHERE name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| StoreError::Storage)?;
        let (nonce, ciphertext) = record.ok_or(StoreError::NotFound)?;
        let nonce: &[u8; 24] = nonce
            .as_slice()
            .try_into()
            .map_err(|_| StoreError::Decryption)?;
        let plaintext = self
            .cipher
            .decrypt(
                nonce.into(),
                Payload {
                    msg: &ciphertext,
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| StoreError::Decryption)?;
        let value = std::str::from_utf8(&plaintext).map_err(|_| StoreError::Decryption)?;
        if value.starts_with("bot:") {
            return BotChannel::decode(value)
                .map(Channel::Bot)
                .ok_or(StoreError::Decryption);
        }
        WebhookUrl::parse(value)
            .map(Channel::Webhook)
            .map_err(|_| StoreError::Decryption)
    }

    pub fn list(&self) -> Result<Vec<String>, StoreError> {
        let mut query = self
            .db
            .prepare("SELECT name FROM channels ORDER BY name")
            .map_err(|_| StoreError::Storage)?;
        let rows = query
            .query_map([], |row| row.get(0))
            .map_err(|_| StoreError::Storage)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| StoreError::Storage)
    }

    pub fn delete(&self, name: &str) -> Result<(), StoreError> {
        let count = self
            .db
            .execute("DELETE FROM channels WHERE name = ?1", [name])
            .map_err(|_| StoreError::Storage)?;
        if count == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// This persists policy only; the caller must authenticate and authorize the administrator.
    pub fn set_permissions(
        &self,
        user_id: &str,
        channel: &str,
        can_read: bool,
        can_send: bool,
    ) -> Result<(), StoreError> {
        if !valid_user_id(user_id) {
            return Err(StoreError::InvalidInput);
        }
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|_| StoreError::Storage)?;
        if can_read && matches!(self.get_channel(channel)?, Channel::Webhook(_)) {
            return Err(StoreError::InvalidInput);
        }
        if !can_read && !can_send {
            transaction
                .execute(
                    "DELETE FROM channel_permissions WHERE user_id=?1 AND channel_name=?2",
                    params![user_id, channel],
                )
                .map_err(|_| StoreError::Storage)?;
            return transaction.commit().map_err(|_| StoreError::Storage);
        }
        let changed = transaction.execute(
            "INSERT INTO channel_permissions (user_id, channel_name, can_read, can_send)
             SELECT ?1, name, ?3, ?4 FROM channels WHERE name=?2
             ON CONFLICT(user_id, channel_name) DO UPDATE SET can_read=excluded.can_read, can_send=excluded.can_send",
            params![user_id, channel, can_read, can_send],
        ).map_err(|_| StoreError::Storage)?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        transaction.commit().map_err(|_| StoreError::Storage)
    }

    /// Call on every operation using a user ID established by the authentication layer.
    pub fn is_permitted(
        &self,
        user_id: &str,
        channel: &str,
        action: ChannelAction,
    ) -> Result<bool, StoreError> {
        if !valid_user_id(user_id) {
            return Ok(false);
        }
        let permission: Option<(bool, bool)> = self.db.query_row(
            "SELECT can_read, can_send FROM channel_permissions WHERE user_id=?1 AND channel_name=?2",
            params![user_id, channel], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|_| StoreError::Storage)?;
        let granted = match (permission, action) {
            (Some((read, _)), ChannelAction::Read) => read,
            (Some((_, send)), ChannelAction::Send) => send,
            (None, _) => false,
        };
        if granted && action == ChannelAction::Read {
            return Ok(matches!(self.get_channel(channel)?, Channel::Bot(_)));
        }
        Ok(granted)
    }

    /// Resolve authorization and the destination from the same database snapshot.
    /// Identity must be established by the server, never supplied by an MCP tool.
    pub fn authorized_channel(
        &self,
        user_id: &str,
        channel: &str,
        action: ChannelAction,
    ) -> Result<Channel, StoreError> {
        let transaction = self
            .db
            .unchecked_transaction()
            .map_err(|_| StoreError::Storage)?;
        if !self.is_permitted(user_id, channel, action)? {
            return Err(StoreError::PermissionDenied);
        }
        let destination = self.get_channel(channel)?;
        transaction.commit().map_err(|_| StoreError::Storage)?;
        Ok(destination)
    }

    /// Issue only after identity and consent have been verified by the authorization server.
    /// Resource must come from trusted server configuration, not directly from a request.
    pub fn issue_access_token(
        &self,
        user_id: &str,
        resource: &str,
        now: i64,
        lifetime_seconds: i64,
    ) -> Result<String, StoreError> {
        if !valid_user_id(user_id)
            || !valid_resource(resource)
            || now < 0
            || !(1..=3600).contains(&lifetime_seconds)
        {
            return Err(StoreError::InvalidInput);
        }
        let expires_at = now
            .checked_add(lifetime_seconds)
            .ok_or(StoreError::InvalidInput)?;
        let mut token = [0u8; 32];
        getrandom::fill(&mut token).map_err(|_| StoreError::Storage)?;
        let encoded = hex::encode(token);
        let hash = Sha256::digest(encoded.as_bytes());
        self.db.execute("INSERT INTO mcp_access_tokens(token_hash,user_id,resource,expires_at) VALUES (?1,?2,?3,?4)", params![hash.as_slice(), user_id, resource, expires_at]).map_err(|_| StoreError::Storage)?;
        Ok(encoded)
    }

    /// Validate expiry and exact intended resource on every request.
    pub fn verify_access_token(
        &self,
        token: &str,
        resource: &str,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        if !valid_token(token) || !valid_resource(resource) || now < 0 {
            return Ok(None);
        }
        let hash = Sha256::digest(token.as_bytes());
        self.db.query_row("SELECT user_id FROM mcp_access_tokens WHERE token_hash=?1 AND resource=?2 AND expires_at>?3", params![hash.as_slice(), resource, now], |row| row.get(0)).optional().map_err(|_| StoreError::Storage)
    }

    pub fn revoke_access_token(&self, token: &str) -> Result<(), StoreError> {
        if !valid_token(token) {
            return Ok(());
        }
        let hash = Sha256::digest(token.as_bytes());
        self.db
            .execute(
                "DELETE FROM mcp_access_tokens WHERE token_hash=?1",
                [hash.as_slice()],
            )
            .map_err(|_| StoreError::Storage)?;
        Ok(())
    }
}

fn valid_user_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit())
}

fn valid_token(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_resource(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let local = url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("[::1]")
        );
    (url.scheme() == "https" || local)
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str() == value
}

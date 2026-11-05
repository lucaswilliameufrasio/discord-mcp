//! Local channel storage: SQLite metadata and authenticated-encrypted webhooks.
use std::{fmt, path::Path, time::Duration};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305,
    aead::{Aead, Payload},
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{bot::BotChannel, webhook::WebhookUrl};

pub enum Channel {
    Webhook(WebhookUrl),
    Bot(BotChannel),
}

#[derive(Debug)]
pub enum StoreError {
    InvalidInput,
    NotFound,
    Storage,
    Decryption,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid channel or encryption key",
            Self::NotFound => "channel not found",
            Self::Storage => "channel storage error",
            Self::Decryption => "unable to decrypt channel credential",
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
        self.db.execute("INSERT INTO channels(name, nonce, ciphertext) VALUES (?1, ?2, ?3) ON CONFLICT(name) DO UPDATE SET nonce = excluded.nonce, ciphertext = excluded.ciphertext", params![name, nonce.as_slice(), encrypted]).map_err(|_| StoreError::Storage)?;
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
}

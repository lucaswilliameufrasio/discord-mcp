//! Local channel storage: SQLite metadata and authenticated-encrypted webhooks.
use std::{fmt, path::Path, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305,
    aead::{Aead, Payload},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

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

pub struct ChannelPermission {
    pub user_id: String,
    pub can_read: bool,
    pub can_send: bool,
}

#[derive(Clone, Copy)]
pub enum AuditActor<'a> {
    LocalOperator,
    Discord(&'a str),
    Unknown,
}

pub struct AuditContext<'a> {
    pub actor: AuditActor<'a>,
    pub now: i64,
    pub retention_days: u16,
}

#[derive(Clone, Copy)]
pub enum LoginKind {
    Admin,
    Mcp,
}

pub struct AuditRow {
    pub id: i64,
    pub occurred_at: i64,
    pub event_type: String,
    pub outcome: String,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub channel: Option<String>,
    pub subject_user_id: Option<String>,
    pub can_read: Option<bool>,
    pub can_send: Option<bool>,
}

/// Point-in-time database counts; not a measure of live connections or delivery health.
#[derive(Debug, PartialEq, Eq)]
pub struct Overview {
    pub channels: i64,
    pub permitted_users: i64,
    pub read_grants: i64,
    pub send_grants: i64,
    pub unexpired_tokens: i64,
}

/// Fixed aggregate types. No caller-supplied labels or payloads are stored.
pub enum OperationalEvent {
    Http { failed: bool, response_ms: u64 },
    Send { failed: bool },
    Read { failed: bool },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct OperationalSummary {
    pub buckets: i64,
    pub http_requests: i64,
    pub http_failures: i64,
    pub response_ms_sum: i64,
    pub send_completed: i64,
    pub send_failures: i64,
    pub read_completed: i64,
    pub read_failures: i64,
}

/// Inputs already validated against a registered client and a consented login.
pub struct AuthorizationGrant<'a> {
    pub user_id: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub resource: &'a str,
    pub code_challenge: &'a str,
}

pub struct AuthorizationExchange<'a> {
    pub code: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub resource: &'a str,
    pub code_verifier: &'a str,
}

#[derive(Debug)]
pub enum StoreError {
    InvalidInput,
    NotFound,
    Storage,
    Decryption,
    PermissionDenied,
    AuthorizationDenied,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid channel or encryption key",
            Self::NotFound => "channel not found",
            Self::Storage => "channel storage error",
            Self::Decryption => "unable to decrypt channel credential",
            Self::PermissionDenied => "channel access denied",
            Self::AuthorizationDenied => "authorization code invalid or expired",
        })
    }
}

impl std::error::Error for StoreError {}

pub struct ChannelStore {
    db: Connection,
    cipher: XChaCha20Poly1305,
    audit_retention_days: u16,
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
            audit_retention_days: 90,
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
                "CREATE TABLE IF NOT EXISTS audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            occurred_at INTEGER NOT NULL,
            event_type TEXT NOT NULL,
            outcome TEXT NOT NULL,
            actor_kind TEXT NOT NULL,
            actor_id TEXT,
            channel_name TEXT,
            subject_user_id TEXT,
            can_read INTEGER,
            can_send INTEGER
        ); CREATE INDEX IF NOT EXISTS audit_events_time ON audit_events(occurred_at);",
            )
            .map_err(|_| StoreError::Storage)?;
        store
            .db
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS operational_metrics (
            minute INTEGER PRIMARY KEY NOT NULL,
            http_requests INTEGER NOT NULL DEFAULT 0,
            http_failures INTEGER NOT NULL DEFAULT 0,
            response_ms_sum INTEGER NOT NULL DEFAULT 0,
            send_completed INTEGER NOT NULL DEFAULT 0,
            send_failures INTEGER NOT NULL DEFAULT 0,
            read_completed INTEGER NOT NULL DEFAULT 0,
            read_failures INTEGER NOT NULL DEFAULT 0
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
        store
            .db
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mcp_authorization_codes (
            code_hash BLOB PRIMARY KEY NOT NULL,
            client_id TEXT NOT NULL,
            redirect_uri TEXT NOT NULL,
            resource TEXT NOT NULL,
            user_id TEXT NOT NULL,
            challenge TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );",
            )
            .map_err(|_| StoreError::Storage)?;
        Ok(store)
    }

    pub fn with_audit_retention(mut self, days: u16) -> Result<Self, StoreError> {
        validate_audit(AuditActor::Unknown, 0, days)?;
        self.audit_retention_days = days;
        Ok(self)
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
        let revoked = transaction
            .execute(
                "DELETE FROM channel_permissions WHERE channel_name=?1",
                [name],
            )
            .map_err(|_| StoreError::Storage)?;
        if revoked > 0 {
            self.append_audit(
                current_timestamp()?,
                self.audit_retention_days,
                "channel_grants_revoked",
                "success",
                AuditActor::LocalOperator,
                Some(name),
                None,
                None,
                None,
            )?;
        }
        transaction.execute("INSERT INTO channels(name, nonce, ciphertext) VALUES (?1, ?2, ?3) ON CONFLICT(name) DO UPDATE SET nonce = excluded.nonce, ciphertext = excluded.ciphertext", params![name, nonce.as_slice(), encrypted]).map_err(|_| StoreError::Storage)?;
        self.append_audit(
            current_timestamp()?,
            self.audit_retention_days,
            "channel_configured",
            "success",
            AuditActor::LocalOperator,
            Some(name),
            None,
            None,
            None,
        )?;
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
        let transaction = self
            .db
            .unchecked_transaction()
            .map_err(|_| StoreError::Storage)?;
        let grants: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM channel_permissions WHERE channel_name=?1",
                [name],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::Storage)?;
        let count = transaction
            .execute("DELETE FROM channels WHERE name = ?1", [name])
            .map_err(|_| StoreError::Storage)?;
        if count == 0 {
            return Err(StoreError::NotFound);
        }
        if grants > 0 {
            self.append_audit(
                current_timestamp()?,
                self.audit_retention_days,
                "channel_grants_revoked",
                "success",
                AuditActor::LocalOperator,
                Some(name),
                None,
                None,
                None,
            )?;
        }
        self.append_audit(
            current_timestamp()?,
            self.audit_retention_days,
            "channel_deleted",
            "success",
            AuditActor::LocalOperator,
            Some(name),
            None,
            None,
            None,
        )?;
        transaction.commit().map_err(|_| StoreError::Storage)
    }

    /// This persists policy only; the caller must authenticate and authorize the administrator.
    pub fn set_permissions(
        &self,
        user_id: &str,
        channel: &str,
        can_read: bool,
        can_send: bool,
    ) -> Result<(), StoreError> {
        let now = current_timestamp()?;
        self.set_permissions_audited(
            user_id,
            channel,
            can_read,
            can_send,
            AuditContext {
                actor: AuditActor::LocalOperator,
                now,
                retention_days: self.audit_retention_days,
            },
        )
    }

    /// Call only after authenticating and authorizing the administrator.
    pub fn set_permissions_audited(
        &self,
        user_id: &str,
        channel: &str,
        can_read: bool,
        can_send: bool,
        context: AuditContext<'_>,
    ) -> Result<(), StoreError> {
        if !valid_user_id(user_id) {
            return Err(StoreError::InvalidInput);
        }
        let AuditContext {
            actor,
            now,
            retention_days,
        } = context;
        validate_audit(actor, now, retention_days)?;
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|_| StoreError::Storage)?;
        if can_read && matches!(self.get_channel(channel)?, Channel::Webhook(_)) {
            return Err(StoreError::InvalidInput);
        }
        let exists: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM channels WHERE name=?1)",
                [channel],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::Storage)?;
        if !exists {
            return Err(StoreError::NotFound);
        }
        if !can_read && !can_send {
            transaction
                .execute(
                    "DELETE FROM channel_permissions WHERE user_id=?1 AND channel_name=?2",
                    params![user_id, channel],
                )
                .map_err(|_| StoreError::Storage)?;
            self.append_audit(
                now,
                retention_days,
                "permission_change",
                "success",
                actor,
                Some(channel),
                Some(user_id),
                Some(can_read),
                Some(can_send),
            )?;
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
        self.append_audit(
            now,
            retention_days,
            "permission_change",
            "success",
            actor,
            Some(channel),
            Some(user_id),
            Some(can_read),
            Some(can_send),
        )?;
        transaction.commit().map_err(|_| StoreError::Storage)
    }

    pub fn record_discord_login(
        &self,
        kind: LoginKind,
        actor: AuditActor<'_>,
        success: bool,
        now: i64,
        retention_days: u16,
    ) -> Result<(), StoreError> {
        validate_audit(actor, now, retention_days)?;
        let event_type = match kind {
            LoginKind::Admin => "admin_login",
            LoginKind::Mcp => "mcp_login",
        };
        let outcome = if success { "success" } else { "denied" };
        let transaction = self
            .db
            .unchecked_transaction()
            .map_err(|_| StoreError::Storage)?;
        self.append_audit(
            now,
            retention_days,
            event_type,
            outcome,
            actor,
            None,
            None,
            None,
            None,
        )?;
        transaction.commit().map_err(|_| StoreError::Storage)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_audit(
        &self,
        now: i64,
        retention_days: u16,
        event_type: &str,
        outcome: &str,
        actor: AuditActor<'_>,
        channel: Option<&str>,
        subject: Option<&str>,
        can_read: Option<bool>,
        can_send: Option<bool>,
    ) -> Result<(), StoreError> {
        let (actor_kind, actor_id) = match actor {
            AuditActor::LocalOperator => ("local_operator", None),
            AuditActor::Discord(id) => ("discord", Some(id)),
            AuditActor::Unknown => ("unknown", None),
        };
        self.db.execute("INSERT INTO audit_events(occurred_at,event_type,outcome,actor_kind,actor_id,channel_name,subject_user_id,can_read,can_send) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![now,event_type,outcome,actor_kind,actor_id,channel,subject,can_read,can_send]).map_err(|_| StoreError::Storage)?;
        self.prune_audit(now, retention_days)
    }

    pub fn prune_audit(&self, now: i64, retention_days: u16) -> Result<(), StoreError> {
        validate_audit(AuditActor::Unknown, now, retention_days)?;
        let cutoff = now.saturating_sub(i64::from(retention_days) * 86400);
        self.db
            .execute("DELETE FROM audit_events WHERE occurred_at < ?1", [cutoff])
            .map_err(|_| StoreError::Storage)?;
        Ok(())
    }

    /// Newest first. Supplying the last ID provides stable backwards pagination.
    pub fn audit_events(
        &self,
        before_id: Option<i64>,
        filter: Option<&str>,
        limit: u8,
    ) -> Result<Vec<AuditRow>, StoreError> {
        if limit == 0 || limit > 100 || before_id.is_some_and(|id| id <= 0) {
            return Err(StoreError::InvalidInput);
        }
        if filter.is_some_and(|value| {
            !matches!(
                value,
                "admin_login"
                    | "mcp_login"
                    | "permission_change"
                    | "channel_grants_revoked"
                    | "channel_configured"
                    | "channel_deleted"
            )
        }) {
            return Err(StoreError::InvalidInput);
        }
        let mut statement = self.db.prepare("SELECT id,occurred_at,event_type,outcome,actor_kind,actor_id,channel_name,subject_user_id,can_read,can_send FROM audit_events WHERE (?1 IS NULL OR id < ?1) AND (?2 IS NULL OR event_type=?2) ORDER BY id DESC LIMIT ?3").map_err(|_| StoreError::Storage)?;
        let rows = statement
            .query_map(params![before_id, filter, limit], |row| {
                Ok(AuditRow {
                    id: row.get(0)?,
                    occurred_at: row.get(1)?,
                    event_type: row.get(2)?,
                    outcome: row.get(3)?,
                    actor_kind: row.get(4)?,
                    actor_id: row.get(5)?,
                    channel: row.get(6)?,
                    subject_user_id: row.get(7)?,
                    can_read: row.get(8)?,
                    can_send: row.get(9)?,
                })
            })
            .map_err(|_| StoreError::Storage)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| StoreError::Storage)
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

    /// Administrative query; callers must restrict this to a trusted operator.
    pub fn list_permissions(&self, channel: &str) -> Result<Vec<ChannelPermission>, StoreError> {
        let exists: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM channels WHERE name=?1)",
                [channel],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::Storage)?;
        if !exists {
            return Err(StoreError::NotFound);
        }
        let mut statement = self.db.prepare("SELECT user_id, can_read, can_send FROM channel_permissions WHERE channel_name=?1 ORDER BY user_id").map_err(|_| StoreError::Storage)?;
        let rows = statement
            .query_map([channel], |row| {
                Ok(ChannelPermission {
                    user_id: row.get(0)?,
                    can_read: row.get(1)?,
                    can_send: row.get(2)?,
                })
            })
            .map_err(|_| StoreError::Storage)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| StoreError::Storage)
    }

    /// One SQLite statement provides a consistent point-in-time view.
    pub fn overview(&self, now: i64) -> Result<Overview, StoreError> {
        if now < 0 {
            return Err(StoreError::InvalidInput);
        }
        self.db
            .query_row(
                "SELECT (SELECT COUNT(*) FROM channels),
                    (SELECT COUNT(DISTINCT user_id) FROM channel_permissions),
                    (SELECT COUNT(*) FROM channel_permissions WHERE can_read=1),
                    (SELECT COUNT(*) FROM channel_permissions WHERE can_send=1),
                    (SELECT COUNT(*) FROM mcp_access_tokens WHERE expires_at>?1)",
                [now],
                |row| {
                    Ok(Overview {
                        channels: row.get(0)?,
                        permitted_users: row.get(1)?,
                        read_grants: row.get(2)?,
                        send_grants: row.get(3)?,
                        unexpired_tokens: row.get(4)?,
                    })
                },
            )
            .map_err(|_| StoreError::Storage)
    }

    /// Aggregates only fixed counters and response-header latency, never request content.
    pub fn record_operational_event(
        &self,
        now: i64,
        retention_days: u16,
        event: OperationalEvent,
    ) -> Result<(), StoreError> {
        if now < 0 || !(1..=365).contains(&retention_days) {
            return Err(StoreError::InvalidInput);
        }
        let minute = now / 60;
        let cutoff = now.saturating_sub(i64::from(retention_days) * 86400) / 60;
        let (requests, failures, latency, sends, send_failures, reads, read_failures) = match event
        {
            OperationalEvent::Http {
                failed,
                response_ms,
            } => (
                1,
                i64::from(failed),
                i64::try_from(response_ms.min(600_000)).map_err(|_| StoreError::InvalidInput)?,
                0,
                0,
                0,
                0,
            ),
            OperationalEvent::Send { failed } => (0, 0, 0, 1, i64::from(failed), 0, 0),
            OperationalEvent::Read { failed } => (0, 0, 0, 0, 0, 1, i64::from(failed)),
        };
        let transaction = self
            .db
            .unchecked_transaction()
            .map_err(|_| StoreError::Storage)?;
        transaction.execute("INSERT INTO operational_metrics(minute,http_requests,http_failures,response_ms_sum,send_completed,send_failures,read_completed,read_failures) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
            ON CONFLICT(minute) DO UPDATE SET http_requests=http_requests+excluded.http_requests,http_failures=http_failures+excluded.http_failures,response_ms_sum=response_ms_sum+excluded.response_ms_sum,send_completed=send_completed+excluded.send_completed,send_failures=send_failures+excluded.send_failures,read_completed=read_completed+excluded.read_completed,read_failures=read_failures+excluded.read_failures", params![minute,requests,failures,latency,sends,send_failures,reads,read_failures]).map_err(|_| StoreError::Storage)?;
        transaction
            .execute(
                "DELETE FROM operational_metrics WHERE minute < ?1",
                [cutoff],
            )
            .map_err(|_| StoreError::Storage)?;
        transaction.commit().map_err(|_| StoreError::Storage)
    }

    /// The requested range is rounded down to minute buckets.
    pub fn operational_summary(
        &self,
        start: i64,
        end: i64,
    ) -> Result<OperationalSummary, StoreError> {
        if start < 0 || end < start {
            return Err(StoreError::InvalidInput);
        }
        self.db.query_row("SELECT COUNT(*),COALESCE(SUM(http_requests),0),COALESCE(SUM(http_failures),0),COALESCE(SUM(response_ms_sum),0),COALESCE(SUM(send_completed),0),COALESCE(SUM(send_failures),0),COALESCE(SUM(read_completed),0),COALESCE(SUM(read_failures),0) FROM operational_metrics WHERE minute>=?1 AND minute<=?2", params![start / 60,end / 60], |row| Ok(OperationalSummary { buckets: row.get(0)?, http_requests: row.get(1)?, http_failures: row.get(2)?, response_ms_sum: row.get(3)?, send_completed: row.get(4)?, send_failures: row.get(5)?, read_completed: row.get(6)?, read_failures: row.get(7)? })).map_err(|_| StoreError::Storage)
    }

    pub fn prune_operational_metrics(
        &self,
        now: i64,
        retention_days: u16,
    ) -> Result<(), StoreError> {
        if now < 0 || !(1..=365).contains(&retention_days) {
            return Err(StoreError::InvalidInput);
        }
        let cutoff = now.saturating_sub(i64::from(retention_days) * 86400) / 60;
        self.db
            .execute(
                "DELETE FROM operational_metrics WHERE minute < ?1",
                [cutoff],
            )
            .map_err(|_| StoreError::Storage)?;
        Ok(())
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

    /// Only call after authenticating the user, validating the registered client
    /// and exact redirect, and receiving explicit consent for this transaction.
    pub fn issue_authorization_code(
        &self,
        grant: &AuthorizationGrant<'_>,
        now: i64,
    ) -> Result<String, StoreError> {
        if now < 0
            || !valid_user_id(grant.user_id)
            || !valid_client_id(grant.client_id)
            || !valid_redirect(grant.redirect_uri)
            || !valid_resource(grant.resource)
            || !valid_challenge(grant.code_challenge)
        {
            return Err(StoreError::InvalidInput);
        }
        let expires_at = now.checked_add(300).ok_or(StoreError::InvalidInput)?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| StoreError::Storage)?;
        let code = hex::encode(random);
        let hash = Sha256::digest(code.as_bytes());
        self.db.execute("INSERT INTO mcp_authorization_codes(code_hash,client_id,redirect_uri,resource,user_id,challenge,created_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![hash.as_slice(),grant.client_id,grant.redirect_uri,grant.resource,grant.user_id,grant.code_challenge,now,expires_at]).map_err(|_| StoreError::Storage)?;
        Ok(code)
    }

    /// Exchange and token issuance commit atomically; failed validation issues nothing.
    pub fn exchange_authorization_code(
        &self,
        exchange: &AuthorizationExchange<'_>,
        now: i64,
    ) -> Result<String, StoreError> {
        if now < 0
            || !valid_token(exchange.code)
            || !valid_client_id(exchange.client_id)
            || !valid_redirect(exchange.redirect_uri)
            || !valid_resource(exchange.resource)
            || !valid_verifier(exchange.code_verifier)
        {
            return Err(StoreError::AuthorizationDenied);
        }
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|_| StoreError::Storage)?;
        let hash = Sha256::digest(exchange.code.as_bytes());
        let record: Option<(String, String)> = transaction.query_row(
            "SELECT user_id, challenge FROM mcp_authorization_codes WHERE code_hash=?1 AND client_id=?2 AND redirect_uri=?3 AND resource=?4 AND created_at<=?5 AND expires_at>?5",
            params![hash.as_slice(), exchange.client_id, exchange.redirect_uri, exchange.resource, now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|_| StoreError::Storage)?;
        let (user_id, challenge) = record.ok_or(StoreError::AuthorizationDenied)?;
        let calculated = URL_SAFE_NO_PAD.encode(Sha256::digest(exchange.code_verifier.as_bytes()));
        if !bool::from(calculated.as_bytes().ct_eq(challenge.as_bytes())) {
            return Err(StoreError::AuthorizationDenied);
        }
        let changed = transaction
            .execute(
                "DELETE FROM mcp_authorization_codes WHERE code_hash=?1",
                [hash.as_slice()],
            )
            .map_err(|_| StoreError::Storage)?;
        if changed != 1 {
            return Err(StoreError::AuthorizationDenied);
        }
        let access_token = self.issue_access_token(&user_id, exchange.resource, now, 3600)?;
        transaction.commit().map_err(|_| StoreError::Storage)?;
        Ok(access_token)
    }
}

fn valid_user_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit())
}

pub fn current_timestamp() -> Result<i64, StoreError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
        .ok_or(StoreError::Storage)
}

fn validate_audit(actor: AuditActor<'_>, now: i64, retention_days: u16) -> Result<(), StoreError> {
    if now < 0 || !(1..=3650).contains(&retention_days) {
        return Err(StoreError::InvalidInput);
    }
    if let AuditActor::Discord(id) = actor
        && !valid_user_id(id)
    {
        return Err(StoreError::InvalidInput);
    }
    Ok(())
}

fn valid_token(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn valid_resource(value: &str) -> bool {
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

fn valid_client_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && value.bytes().all(|b| b.is_ascii_graphic())
}

pub(crate) fn valid_redirect(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let loopback = url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]")
        );
    (url.scheme() == "https" || loopback)
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.as_str() == value
}

fn valid_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

pub(crate) fn valid_challenge(value: &str) -> bool {
    if value.len() != 43 {
        return false;
    }
    match URL_SAFE_NO_PAD.decode(value) {
        Ok(bytes) => bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == value,
        Err(_) => false,
    }
}

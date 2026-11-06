//! Prepare an already restored, offline SQLite image in a new quarantined file.
//! This module does not fetch backups, restore over a live DB, or start the service.
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};

use crate::channels::{ChannelStore, RecoveryInvalidations};

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum RecoveryError {
    InvalidPath,
    SourceInvalid,
    DestinationExists,
    InvalidKeyOrData,
    Timeout,
    Storage,
    PublishedButSyncFailed,
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPath => "source and destination must be in private directories and source must be an offline regular file",
            Self::SourceInvalid => "restored source is invalid, incomplete, too large, or incompatible",
            Self::DestinationExists => "destination or a SQLite sidecar already exists",
            Self::InvalidKeyOrData => "unable to validate the restored database and encryption key",
            Self::Timeout => "recovery preparation exceeded its time budget",
            Self::Storage => "unable to prepare the restored database",
            Self::PublishedButSyncFailed => "quarantined destination was published but directory sync failed; inspect it before retrying",
        })
    }
}

impl std::error::Error for RecoveryError {}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn path_absent(path: &Path) -> Result<(), RecoveryError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(RecoveryError::DestinationExists),
        Err(_) => Err(RecoveryError::Storage),
    }
}

fn private_directory(path: &Path) -> Result<(), RecoveryError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| RecoveryError::InvalidPath)?;
    if !metadata.is_dir() {
        return Err(RecoveryError::InvalidPath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RecoveryError::InvalidPath);
        }
    }
    Ok(())
}

fn parent(path: &Path) -> Result<&Path, RecoveryError> {
    path.parent()
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(RecoveryError::InvalidPath)
}

fn elapsed(start: Instant) -> Result<(), RecoveryError> {
    if start.elapsed() > MAX_DURATION {
        return Err(RecoveryError::Timeout);
    }
    Ok(())
}

/// Copies an offline image to a new path, verifies it, and invalidates old grants,
/// access tokens and authorization codes before publishing it in quarantine.
pub fn prepare_restored_copy(
    source: &Path,
    destination: &Path,
    key: &[u8; 32],
    retention_days: u16,
) -> Result<RecoveryInvalidations, RecoveryError> {
    prepare_restored_copy_inner(
        source,
        destination,
        key,
        retention_days,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::channels::{AuthorizationGrant, ChannelAction};
    use std::cell::Cell;

    #[test]
    fn quarantine_audit_failure_never_publishes_and_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let source = dir.path().join("source.db");
        let destination = dir.path().join("restored.db");
        let key = [154_u8; 32];
        let resource = "https://example.invalid/mcp";
        let store = ChannelStore::open(&source, &key).unwrap();
        store.put_bot("alerts", "123", "synthetic-token").unwrap();
        store.set_permissions("101", "alerts", true, false).unwrap();
        let token = store.issue_access_token("101", resource, 1000, 60).unwrap();
        let grant = AuthorizationGrant {
            user_id: "101",
            client_id: "client",
            redirect_uri: "http://127.0.0.1/callback",
            resource,
            code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        };
        store.issue_authorization_code(&grant, 1000).unwrap();
        drop(store);
        let baseline = fs::read(&source).unwrap();
        let injected = Cell::new(false);
        let hook = |staged: &Path| {
            let db = Connection::open(staged).unwrap();
            db.execute_batch("CREATE TRIGGER synthetic_fail_quarantine_audit AFTER INSERT ON audit_events WHEN NEW.event_type='recovery_quarantined' BEGIN SELECT RAISE(ABORT, 'synthetic audit failure'); END;").unwrap();
            injected.set(true);
        };
        assert!(matches!(
            prepare_restored_copy_inner(&source, &destination, &key, 90, Some(&hook)),
            Err(RecoveryError::Storage)
        ));
        assert!(injected.get());
        assert!(!destination.exists());
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!sidecar(&destination, suffix).exists());
        }
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".discord-mcp-recovery-")
        }));
        assert_eq!(fs::read(&source).unwrap(), baseline);
        let source_store = ChannelStore::open(&source, &key).unwrap();
        assert!(
            source_store
                .is_permitted("101", "alerts", ChannelAction::Read)
                .unwrap()
        );
        assert_eq!(
            source_store
                .verify_access_token(&token, resource, 1001)
                .unwrap()
                .as_deref(),
            Some("101")
        );
        drop(source_store);
        let report = prepare_restored_copy(&source, &destination, &key, 90).unwrap();
        assert_eq!((report.permissions, report.tokens, report.codes), (1, 1, 1));
        let restored = ChannelStore::open(&destination, &key).unwrap();
        assert!(restored.recovery_quarantined().unwrap());
        assert_eq!(
            restored
                .verify_access_token(&token, resource, 1001)
                .unwrap(),
            None
        );
    }
}

fn prepare_restored_copy_inner(
    source: &Path,
    destination: &Path,
    key: &[u8; 32],
    retention_days: u16,
    #[cfg(test)] before_quarantine: Option<&dyn Fn(&Path)>,
) -> Result<RecoveryInvalidations, RecoveryError> {
    let start = Instant::now();
    private_directory(parent(source)?)?;
    private_directory(parent(destination)?)?;
    let metadata = fs::symlink_metadata(source).map_err(|_| RecoveryError::InvalidPath)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BYTES {
        return Err(RecoveryError::InvalidPath);
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        match fs::symlink_metadata(sidecar(source, suffix)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(RecoveryError::InvalidPath),
        }
    }
    path_absent(destination)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        path_absent(&sidecar(destination, suffix))?;
    }
    let canonical_source = fs::canonicalize(source).map_err(|_| RecoveryError::InvalidPath)?;
    let mut source_uri =
        reqwest::Url::from_file_path(canonical_source).map_err(|_| RecoveryError::InvalidPath)?;
    source_uri.query_pairs_mut().append_pair("immutable", "1");
    let source_db = Connection::open_with_flags(
        source_uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| RecoveryError::SourceInvalid)?;
    source_db
        .execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
        .map_err(|_| RecoveryError::SourceInvalid)?;
    let unexpected_schema: i64 = source_db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type IN ('trigger', 'view')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| RecoveryError::SourceInvalid)?;
    if unexpected_schema != 0 {
        return Err(RecoveryError::SourceInvalid);
    }
    let integrity: String = source_db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|_| RecoveryError::SourceInvalid)?;
    if integrity != "ok" {
        return Err(RecoveryError::SourceInvalid);
    }
    // Reject incomplete/foreign databases before ChannelStore can create missing tables.
    for statement in [
        "SELECT name,nonce,ciphertext FROM channels LIMIT 0",
        "SELECT id,nonce,ciphertext FROM key_check LIMIT 0",
        "SELECT user_id,channel_name,can_read,can_send FROM channel_permissions LIMIT 0",
        "SELECT token_hash,user_id,resource,expires_at FROM mcp_access_tokens LIMIT 0",
        "SELECT code_hash,client_id,redirect_uri,resource,user_id,challenge,created_at,expires_at FROM mcp_authorization_codes LIMIT 0",
        "SELECT id,occurred_at,event_type,outcome,actor_kind FROM audit_events LIMIT 0",
        "SELECT minute,http_requests FROM operational_metrics LIMIT 0",
    ] {
        source_db
            .prepare(statement)
            .map_err(|_| RecoveryError::SourceInvalid)?;
    }
    let key_rows: i64 = source_db
        .query_row("SELECT COUNT(*) FROM key_check WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(|_| RecoveryError::SourceInvalid)?;
    if key_rows != 1 {
        return Err(RecoveryError::SourceInvalid);
    }
    let page_count: i64 = source_db
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .map_err(|_| RecoveryError::SourceInvalid)?;
    let page_size: i64 = source_db
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .map_err(|_| RecoveryError::SourceInvalid)?;
    if page_count <= 0
        || page_size <= 0
        || page_count
            .checked_mul(page_size)
            .is_none_or(|size| size > MAX_BYTES as i64)
    {
        return Err(RecoveryError::SourceInvalid);
    }
    elapsed(start)?;
    // Contain SQLite's journal/WAL sidecars in a private directory so failure
    // paths remove them together with the staging database.
    let staging = tempfile::Builder::new()
        .prefix(".discord-mcp-recovery-")
        .tempdir_in(parent(destination)?)
        .map_err(|_| RecoveryError::Storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| RecoveryError::Storage)?;
    }
    let temporary = tempfile::Builder::new()
        .prefix("staged-")
        .tempfile_in(staging.path())
        .map_err(|_| RecoveryError::Storage)?;
    {
        let mut output_db =
            Connection::open(temporary.path()).map_err(|_| RecoveryError::Storage)?;
        {
            let backup =
                Backup::new(&source_db, &mut output_db).map_err(|_| RecoveryError::Storage)?;
            loop {
                elapsed(start)?;
                match backup.step(128).map_err(|_| RecoveryError::Storage)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    _ => return Err(RecoveryError::Storage),
                }
            }
        }
    }
    drop(source_db);
    let source_after = fs::symlink_metadata(source).map_err(|_| RecoveryError::InvalidPath)?;
    if !source_after.is_file()
        || source_after.len() != metadata.len()
        || source_after.modified().ok() != metadata.modified().ok()
    {
        return Err(RecoveryError::SourceInvalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if source_after.dev() != metadata.dev() || source_after.ino() != metadata.ino() {
            return Err(RecoveryError::SourceInvalid);
        }
    }
    elapsed(start)?;
    let staged = ChannelStore::open(temporary.path(), key)
        .and_then(|store| store.with_audit_retention(retention_days))
        .map_err(|_| RecoveryError::InvalidKeyOrData)?;
    for name in staged.list().map_err(|_| RecoveryError::InvalidKeyOrData)? {
        staged
            .get_channel(&name)
            .map_err(|_| RecoveryError::InvalidKeyOrData)?;
    }
    #[cfg(test)]
    if let Some(inject) = before_quarantine {
        inject(temporary.path());
    }
    let invalidated = staged
        .quarantine_recovered_copy()
        .map_err(|_| RecoveryError::Storage)?;
    drop(staged);
    {
        let connection = Connection::open(temporary.path()).map_err(|_| RecoveryError::Storage)?;
        let (busy, _, _): (i64, i64, i64) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|_| RecoveryError::Storage)?;
        if busy != 0 {
            return Err(RecoveryError::Storage);
        }
        let journal: String = connection
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .map_err(|_| RecoveryError::Storage)?;
        if journal != "delete" {
            return Err(RecoveryError::Storage);
        }
    }
    elapsed(start)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        path_absent(&sidecar(destination, suffix))?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| RecoveryError::Storage)?;
    temporary.persist_noclobber(destination).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            RecoveryError::DestinationExists
        } else {
            RecoveryError::Storage
        }
    })?;
    #[cfg(unix)]
    fs::File::open(parent(destination)?)
        .and_then(|file| file.sync_all())
        .map_err(|_| RecoveryError::PublishedButSyncFailed)?;
    Ok(invalidated)
}

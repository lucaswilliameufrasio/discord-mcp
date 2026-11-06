//! Experimental explicit backup API. Call only with trusted private paths and
//! a separately provisioned S3 client and age recovery identity.
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use age::x25519::{Identity, Recipient};
use aws_sdk_s3::Client;
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use tokio::io::AsyncReadExt;

use crate::{channels::RecoveryInvalidations, recovery::prepare_restored_copy};

const MAX_SNAPSHOT: u64 = 64 * 1024 * 1024;
const MAX_OBJECT: u64 = MAX_SNAPSHOT + 1024 * 1024;

#[derive(Debug)]
pub struct BackupError;

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("encrypted backup or restore failed; no sensitive details are exposed")
    }
}

impl std::error::Error for BackupError {}

/// An executor intended to be owned by the service, not by remote clients.
/// It does not authenticate callers or provide a durable job queue.
pub struct ServiceBackup {
    client: Client,
    bucket: String,
    source: PathBuf,
    staging_parent: PathBuf,
    recipient: Recipient,
    admission: tokio::sync::Mutex<()>,
}

impl ServiceBackup {
    pub fn new(
        client: Client,
        bucket: String,
        source: PathBuf,
        staging_parent: PathBuf,
        recipient: Recipient,
    ) -> Result<Self, BackupError> {
        if bucket.is_empty() || bucket.len() > 63 {
            return Err(BackupError);
        }
        private_dir(&staging_parent)?;
        Ok(Self {
            client,
            bucket,
            source,
            staging_parent,
            recipient,
            admission: tokio::sync::Mutex::new(()),
        })
    }

    /// The ID selects only a new object under the fixed server-side prefix.
    /// Cancellation or lost responses can leave the remote outcome unknown.
    pub async fn snapshot(&self, id: &str) -> Result<(), BackupError> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(BackupError);
        }
        let _permit = self.admission.try_lock().map_err(|_| BackupError)?;
        let object = format!("service-backups/{id}.age");
        upload_snapshot(
            &self.client,
            &self.bucket,
            &object,
            &self.source,
            &self.staging_parent,
            &self.recipient,
        )
        .await
    }
}

fn private_dir(path: &Path) -> Result<(), BackupError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| BackupError)?;
    if !metadata.is_dir() {
        return Err(BackupError);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(BackupError);
        }
    }
    Ok(())
}

/// Online SQLite snapshot followed by age encryption and a conditional S3 PUT.
/// The private staging directory may contain plaintext until cleanup, including
/// after a crash. S3 providers must implement If-None-Match correctly.
pub async fn upload_snapshot(
    client: &Client,
    bucket: &str,
    object_key: &str,
    source: &Path,
    staging_parent: &Path,
    recipient: &Recipient,
) -> Result<(), BackupError> {
    private_dir(staging_parent)?;
    if !fs::symlink_metadata(source)
        .map_err(|_| BackupError)?
        .is_file()
    {
        return Err(BackupError);
    }
    let staging = tempfile::Builder::new()
        .prefix(".discord-mcp-backup-")
        .tempdir_in(staging_parent)
        .map_err(|_| BackupError)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| BackupError)?;
    }
    let snapshot = staging.path().join("snapshot.db");
    let source_db = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| BackupError)?;
    let start = Instant::now();
    {
        let mut target = Connection::open(&snapshot).map_err(|_| BackupError)?;
        let backup = Backup::new(&source_db, &mut target).map_err(|_| BackupError)?;
        loop {
            if start.elapsed() > Duration::from_secs(30) {
                return Err(BackupError);
            }
            match backup.step(128).map_err(|_| BackupError)? {
                StepResult::Done => break,
                StepResult::More => {}
                _ => return Err(BackupError),
            }
            if fs::metadata(&snapshot).map_err(|_| BackupError)?.len() > MAX_SNAPSHOT {
                return Err(BackupError);
            }
        }
    }
    let mut data = Vec::new();
    fs::File::open(&snapshot)
        .map_err(|_| BackupError)?
        .take(MAX_SNAPSHOT + 1)
        .read_to_end(&mut data)
        .map_err(|_| BackupError)?;
    if data.is_empty() || data.len() as u64 > MAX_SNAPSHOT {
        return Err(BackupError);
    }
    let encryptor = age::Encryptor::with_recipients(std::iter::once(recipient as _))
        .map_err(|_| BackupError)?;
    let mut encrypted = Vec::new();
    {
        let mut writer = encryptor
            .wrap_output(&mut encrypted)
            .map_err(|_| BackupError)?;
        writer.write_all(&data).map_err(|_| BackupError)?;
        writer.finish().map_err(|_| BackupError)?;
    }
    if encrypted.len() as u64 > MAX_OBJECT {
        return Err(BackupError);
    }
    drop(data);
    drop(staging);
    client
        .put_object()
        .bucket(bucket)
        .key(object_key)
        .if_none_match("*")
        .body(encrypted.into())
        .send()
        .await
        .map_err(|_| BackupError)?;
    Ok(())
}

/// Download, authenticate and prepare a new quarantined database. The
/// destination parent must be private. No existing destination is overwritten.
pub async fn restore_quarantined(
    client: &Client,
    bucket: &str,
    object_key: &str,
    destination: &Path,
    identity: &Identity,
    application_key: &[u8; 32],
    retention_days: u16,
) -> Result<RecoveryInvalidations, BackupError> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(BackupError)?;
    private_dir(parent)?;
    let object = client
        .get_object()
        .bucket(bucket)
        .key(object_key)
        .send()
        .await
        .map_err(|_| BackupError)?;
    if object
        .content_length()
        .is_some_and(|n| n < 0 || n as u64 > MAX_OBJECT)
    {
        return Err(BackupError);
    }
    let mut encrypted = Vec::new();
    object
        .body
        .into_async_read()
        .take(MAX_OBJECT + 1)
        .read_to_end(&mut encrypted)
        .await
        .map_err(|_| BackupError)?;
    if encrypted.len() as u64 > MAX_OBJECT {
        return Err(BackupError);
    }
    let decryptor = age::Decryptor::new(encrypted.as_slice()).map_err(|_| BackupError)?;
    let reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|_| BackupError)?;
    let mut data = Vec::new();
    reader
        .take(MAX_SNAPSHOT + 1)
        .read_to_end(&mut data)
        .map_err(|_| BackupError)?;
    if data.is_empty() || data.len() as u64 > MAX_SNAPSHOT {
        return Err(BackupError);
    }
    let staging = tempfile::Builder::new()
        .prefix(".discord-mcp-decrypt-")
        .tempdir_in(parent)
        .map_err(|_| BackupError)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| BackupError)?;
    }
    let source = staging.path().join("snapshot.db");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&source)
        .map_err(|_| BackupError)?;
    file.write_all(&data).map_err(|_| BackupError)?;
    drop(file);
    prepare_restored_copy(&source, destination, application_key, retention_days)
        .map_err(|_| BackupError)
}

#[cfg(test)]
mod service_tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_submission_is_rejected_without_queueing() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test", "test", None, None, "fixture",
            ))
            .build();
        let service = ServiceBackup::new(
            Client::from_conf(config),
            "fixture-bucket".into(),
            directory.path().join("absent.db"),
            directory.path().into(),
            Identity::generate().to_public(),
        )
        .unwrap();
        let guard = service.admission.lock().await;
        assert!(service.snapshot("second").await.is_err());
        drop(guard);
        assert!(service.admission.try_lock().is_ok());
    }
}

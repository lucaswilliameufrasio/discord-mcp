//! Experimental explicit backup API. Call only with trusted private paths and
//! a separately provisioned S3 client and age recovery identity.
use std::{
    fmt, fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr},
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
pub const MAX_ENCRYPTED_OBJECT: u64 = MAX_SNAPSHOT + 1024 * 1024;
const MAX_OBJECT: u64 = MAX_ENCRYPTED_OBJECT;

#[derive(Debug)]
pub struct BackupError;

#[derive(Debug, PartialEq, Eq)]
pub enum ObjectObservation {
    Found,
    NotFound,
    Unavailable,
}

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
    binding: [u8; 32],
    admission: tokio::sync::Mutex<()>,
}

impl ServiceBackup {
    /// Only ciphertext is returned; existence is not provenance or integrity.
    pub async fn fetch_ciphertext(&self, id: &str) -> Result<Vec<u8>, BackupError> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(BackupError);
        }
        let _permit = self.admission.try_lock().map_err(|_| BackupError)?;
        let object = tokio::time::timeout(
            Duration::from_secs(30),
            self.client
                .get_object()
                .bucket(&self.bucket)
                .key(format!("service-backups/{id}.age"))
                .send(),
        )
        .await
        .map_err(|_| BackupError)?
        .map_err(|_| BackupError)?;
        if object
            .content_length()
            .is_some_and(|n| n < 0 || n as u64 > MAX_OBJECT)
        {
            return Err(BackupError);
        }
        let mut encrypted = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(30),
            object
                .body
                .into_async_read()
                .take(MAX_OBJECT + 1)
                .read_to_end(&mut encrypted),
        )
        .await
        .map_err(|_| BackupError)?
        .map_err(|_| BackupError)?;
        if encrypted.is_empty() || encrypted.len() as u64 > MAX_OBJECT {
            return Err(BackupError);
        }
        Ok(encrypted)
    }
    pub fn destination_binding(&self) -> [u8; 32] {
        self.binding
    }
    /// Observe only the currently configured bucket; this does not verify the
    /// object, bind it to an attempt, or prove that a pending PUT has finished.
    pub async fn observe(&self, id: &str) -> ObjectObservation {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return ObjectObservation::Unavailable;
        }
        let Ok(_permit) = self.admission.try_lock() else {
            return ObjectObservation::Unavailable;
        };
        let request = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(format!("service-backups/{id}.age"))
            .send();
        match tokio::time::timeout(Duration::from_secs(10), request).await {
            Ok(Ok(_)) => ObjectObservation::Found,
            Ok(Err(error)) if error.as_service_error().is_some_and(|e| e.is_not_found()) => {
                ObjectObservation::NotFound
            }
            _ => ObjectObservation::Unavailable,
        }
    }
    pub fn destination_description(&self) -> (String, String) {
        (self.bucket.clone(), self.recipient.to_string())
    }
    pub fn new_floci(
        address: SocketAddr,
        bucket: String,
        source: PathBuf,
        staging_parent: PathBuf,
        recipient: Recipient,
    ) -> Result<Self, BackupError> {
        if address.ip() != Ipv4Addr::LOCALHOST
            || address.port() == 0
            || bucket.is_empty()
            || bucket.len() > 63
        {
            return Err(BackupError);
        }
        private_dir(&staging_parent)?;
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"discord-mcp-floci-target-v1\0");
        for value in [address.to_string(), bucket.clone(), recipient.to_string()] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        let binding = hash.finalize().into();
        let config = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url(format!("http://{address}"))
            .force_path_style(true)
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "local-floci",
            ))
            .build();
        Ok(Self {
            client: Client::from_conf(config),
            bucket,
            source,
            staging_parent,
            recipient,
            binding,
            admission: tokio::sync::Mutex::new(()),
        })
    }

    /// The ID selects only a new object under the fixed server-side prefix.
    /// Cancellation or lost responses can leave the remote outcome unknown.
    pub async fn snapshot(&self, id: &str) -> Result<(), BackupError> {
        self.snapshot_with_commitment(id, |_, _| Ok(())).await
    }

    /// Persist the ciphertext commitment before any PUT. If persistence fails,
    /// no external write is attempted; the callback must be durable and atomic.
    pub async fn snapshot_with_commitment<F>(&self, id: &str, commit: F) -> Result<(), BackupError>
    where
        F: FnOnce(&[u8; 32], u64) -> Result<(), BackupError>,
    {
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
        let encrypted = prepare_snapshot(&self.source, &self.staging_parent, &self.recipient)?;
        use sha2::{Digest, Sha256};
        let digest: [u8; 32] = Sha256::digest(&encrypted).into();
        commit(&digest, encrypted.len() as u64)?;
        put_snapshot(&self.client, &self.bucket, &object, encrypted).await
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
    let encrypted = prepare_snapshot(source, staging_parent, recipient)?;
    put_snapshot(client, bucket, object_key, encrypted).await
}

fn prepare_snapshot(
    source: &Path,
    staging_parent: &Path,
    recipient: &Recipient,
) -> Result<Vec<u8>, BackupError> {
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
    Ok(encrypted)
}

async fn put_snapshot(
    client: &Client,
    bucket: &str,
    object_key: &str,
    encrypted: Vec<u8>,
) -> Result<(), BackupError> {
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
    restore_ciphertext_quarantined(
        &encrypted,
        destination,
        identity,
        application_key,
        retention_days,
    )
}

/// Authenticate a bounded ciphertext and prepare a fresh quarantined copy.
pub fn restore_ciphertext_quarantined(
    encrypted: &[u8],
    destination: &Path,
    identity: &Identity,
    application_key: &[u8; 32],
    retention_days: u16,
) -> Result<RecoveryInvalidations, BackupError> {
    if encrypted.is_empty() || encrypted.len() as u64 > MAX_OBJECT {
        return Err(BackupError);
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(BackupError)?;
    private_dir(parent)?;
    let decryptor = age::Decryptor::new(encrypted).map_err(|_| BackupError)?;
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

    #[test]
    fn binding_changes_for_endpoint_bucket_or_recipient() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let recipient = Identity::generate().to_public();
        let make = |address: &str, bucket: &str, recipient: Recipient| {
            ServiceBackup::new_floci(
                address.parse().unwrap(),
                bucket.to_owned(),
                directory.path().join("source.db"),
                directory.path().into(),
                recipient,
            )
            .unwrap()
            .destination_binding()
        };
        let reference = make("127.0.0.1:1234", "fixture-one", recipient.clone());
        assert_eq!(
            reference,
            make("127.0.0.1:1234", "fixture-one", recipient.clone())
        );
        assert_ne!(
            reference,
            make("127.0.0.1:1235", "fixture-one", recipient.clone())
        );
        assert_ne!(reference, make("127.0.0.1:1234", "fixture-two", recipient));
        assert_ne!(
            reference,
            make(
                "127.0.0.1:1234",
                "fixture-one",
                Identity::generate().to_public()
            )
        );
    }

    #[tokio::test]
    async fn concurrent_submission_is_rejected_without_queueing() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let service = ServiceBackup::new_floci(
            "127.0.0.1:1".parse().unwrap(),
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

    #[tokio::test]
    async fn denied_probe_is_unavailable_not_missing() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().fallback(|| async { axum::http::StatusCode::FORBIDDEN }),
            )
            .await
            .unwrap();
        });
        let service = ServiceBackup::new_floci(
            endpoint,
            "fixture-bucket".into(),
            directory.path().join("absent.db"),
            directory.path().into(),
            Identity::generate().to_public(),
        )
        .unwrap();
        assert_eq!(
            service.observe("test").await,
            ObjectObservation::Unavailable
        );
        task.abort();
    }
}

//! Cooperative process separation on Linux, not a security or replication gate.
//! The lock is held on the SQLite inode for the lifetime of the process.

#[cfg(target_os = "linux")]
pub fn acquire(
    path: &std::path::Path,
    exclusive: bool,
    create: bool,
) -> Result<std::fs::File, Box<dyn std::error::Error>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(create).create(create).mode(0o600);
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    if !named.is_file()
        || !opened.is_file()
        || opened.nlink() != 1
        || opened.permissions().mode() & 0o077 != 0
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
    {
        return Err("database must be a private regular file without hard links".into());
    }
    if exclusive {
        file.try_lock()
            .map_err(|_| "database is in use by another application process")?;
    } else {
        file.try_lock_shared()
            .map_err(|_| "database is owned by the remote service")?;
    }
    let current = std::fs::symlink_metadata(path)?;
    if !current.is_file()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
        || current.nlink() != 1
    {
        return Err("database changed while acquiring process lock".into());
    }
    Ok(file)
}

//! Synthetic identity experiment; not used by the application or real gate.
#![cfg(unix)]

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, Command},
    time::timeout,
};

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn matches_identity(path: &Path, file: &File, expected: &str) -> bool {
    let Ok(path_meta) = fs::symlink_metadata(path) else {
        return false;
    };
    let Ok(file_meta) = file.metadata() else {
        return false;
    };
    if !path_meta.file_type().is_file() {
        return false;
    }
    identity(&path_meta) == identity(&file_meta)
        && format!("{}:{}", file_meta.dev(), file_meta.ino()) == expected.trim()
}

#[test]
fn identity_worker() {
    let Some(dir) = std::env::var_os("IDENTITY_FIXTURE_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let path = dir.join("activity.lock");
    let role = std::env::var("IDENTITY_FIXTURE_ROLE").unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    if role == "writer" {
        file.lock_shared().unwrap();
        eprintln!("MARK holding");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "release");
        return;
    }
    let expected = fs::read_to_string(dir.join("identity")).unwrap_or_default();
    if !matches_identity(&path, &file, &expected) {
        eprintln!("MARK mismatch");
        return;
    }
    eprintln!("MARK verified");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "continue");
    if file.try_lock().is_err() {
        eprintln!("MARK busy");
    } else if !matches_identity(&path, &file, &expected) {
        eprintln!("MARK mismatch");
    } else {
        eprintln!("MARK acquired");
        if role == "pause" {
            let mut release = String::new();
            std::io::stdin().read_line(&mut release).unwrap();
            assert_eq!(release.trim(), "release");
        }
    }
}

struct Worker {
    child: Child,
    stderr: BufReader<ChildStderr>,
}

impl Worker {
    fn start(dir: &Path, role: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("IDENTITY_FIXTURE_DIR", dir)
            .env("IDENTITY_FIXTURE_ROLE", role)
            .args(["--exact", "identity_worker", "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        Self { child, stderr }
    }

    async fn marker(&mut self, expected: &str) {
        let mut line = String::new();
        let count = timeout(Duration::from_secs(5), self.stderr.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0);
        assert_eq!(line.trim(), format!("MARK {expected}"));
    }

    async fn send(&mut self, command: &str) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{command}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn finish(&mut self) {
        assert!(
            timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let file = File::create(dir.path().join("activity.lock")).unwrap();
    let meta = file.metadata().unwrap();
    let mut record = File::create(dir.path().join("identity")).unwrap();
    write!(record, "{}:{}", meta.dev(), meta.ino()).unwrap();
    record.sync_all().unwrap();
    dir
}

fn swap(dir: &Path) {
    fs::write(dir.join("replacement"), []).unwrap();
    fs::rename(dir.join("replacement"), dir.join("activity.lock")).unwrap();
}

#[tokio::test]
async fn restart_rejects_replacement_and_recovery_drains_original_writer() {
    let dir = fixture();
    fs::hard_link(
        dir.path().join("activity.lock"),
        dir.path().join("original"),
    )
    .unwrap();
    let mut writer = Worker::start(dir.path(), "writer");
    writer.marker("holding").await;
    swap(dir.path());
    let mut restarted = Worker::start(dir.path(), "authority");
    restarted.marker("mismatch").await;
    restarted.finish().await;

    // Restore only the original fixture inode, not a newly created lock file.
    fs::rename(
        dir.path().join("original"),
        dir.path().join("activity.lock"),
    )
    .unwrap();
    let mut recovery = Worker::start(dir.path(), "authority");
    recovery.marker("verified").await;
    recovery.send("continue").await;
    recovery.marker("busy").await;
    recovery.finish().await;
    writer.send("release").await;
    writer.finish().await;
    let mut after = Worker::start(dir.path(), "authority");
    after.marker("verified").await;
    after.send("continue").await;
    after.marker("acquired").await;
    after.finish().await;
}

#[tokio::test]
async fn replacement_between_validation_and_lock_is_rejected() {
    let dir = fixture();
    let mut authority = Worker::start(dir.path(), "authority");
    authority.marker("verified").await;
    swap(dir.path());
    authority.send("continue").await;
    authority.marker("mismatch").await;
    authority.finish().await;
}

#[tokio::test]
async fn replacement_after_final_check_still_splits_the_lock() {
    let dir = fixture();
    let mut authority = Worker::start(dir.path(), "pause");
    authority.marker("verified").await;
    authority.send("continue").await;
    authority.marker("acquired").await;
    swap(dir.path());
    let replacement = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("activity.lock"))
        .unwrap();
    replacement.try_lock_shared().unwrap();
    authority.send("release").await;
    authority.finish().await;
}

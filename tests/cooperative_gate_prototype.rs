//! Research fixture only. This protocol is not used by the application.
#![cfg(unix)]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, Command},
    time::timeout,
};

fn open_file(dir: &Path, name: &str) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(name))
        .unwrap()
}

fn set_closed(state: &mut File, closed: bool, fail_before_sync: bool) {
    state.seek(SeekFrom::Start(0)).unwrap();
    state.write_all(&[u8::from(closed)]).unwrap();
    assert!(
        !fail_before_sync,
        "synthetic failure after state write before sync"
    );
    state.sync_all().unwrap();
}

fn is_closed(state: &mut File) -> bool {
    let mut value = [0u8; 1];
    state.seek(SeekFrom::Start(0)).unwrap();
    state.read_exact(&mut value).unwrap();
    assert!(value[0] <= 1);
    value[0] == 1
}

fn await_release() {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "release");
}

// Re-exec this integration-test binary as an independent process. Marker
// messages use stderr; the test harness's own output is deliberately ignored.
#[test]
fn gate_worker() {
    let Ok(role) = std::env::var("GATE_PROTOTYPE_ROLE") else {
        return;
    };
    let dir = std::env::var_os("GATE_PROTOTYPE_DIR").unwrap();
    let dir = Path::new(&dir);
    let admission = open_file(dir, "admission.lock");
    let activity = open_file(dir, "activity.lock");
    let mut state = open_file(dir, "state");
    if role == "writer" || role == "writer_once" {
        if admission.try_lock_shared().is_err() {
            eprintln!("MARK waiting");
            admission.lock_shared().unwrap();
        }
        if is_closed(&mut state) {
            eprintln!("MARK denied");
            return;
        }
        activity.lock_shared().unwrap();
        admission.unlock().unwrap();
        eprintln!("MARK admitted");
        let db = rusqlite::Connection::open(dir.join("synthetic.db")).unwrap();
        db.busy_timeout(Duration::from_secs(3)).unwrap();
        db.execute_batch("BEGIN IMMEDIATE; INSERT INTO writes(value) VALUES ('synthetic');")
            .unwrap();
        eprintln!("MARK entered");
        if role == "writer" {
            await_release();
        }
        db.execute_batch("COMMIT").unwrap();
        activity.unlock().unwrap();
    } else if role == "capture" || role == "capture_fail_reopen" || role == "capture_fail_close" {
        if admission.try_lock().is_err() {
            eprintln!("MARK capture_waiting");
            admission.lock().unwrap();
        }
        set_closed(&mut state, true, role == "capture_fail_close");
        eprintln!("MARK closed");
        if activity.try_lock().is_err() {
            eprintln!("MARK draining");
            activity.lock().unwrap();
        }
        // The application gate must not hold SQLite's reservation: capture
        // needs to obtain its own reservation after draining the writers.
        let db = rusqlite::Connection::open(dir.join("synthetic.db")).unwrap();
        db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
        eprintln!("MARK drained");
        await_release();
        // Reopening is explicit and only while admission remains exclusive.
        set_closed(&mut state, false, role == "capture_fail_reopen");
        activity.unlock().unwrap();
        admission.unlock().unwrap();
        eprintln!("MARK reopened");
    } else {
        panic!("unknown synthetic role");
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stderr: BufReader<ChildStderr>,
}

impl Worker {
    fn spawn(dir: &Path, role: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("GATE_PROTOTYPE_ROLE", role)
            .env("GATE_PROTOTYPE_DIR", dir)
            .args(["--exact", "gate_worker", "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        Self {
            child,
            stdin,
            stderr,
        }
    }

    async fn marker(&mut self, expected: &str) {
        let expected = format!("MARK {expected}");
        timeout(Duration::from_secs(5), async {
            loop {
                let mut line = String::new();
                let n = self.stderr.read_line(&mut line).await.unwrap();
                assert!(n > 0, "worker exited before marker {expected}");
                if line.trim() == expected {
                    break;
                }
            }
        })
        .await
        .expect("worker marker timed out");
    }

    async fn release(&mut self) {
        self.stdin.write_all(b"release\n").await.unwrap();
        self.stdin.flush().await.unwrap();
    }
    async fn success(&mut self) {
        assert!(
            timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    async fn kill(&mut self) {
        self.child.kill().await.unwrap();
        let _ = self.child.wait().await.unwrap();
    }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in ["admission.lock", "activity.lock", "state"] {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.path().join(name))
            .unwrap();
        if name == "state" {
            file.write_all(&[0]).unwrap();
            file.sync_all().unwrap();
        }
    }
    let db = rusqlite::Connection::open(dir.path().join("synthetic.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE writes(value TEXT NOT NULL);")
        .unwrap();
    dir
}

fn committed_writes(dir: &Path) -> i64 {
    rusqlite::Connection::open(dir.join("synthetic.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM writes", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn closes_admission_drains_writer_and_reopens_explicitly() {
    let dir = fixture();
    let mut writer = Worker::spawn(dir.path(), "writer");
    writer.marker("entered").await;
    let mut capture = Worker::spawn(dir.path(), "capture");
    capture.marker("closed").await;
    capture.marker("draining").await;
    let mut later = Worker::spawn(dir.path(), "writer");
    later.marker("waiting").await;
    assert_eq!(committed_writes(dir.path()), 0);
    writer.release().await;
    writer.success().await;
    capture.marker("drained").await;
    assert_eq!(committed_writes(dir.path()), 1);
    assert!(later.child.try_wait().unwrap().is_none());
    capture.release().await;
    capture.marker("reopened").await;
    capture.success().await;
    later.marker("entered").await;
    later.release().await;
    later.success().await;
    assert_eq!(committed_writes(dir.path()), 2);
}

#[tokio::test]
async fn writer_crash_releases_active_lock_and_allows_drain() {
    let dir = fixture();
    let mut writer = Worker::spawn(dir.path(), "writer");
    writer.marker("entered").await;
    let mut capture = Worker::spawn(dir.path(), "capture");
    capture.marker("closed").await;
    capture.marker("draining").await;
    writer.kill().await;
    capture.marker("drained").await;
    assert_eq!(committed_writes(dir.path()), 0);
    capture.release().await;
    capture.marker("reopened").await;
    capture.success().await;
}

#[tokio::test]
async fn capture_crash_leaves_persistent_denial_until_explicit_recovery() {
    let dir = fixture();
    let mut capture = Worker::spawn(dir.path(), "capture");
    capture.marker("closed").await;
    capture.marker("drained").await;
    capture.kill().await;
    let mut denied = Worker::spawn(dir.path(), "writer");
    denied.marker("denied").await;
    denied.success().await;
    let mut recovery = Worker::spawn(dir.path(), "capture");
    recovery.marker("closed").await;
    recovery.marker("drained").await;
    recovery.release().await;
    recovery.marker("reopened").await;
    recovery.success().await;
    let mut writer = Worker::spawn(dir.path(), "writer");
    writer.marker("entered").await;
    writer.release().await;
    writer.success().await;
}

#[tokio::test]
async fn capture_crash_during_drain_requires_restart_and_drains_existing_writer() {
    let dir = fixture();
    let mut writer = Worker::spawn(dir.path(), "writer");
    writer.marker("entered").await;
    let mut capture = Worker::spawn(dir.path(), "capture");
    capture.marker("closed").await;
    capture.marker("draining").await;
    capture.kill().await;
    let mut denied = Worker::spawn(dir.path(), "writer");
    denied.marker("denied").await;
    denied.success().await;
    let mut recovery = Worker::spawn(dir.path(), "capture");
    recovery.marker("closed").await;
    recovery.marker("draining").await;
    writer.release().await;
    writer.success().await;
    recovery.marker("drained").await;
    recovery.release().await;
    recovery.marker("reopened").await;
    recovery.success().await;
}

#[tokio::test]
async fn invalid_persisted_state_does_not_admit_writer() {
    let dir = fixture();
    std::fs::write(dir.path().join("state"), [7]).unwrap();
    let mut writer = Worker::spawn(dir.path(), "writer");
    let result = timeout(Duration::from_secs(5), writer.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!result.success());
}

#[tokio::test]
async fn incomplete_initialization_is_rejected_without_creating_missing_files() {
    for name in ["state", "admission.lock", "activity.lock"] {
        let dir = fixture();
        std::fs::remove_file(dir.path().join(name)).unwrap();
        let mut writer = Worker::spawn(dir.path(), "writer");
        let status = timeout(Duration::from_secs(5), writer.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success(), "writer admitted with missing {name}");
        assert!(!dir.path().join(name).exists());
        assert_eq!(committed_writes(dir.path()), 0);
    }
    let dir = fixture();
    std::fs::write(dir.path().join("state"), []).unwrap();
    let mut writer = Worker::spawn(dir.path(), "writer");
    let status = timeout(Duration::from_secs(5), writer.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert_eq!(committed_writes(dir.path()), 0);
}

#[tokio::test]
async fn all_admitted_writers_finish_before_capture_drains() {
    let dir = fixture();
    let mut first = Worker::spawn(dir.path(), "writer");
    first.marker("entered").await;
    let mut others = Vec::new();
    for _ in 0..3 {
        let mut writer = Worker::spawn(dir.path(), "writer_once");
        writer.marker("admitted").await;
        others.push(writer);
    }
    let mut capture = Worker::spawn(dir.path(), "capture");
    capture.marker("closed").await;
    capture.marker("draining").await;
    let mut late = Worker::spawn(dir.path(), "writer");
    late.marker("waiting").await;
    first.release().await;
    first.success().await;
    for writer in &mut others {
        writer.marker("entered").await;
        writer.success().await;
    }
    capture.marker("drained").await;
    assert_eq!(committed_writes(dir.path()), 4);
    assert!(late.child.try_wait().unwrap().is_none());
    capture.release().await;
    capture.marker("reopened").await;
    capture.success().await;
    late.marker("entered").await;
    late.release().await;
    late.success().await;
    assert_eq!(committed_writes(dir.path()), 5);
}

#[tokio::test]
async fn competing_coordinators_do_not_capture_at_the_same_time() {
    let dir = fixture();
    let mut first = Worker::spawn(dir.path(), "capture");
    first.marker("closed").await;
    first.marker("drained").await;
    let mut second = Worker::spawn(dir.path(), "capture");
    second.marker("capture_waiting").await;
    first.release().await;
    first.marker("reopened").await;
    first.success().await;
    second.marker("closed").await;
    second.marker("drained").await;
    second.release().await;
    second.marker("reopened").await;
    second.success().await;
}

#[tokio::test]
async fn interrupted_reopen_exposes_a_fail_open_gap_in_the_prototype() {
    let dir = fixture();
    let mut capture = Worker::spawn(dir.path(), "capture_fail_reopen");
    capture.marker("closed").await;
    capture.marker("drained").await;
    capture.release().await;
    let status = timeout(Duration::from_secs(5), capture.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    // The marker was overwritten before the injected sync failure. The next
    // process sees it as open even though successful persistence is unproven.
    let mut writer = Worker::spawn(dir.path(), "writer");
    writer.marker("entered").await;
    writer.release().await;
    writer.success().await;
    assert_eq!(committed_writes(dir.path()), 1);
}

#[tokio::test]
async fn failed_close_before_sync_is_visible_as_closed_without_power_loss() {
    let dir = fixture();
    let mut capture = Worker::spawn(dir.path(), "capture_fail_close");
    let status = timeout(Duration::from_secs(5), capture.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    let mut writer = Worker::spawn(dir.path(), "writer");
    writer.marker("denied").await;
    writer.success().await;
    assert_eq!(committed_writes(dir.path()), 0);
}

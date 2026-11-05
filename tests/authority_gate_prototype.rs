//! Experimental process-owned authority; never used by application code.
#![cfg(unix)]

use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader as StdReader, Write},
    os::unix::net::{UnixListener as StdListener, UnixStream as StdStream},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::{Child, Command},
    time::timeout,
};

#[derive(Default)]
struct Gate {
    open: bool,
    active: usize,
    capturing: bool,
    drained: bool,
    capture_barrier: Option<File>,
}
type Shared = Arc<(Mutex<Gate>, Condvar)>;

fn persist_closed(path: &Path, inject_failure: bool) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(b"closed\n")?;
    if inject_failure {
        return Err(std::io::Error::other("synthetic sync failure"));
    }
    file.sync_all()
}

fn serve_client(
    mut socket: StdStream,
    shared: Shared,
    marker: PathBuf,
    barrier: PathBuf,
) -> std::io::Result<()> {
    let mut reader = StdReader::new(socket.try_clone()?);
    let mut command = String::new();
    reader.read_line(&mut command)?;
    let (mutex, condition) = &*shared;
    match command.trim() {
        "WRITE" => {
            let mut state = mutex.lock().unwrap();
            if !state.open {
                writeln!(socket, "DENIED")?;
                return Ok(());
            }
            state.active += 1;
            drop(state);
            // Always release the in-memory permit on disconnect or write failure.
            let result = writeln!(socket, "ADMITTED").and_then(|_| {
                let mut done = String::new();
                reader.read_line(&mut done).map(|_| ())
            });
            let mut state = mutex.lock().unwrap();
            state.active -= 1;
            condition.notify_all();
            result?;
        }
        "CLOSE" | "CLOSE_FAIL" => {
            let mut state = mutex.lock().unwrap();
            if state.capturing {
                writeln!(socket, "CAPTURE_BUSY")?;
                return Ok(());
            }
            state.open = false;
            state.capturing = true;
            state.drained = false;
            if persist_closed(&marker, command.trim() == "CLOSE_FAIL").is_err() {
                writeln!(socket, "PERSIST_FAILED")?;
                return Ok(());
            }
            writeln!(socket, "CLOSED")?;
            while state.active > 0 {
                state = condition.wait(state).unwrap();
            }
            let file = OpenOptions::new().read(true).write(true).open(&barrier)?;
            if file.try_lock().is_err() {
                writeln!(socket, "BARRIER_BUSY")?;
            } else {
                state.capture_barrier = Some(file);
                state.drained = true;
                writeln!(socket, "DRAINED")?;
            }
        }
        "RETRY_DRAIN" => {
            let mut state = mutex.lock().unwrap();
            if !state.capturing || state.drained {
                writeln!(socket, "NOT_DRAINED")?;
            } else if persist_closed(&marker, false).is_err() {
                writeln!(socket, "PERSIST_FAILED")?;
            } else if state.active != 0 {
                writeln!(socket, "NOT_DRAINED")?;
            } else {
                let file = OpenOptions::new().read(true).write(true).open(&barrier)?;
                if file.try_lock().is_err() {
                    writeln!(socket, "BARRIER_BUSY")?;
                } else {
                    state.capture_barrier = Some(file);
                    state.drained = true;
                    writeln!(socket, "DRAINED")?;
                }
            }
        }
        "FINISH" => {
            let mut state = mutex.lock().unwrap();
            if !state.capturing || !state.drained {
                writeln!(socket, "NOT_DRAINED")?;
            } else {
                drop(state.capture_barrier.take());
                state.capturing = false;
                writeln!(socket, "FINISHED_CLOSED")?;
            }
        }
        "OPEN" | "OPEN_FAIL" => {
            let mut state = mutex.lock().unwrap();
            if state.open || state.active != 0 || state.capturing {
                writeln!(socket, "NOT_DRAINED")?;
                return Ok(());
            }
            let file = OpenOptions::new().read(true).write(true).open(&barrier)?;
            if file.try_lock().is_err() {
                writeln!(socket, "BARRIER_BUSY")?;
                return Ok(());
            }
            // The only persisted marker is closed; no open state is made visible
            // to writers before sync has actually returned success.
            if persist_closed(&marker, command.trim() == "OPEN_FAIL").is_err() {
                writeln!(socket, "PERSIST_FAILED")?;
            } else {
                state.open = true;
                writeln!(socket, "OPENED")?;
            }
            drop(file);
        }
        "STATUS" => {
            let state = mutex.lock().unwrap();
            writeln!(
                socket,
                "{} {}",
                if state.open { "OPEN" } else { "CLOSED" },
                state.active
            )?;
        }
        _ => {
            writeln!(socket, "INVALID")?;
        }
    }
    Ok(())
}

#[test]
fn authority_worker() {
    let Ok(dir) = std::env::var("AUTHORITY_FIXTURE_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let marker = dir.join("closed-marker");
    if persist_closed(&marker, std::env::var_os("AUTHORITY_FAIL_START").is_some()).is_err() {
        panic!("synthetic startup persistence failed");
    }
    let listener = StdListener::bind(dir.join("gate.sock")).unwrap();
    let shared = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
    eprintln!("MARK ready");
    for incoming in listener.incoming() {
        let socket = incoming.unwrap();
        let shared = shared.clone();
        let marker = marker.clone();
        let barrier = dir.join("activity.lock");
        std::thread::spawn(move || {
            let _ = serve_client(socket, shared, marker, barrier);
        });
    }
}

#[test]
fn cooperative_writer_worker() {
    let Ok(dir) = std::env::var("WRITER_FIXTURE_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let barrier = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("activity.lock"))
        .unwrap();
    barrier.lock_shared().unwrap();
    let mut socket = StdStream::connect(dir.join("gate.sock")).unwrap();
    writeln!(socket, "WRITE").unwrap();
    let mut reader = StdReader::new(socket.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert_eq!(response.trim(), "ADMITTED");
    let db = rusqlite::Connection::open(dir.join("writer.db")).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS writes(value INTEGER); BEGIN IMMEDIATE; INSERT INTO writes VALUES (1);").unwrap();
    eprintln!("MARK transaction");
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    assert_eq!(command.trim(), "commit");
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    drop(reader);
    drop(socket);
    drop(barrier);
}

async fn start_writer(dir: &Path) -> Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env("WRITER_FIXTURE_DIR", dir)
        .args(["--exact", "cooperative_writer_worker", "--nocapture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut marker = String::new();
    timeout(Duration::from_secs(5), stderr.read_line(&mut marker))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(marker.trim(), "MARK transaction");
    child
}

#[tokio::test]
async fn previous_writer_blocks_restart_and_capture_until_commit() {
    let mut fixture = Fixture::start().await;
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let mut writer = start_writer(fixture.dir.path()).await;
    fixture.restart().await;
    assert_eq!(fixture.command("OPEN").await, "BARRIER_BUSY");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    let mut capture = fixture.connect("CLOSE").await;
    assert_eq!(line(&mut capture).await, "CLOSED");
    assert_eq!(line(&mut capture).await, "BARRIER_BUSY");
    assert_eq!(fixture.command("RETRY_DRAIN").await, "BARRIER_BUSY");
    writer
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"commit\n")
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_secs(5), writer.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(fixture.command("RETRY_DRAIN").await, "DRAINED");
    assert_eq!(fixture.command("OPEN").await, "NOT_DRAINED");
    assert_eq!(fixture.command("FINISH").await, "FINISHED_CLOSED");
    assert_eq!(fixture.command("OPEN_FAIL").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let db = rusqlite::Connection::open(fixture.dir.path().join("writer.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM writes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn previous_writer_death_releases_barrier_and_rolls_back() {
    let mut fixture = Fixture::start().await;
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let mut writer = start_writer(fixture.dir.path()).await;
    fixture.restart().await;
    assert_eq!(fixture.command("OPEN").await, "BARRIER_BUSY");
    writer.kill().await.unwrap();
    writer.wait().await.unwrap();
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let db = rusqlite::Connection::open(fixture.dir.path().join("writer.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM writes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn retry_after_failed_close_requires_successful_persistence() {
    let fixture = Fixture::start().await;
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    assert_eq!(fixture.command("CLOSE_FAIL").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    std::fs::remove_file(fixture.dir.path().join("closed-marker")).unwrap();
    assert_eq!(fixture.command("RETRY_DRAIN").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("FINISH").await, "NOT_DRAINED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    std::fs::write(fixture.dir.path().join("closed-marker"), b"closed\n").unwrap();
    assert_eq!(fixture.command("RETRY_DRAIN").await, "DRAINED");
    assert_eq!(fixture.command("FINISH").await, "FINISHED_CLOSED");
    assert_eq!(fixture.command("OPEN").await, "OPENED");
}

#[tokio::test]
async fn failed_close_retry_and_restart_preserve_barrier_for_active_writer() {
    let mut fixture = Fixture::start().await;
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let mut writer = start_writer(fixture.dir.path()).await;
    assert_eq!(fixture.command("CLOSE_FAIL").await, "PERSIST_FAILED");
    std::fs::remove_file(fixture.dir.path().join("closed-marker")).unwrap();
    assert_eq!(fixture.command("RETRY_DRAIN").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert_eq!(fixture.command("FINISH").await, "NOT_DRAINED");
    std::fs::write(fixture.dir.path().join("closed-marker"), b"closed\n").unwrap();
    assert_eq!(fixture.command("RETRY_DRAIN").await, "NOT_DRAINED");
    fixture.restart().await;
    assert_eq!(fixture.command("OPEN").await, "BARRIER_BUSY");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    let mut capture = fixture.connect("CLOSE").await;
    assert_eq!(line(&mut capture).await, "CLOSED");
    assert_eq!(line(&mut capture).await, "BARRIER_BUSY");
    assert_eq!(fixture.command("RETRY_DRAIN").await, "BARRIER_BUSY");
    writer
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"commit\n")
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_secs(5), writer.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(fixture.command("RETRY_DRAIN").await, "DRAINED");
    assert_eq!(fixture.command("FINISH").await, "FINISHED_CLOSED");
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let db = rusqlite::Connection::open(fixture.dir.path().join("writer.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM writes", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

struct Fixture {
    dir: tempfile::TempDir,
    child: Child,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("closed-marker"), b"closed\n").unwrap();
        std::fs::write(dir.path().join("activity.lock"), []).unwrap();
        let child = Self::spawn(dir.path(), false).await;
        Self { dir, child }
    }

    async fn spawn(dir: &Path, fail_start: bool) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("AUTHORITY_FIXTURE_DIR", dir)
            .args(["--exact", "authority_worker", "--nocapture"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if fail_start {
            command.env("AUTHORITY_FAIL_START", "1");
        }
        let mut child = command.spawn().unwrap();
        if !fail_start {
            let mut stderr = BufReader::new(child.stderr.take().unwrap());
            let mut line = String::new();
            timeout(Duration::from_secs(5), stderr.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(line.trim(), "MARK ready");
        }
        child
    }

    async fn connect(&self, command: &str) -> BufReader<UnixStream> {
        let mut socket = UnixStream::connect(self.dir.path().join("gate.sock"))
            .await
            .unwrap();
        socket
            .write_all(format!("{command}\n").as_bytes())
            .await
            .unwrap();
        BufReader::new(socket)
    }

    async fn command(&self, command: &str) -> String {
        let mut stream = self.connect(command).await;
        line(&mut stream).await
    }

    async fn restart(&mut self) {
        self.child.kill().await.unwrap();
        let _ = self.child.wait().await.unwrap();
        // Only unlink this test's own socket after its process is reaped.
        std::fs::remove_file(self.dir.path().join("gate.sock")).unwrap();
        self.child = Self::spawn(self.dir.path(), false).await;
    }
}

async fn line(stream: &mut BufReader<UnixStream>) -> String {
    let mut line = String::new();
    let count = timeout(Duration::from_secs(5), stream.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(count > 0);
    line.trim().to_owned()
}

#[tokio::test]
async fn persistence_failures_do_not_reopen_and_restart_starts_closed() {
    let mut fixture = Fixture::start().await;
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert_eq!(fixture.command("OPEN_FAIL").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    fixture.restart().await;
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    assert_eq!(fixture.command("CLOSE_FAIL").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    fixture.restart().await;
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    fixture.restart().await;
    assert_eq!(fixture.command("WRITE").await, "DENIED");
}

#[tokio::test]
async fn closure_waits_for_admitted_writer_and_then_reopens() {
    let fixture = Fixture::start().await;
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let mut writer = fixture.connect("WRITE").await;
    assert_eq!(line(&mut writer).await, "ADMITTED");
    let mut capture = fixture.connect("CLOSE").await;
    assert_eq!(line(&mut capture).await, "CLOSED");
    assert_eq!(fixture.command("STATUS").await, "CLOSED 1");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert_eq!(fixture.command("OPEN").await, "NOT_DRAINED");
    writer.get_mut().write_all(b"DONE\n").await.unwrap();
    assert_eq!(line(&mut capture).await, "DRAINED");
    assert_eq!(fixture.command("OPEN").await, "NOT_DRAINED");
    assert_eq!(fixture.command("FINISH").await, "FINISHED_CLOSED");
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let mut later = fixture.connect("WRITE").await;
    assert_eq!(line(&mut later).await, "ADMITTED");
    later.get_mut().write_all(b"DONE\n").await.unwrap();
}

#[tokio::test]
async fn failed_startup_does_not_publish_an_authority_socket() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("closed-marker"), b"closed\n").unwrap();
    let mut child = Fixture::spawn(dir.path(), true).await;
    assert!(
        !timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!dir.path().join("gate.sock").exists());
}

#[tokio::test]
async fn missing_marker_io_failure_keeps_authority_closed() {
    let fixture = Fixture::start().await;
    std::fs::remove_file(fixture.dir.path().join("closed-marker")).unwrap();
    assert_eq!(fixture.command("OPEN").await, "PERSIST_FAILED");
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    assert!(!fixture.dir.path().join("closed-marker").exists());
}

#[tokio::test]
async fn coordinator_death_does_not_revoke_an_existing_sqlite_transaction() {
    let mut fixture = Fixture::start().await;
    assert_eq!(fixture.command("OPEN").await, "OPENED");
    let mut writer = fixture.connect("WRITE").await;
    assert_eq!(line(&mut writer).await, "ADMITTED");
    let db_path = fixture.dir.path().join("synthetic.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE writes(value INTEGER); BEGIN IMMEDIATE; INSERT INTO writes VALUES (1);",
    )
    .unwrap();
    fixture.restart().await;
    assert_eq!(fixture.command("WRITE").await, "DENIED");
    // Losing the connection does not itself roll back an already admitted
    // transaction; a real writer must participate in a stronger protocol.
    db.execute_batch("COMMIT").unwrap();
    let committed: i64 = db
        .query_row("SELECT COUNT(*) FROM writes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(committed, 1);
}

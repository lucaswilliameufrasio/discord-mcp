//! Synthetic crash boundaries, not a production supervisor or adoption proof.
#![cfg(target_os = "linux")]

use discord_mcp::{
    channels::{ChannelStore, EmbeddedSecret},
    linux_peer_identity::{MessageReceiver, PeerIdentityError, RotationScope, SupervisedPeer},
    rotation_progress::Publication,
};
use rusqlite::OptionalExtension;
use std::{
    os::{fd::OwnedFd, unix::fs::MetadataExt},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    time::{sleep, timeout},
};

struct Guard(Option<SupervisedPeer>);
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(supervisor) = self.0.take() {
            let mut child = supervisor.into_child();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[tokio::test]
async fn supervised_decision_recovers_before_commit_and_after_lost_reply() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fixture.db");
    let key = [117; 32];
    let operation = [81; 16];
    let instance = [82; 16];
    let epoch = [83; 16];
    let scope = RotationScope {
        operation,
        instance,
        boot: epoch,
    };
    let store = ChannelStore::open(&db, &key).unwrap();
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            operation,
            &[instance],
        )
        .unwrap();
    store
        .begin_rotation_process(operation, instance, epoch, "synthetic-new")
        .unwrap();
    rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE fixture_decisions (operation BLOB NOT NULL, instance BLOB NOT NULL, epoch BLOB NOT NULL, event TEXT NOT NULL, UNIQUE(operation,instance,epoch,event))").unwrap();
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let script = r#"import socket, sqlite3, sys
events=socket.socket(fileno=0)
control=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
control.connect(sys.argv[2])
events.send(b'oauth')
if control.recv(1)!=b'B': sys.exit(2)
db=sqlite3.connect(sys.argv[1], timeout=1, isolation_level=None)
db.execute('BEGIN IMMEDIATE')
op,instance,epoch=map(bytes.fromhex,sys.argv[3:6])
cur=db.execute("INSERT INTO fixture_decisions SELECT p.revision,e.instance,e.boot,'oauth' FROM rotation_progress p JOIN rotation_process_epoch e ON e.revision=p.revision WHERE p.revision=? AND e.instance=? AND e.boot=? AND p.version=0",(op,instance,epoch))
if cur.rowcount!=1: sys.exit(3)
control.sendall(b'P')
if control.recv(1)!=b'C': sys.exit(4)
db.execute('COMMIT')
control.sendall(b'D')
control.recv(1)
"#;
    for committed in [false, true] {
        let control_path = dir.path().join(if committed {
            "after.sock"
        } else {
            "before.sock"
        });
        let listener = UnixListener::bind(&control_path).unwrap();
        let (receiver, sender) = MessageReceiver::pair().unwrap();
        let fd: OwnedFd = sender.into();
        let child = Command::new("/usr/bin/python3")
            .env_clear()
            .args(["-c", script])
            .arg(&db)
            .arg(&control_path)
            .arg(hex::encode(operation))
            .arg(hex::encode(instance))
            .arg(hex::encode(epoch))
            .stdin(Stdio::from(fd))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut guard = Guard(Some(SupervisedPeer::new(child, uid, scope).unwrap()));
        let (mut control, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut payload = [0; 16];
        let result = timeout(Duration::from_secs(5), async {
            loop {
                match guard.0.as_mut().unwrap().receive_rotation_message(
                    &receiver,
                    &store,
                    scope,
                    &mut payload,
                ) {
                    Err(PeerIdentityError::MessageUnavailable) => {
                        sleep(Duration::from_millis(10)).await
                    }
                    result => break result,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(result, Ok(5));
        assert_eq!(&payload[..5], b"oauth");
        control.write_all(b"B").await.unwrap();
        let mut marker = [0; 1];
        timeout(Duration::from_secs(5), control.read_exact(&mut marker))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&marker, b"P");
        if committed {
            control.write_all(b"C").await.unwrap();
            timeout(Duration::from_secs(5), control.read_exact(&mut marker))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&marker, b"D"); // Internal test checkpoint, not an operator response.
        }
        drop(guard); // Kill before commit or after commit, before an operator reply.
        let recovered = rusqlite::Connection::open(&db).unwrap();
        let count: i64 = recovered
            .query_row("SELECT count(*) FROM fixture_decisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, i64::from(committed));
        let found: Option<String> = recovered.query_row(
            "SELECT event FROM fixture_decisions WHERE operation=?1 AND instance=?2 AND epoch=?3",
            rusqlite::params![operation.as_slice(), instance.as_slice(), epoch.as_slice()],
            |row| row.get(0),
        ).optional().unwrap();
        assert_eq!(
            found.as_deref(),
            if committed { Some("oauth") } else { None }
        );
        let wrong: Option<String> = recovered.query_row(
            "SELECT event FROM fixture_decisions WHERE operation=?1 AND instance=?2 AND epoch=?3",
            rusqlite::params![[84u8; 16].as_slice(), instance.as_slice(), epoch.as_slice()],
            |row| row.get(0),
        ).optional().unwrap();
        assert!(wrong.is_none());
        if committed {
            let inserted = recovered.execute(
                "INSERT OR IGNORE INTO fixture_decisions(operation,instance,epoch,event) VALUES (?1,?2,?3,'oauth')",
                rusqlite::params![operation.as_slice(), instance.as_slice(), epoch.as_slice()],
            ).unwrap();
            assert_eq!(inserted, 0);
        }
        drop(recovered);
        let reopened = ChannelStore::open(&db, &key).unwrap();
        assert_eq!(
            reopened
                .get_rotation_progress(operation)
                .unwrap()
                .unwrap()
                .1
                .publication(),
            Publication::Unknown
        );
        assert_eq!(
            reopened.get_embedded_secret(secret).unwrap(),
            "synthetic-new"
        );
    }
    let status = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args([
            "setup",
            "local",
            "vault",
            "rotation-status",
            &hex::encode(operation),
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("adoption_verified=false"));
}

#[test]
fn deferred_constraint_rejects_commit_and_preserves_operation_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fixture.db");
    let key = [118; 32];
    let operation = [91; 16];
    let instance = [92; 16];
    let epoch = [93; 16];
    let store = ChannelStore::open(&db, &key).unwrap();
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            operation,
            &[instance],
        )
        .unwrap();
    store
        .begin_rotation_process(operation, instance, epoch, "synthetic-new")
        .unwrap();
    let mut conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE fixture_authority (id INTEGER PRIMARY KEY); CREATE TABLE fixture_commit_decision (operation BLOB PRIMARY KEY, authority_id INTEGER NOT NULL REFERENCES fixture_authority(id) DEFERRABLE INITIALLY DEFERRED)").unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let inserted = tx.execute("INSERT INTO fixture_commit_decision SELECT p.revision,1 FROM rotation_progress p JOIN rotation_process_epoch e ON p.revision=e.revision WHERE p.revision=?1 AND e.instance=?2 AND e.boot=?3 AND p.version=0", rusqlite::params![operation.as_slice(),instance.as_slice(),epoch.as_slice()]).unwrap();
    assert_eq!(inserted, 1); // The deferred foreign key is checked only at commit.
    let error = tx.commit().unwrap_err();
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.code == rusqlite::ErrorCode::ConstraintViolation)
    );
    drop(conn);
    let recovered = rusqlite::Connection::open(&db).unwrap();
    let count: i64 = recovered
        .query_row(
            "SELECT count(*) FROM fixture_commit_decision WHERE operation=?1",
            [operation.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let reopened = ChannelStore::open(&db, &key).unwrap();
    assert_eq!(
        reopened
            .get_rotation_progress(operation)
            .unwrap()
            .unwrap()
            .1
            .publication(),
        Publication::Unknown
    );
    assert_eq!(
        reopened.get_embedded_secret(secret).unwrap(),
        "synthetic-new"
    );
}

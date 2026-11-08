#![cfg(target_os = "linux")]

use discord_mcp::linux_peer_identity::{
    MessageReceiver, PeerIdentityError, RotationScope, SupervisedPeer, verify_peer,
};
use std::{
    os::unix::fs::MetadataExt,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};

#[tokio::test]
async fn kernel_peer_ids_reject_spoofed_pid_and_uid() {
    if let Some(path) = std::env::var_os("DISCORD_TEST_PEER_SOCKET") {
        let _connection = UnixStream::connect(path).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env("DISCORD_TEST_PEER_SOCKET", &socket)
        .args([
            "--exact",
            "kernel_peer_ids_reject_spoofed_pid_and_uid",
            "--nocapture",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let pid = child.id();
    let guard = Cleanup(&mut child);
    let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    assert_eq!(verify_peer(&stream, uid, pid), Ok(()));
    assert_eq!(
        verify_peer(&stream, uid, std::process::id()),
        Err(PeerIdentityError::Mismatch)
    );
    assert_eq!(
        verify_peer(&stream, uid.wrapping_add(1), pid),
        Err(PeerIdentityError::Mismatch)
    );
    assert_eq!(
        verify_peer(&stream, uid, 0),
        Err(PeerIdentityError::InvalidExpectedPid)
    );
    drop(guard);
}

#[tokio::test]
async fn supervisor_rejects_other_child_wrong_scope_and_exited_process() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let spawn = || {
        Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("DISCORD_TEST_PEER_SOCKET", &socket)
            .args([
                "--exact",
                "kernel_peer_ids_reject_spoofed_pid_and_uid",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let first = spawn();
    let (first_stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut second = spawn();
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let second_guard = Cleanup(&mut second);
    let (second_stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let scope = RotationScope {
        operation: [1; 16],
        instance: [2; 16],
        boot: [3; 16],
    };
    let db = dir.path().join("store.db");
    let key = [108; 32];
    let secret = discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret;
    let store = discord_mcp::channels::ChannelStore::open(&db, &key).unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            scope.operation,
            &[scope.instance],
        )
        .unwrap();
    store
        .begin_rotation_process(scope.operation, scope.instance, scope.boot, "synthetic-new")
        .unwrap();
    let mut supervisor = SupervisedPeer::new(first, uid, scope).unwrap();
    assert_eq!(supervisor.verify_connection(&first_stream, scope), Ok(()));
    assert_eq!(
        supervisor.verify_rotation_connection(&first_stream, &store, scope),
        Ok(())
    );
    assert_eq!(
        supervisor.verify_rotation_connection(&second_stream, &store, scope),
        Err(PeerIdentityError::Mismatch)
    );
    assert_eq!(
        supervisor.verify_connection(&second_stream, scope),
        Err(PeerIdentityError::Mismatch)
    );
    assert_eq!(
        supervisor.verify_connection(
            &first_stream,
            RotationScope {
                boot: [4; 16],
                ..scope
            }
        ),
        Err(PeerIdentityError::ScopeMismatch)
    );
    store
        .advance_rotation_progress(
            scope.operation,
            0,
            discord_mcp::rotation_progress::ProgressEvent::Uncertain,
        )
        .unwrap();
    assert_eq!(
        supervisor.verify_rotation_connection(&first_stream, &store, scope),
        Err(PeerIdentityError::TrackingUnavailable)
    );
    store
        .begin_rotation_process(scope.operation, scope.instance, [4; 16], "synthetic-new")
        .unwrap();
    assert!(
        store
            .check_rotation_process_epoch(scope.operation, scope.instance, [4; 16])
            .is_ok()
    );
    assert_eq!(
        supervisor.verify_rotation_connection(&first_stream, &store, scope),
        Err(PeerIdentityError::TrackingUnavailable)
    );
    store
        .advance_rotation_progress(
            scope.operation,
            1,
            discord_mcp::rotation_progress::ProgressEvent::Revoked,
        )
        .unwrap();
    assert!(
        store
            .check_rotation_process_epoch(scope.operation, scope.instance, [4; 16])
            .is_err()
    );
    assert_eq!(
        supervisor.verify_rotation_connection(&first_stream, &store, scope),
        Err(PeerIdentityError::TrackingUnavailable)
    );
    assert_eq!(store.get_embedded_secret(secret).unwrap(), "synthetic-new");
    assert_eq!(
        supervisor.verify_connection(
            &first_stream,
            RotationScope {
                operation: [5; 16],
                ..scope
            }
        ),
        Err(PeerIdentityError::ScopeMismatch)
    );
    tokio::time::timeout(Duration::from_secs(5), first_stream.readable())
        .await
        .unwrap()
        .unwrap();
    // Wait for the owned child to become observable as exited, without reaping it elsewhere.
    let mut exited = false;
    for _ in 0..50 {
        match supervisor.verify_connection(&first_stream, scope) {
            Err(PeerIdentityError::ChildExited) => {
                exited = true;
                break;
            }
            Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("unexpected supervisor result: {other:?}"),
        }
    }
    assert!(exited);
    assert_eq!(
        supervisor.verify_connection(&first_stream, scope),
        Err(PeerIdentityError::ChildExited)
    );
    let mut first = supervisor.into_child();
    let _ = first.wait();
    drop(second_guard);
}

#[tokio::test]
#[ignore = "requires Python 3 and isolated local process fixture"]
async fn inherited_socket_can_carry_messages_from_a_different_process() {
    use tokio::io::AsyncReadExt;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let script = r#"import os, socket, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sys.argv[1])
child = os.fork()
if child == 0:
    s.sendall((str(os.getpid()) + '\n').encode())
    os._exit(0)
os.waitpid(child, 0)
time.sleep(2)
"#;
    let child = Command::new("/usr/bin/python3")
        .env_clear()
        .args(["-c", script])
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let expected_pid = child.id();
    let scope = RotationScope {
        operation: [1; 16],
        instance: [2; 16],
        boot: [3; 16],
    };
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let mut supervisor = SupervisedPeer::new(child, uid, scope).unwrap();
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(supervisor.verify_connection(&stream, scope), Ok(()));
    let mut bytes = [0u8; 32];
    let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let sender: u32 = std::str::from_utf8(&bytes[..count])
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(
        sender, expected_pid,
        "the writer inherited the authenticated connection"
    );
    assert_eq!(supervisor.verify_connection(&stream, scope), Ok(()));
    let mut child = supervisor.into_child();
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn supervised_message_rejects_inherited_sender_and_truncation() {
    use discord_mcp::channels::{ChannelStore, EmbeddedSecret};
    use std::os::fd::OwnedFd;
    let dir = tempfile::tempdir().unwrap();
    let store = ChannelStore::open(&dir.path().join("store.db"), &[109; 32]).unwrap();
    let scope = RotationScope {
        operation: [31; 16],
        instance: [32; 16],
        boot: [33; 16],
    };
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            scope.operation,
            &[scope.instance],
        )
        .unwrap();
    store
        .begin_rotation_process(scope.operation, scope.instance, scope.boot, "synthetic-new")
        .unwrap();
    let (receiver, sender) = MessageReceiver::pair().unwrap();
    let script = "import os,socket,time\ns=socket.socket(fileno=0)\ns.send(b'parent')\np=os.fork()\nif p==0:\n s.send(b'child')\n os._exit(0)\nos.waitpid(p,0)\ns.send(b'long-message')\ns.send(b'old-epoch')\ns.send(b'revoked')\ntime.sleep(5)";
    let fd: OwnedFd = sender.into();
    let child = Command::new("/usr/bin/python3")
        .env_clear()
        .args(["-c", script])
        .stdin(Stdio::from(fd))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let mut supervisor = SupervisedPeer::new(child, uid, scope).unwrap();
    let mut buf = [0; 64];
    async fn next(
        supervisor: &mut SupervisedPeer,
        receiver: &MessageReceiver,
        store: &ChannelStore,
        scope: RotationScope,
        buf: &mut [u8],
    ) -> Result<usize, PeerIdentityError> {
        for _ in 0..200 {
            match supervisor.receive_rotation_message(receiver, store, scope, buf) {
                Err(PeerIdentityError::MessageUnavailable) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                result => return result,
            }
        }
        panic!("message timed out")
    }
    assert_eq!(
        next(&mut supervisor, &receiver, &store, scope, &mut buf).await,
        Ok(6)
    );
    assert_eq!(&buf[..6], b"parent");
    assert_eq!(
        next(&mut supervisor, &receiver, &store, scope, &mut buf).await,
        Err(PeerIdentityError::Mismatch)
    );
    assert_eq!(
        next(&mut supervisor, &receiver, &store, scope, &mut buf[..1]).await,
        Err(PeerIdentityError::InvalidMessage)
    );
    store
        .begin_rotation_process(scope.operation, scope.instance, [34; 16], "synthetic-new")
        .unwrap();
    assert_eq!(
        next(&mut supervisor, &receiver, &store, scope, &mut buf).await,
        Err(PeerIdentityError::TrackingUnavailable)
    );
    store
        .begin_rotation_process(scope.operation, scope.instance, scope.boot, "synthetic-new")
        .unwrap();
    store
        .advance_rotation_progress(
            scope.operation,
            0,
            discord_mcp::rotation_progress::ProgressEvent::Revoked,
        )
        .unwrap();
    assert_eq!(
        next(&mut supervisor, &receiver, &store, scope, &mut buf).await,
        Err(PeerIdentityError::TrackingUnavailable)
    );
    let mut child = supervisor.into_child();
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn queued_message_from_exited_child_is_rejected() {
    use discord_mcp::channels::{ChannelStore, EmbeddedSecret};
    use std::os::fd::OwnedFd;
    let dir = tempfile::tempdir().unwrap();
    let store = ChannelStore::open(&dir.path().join("store.db"), &[110; 32]).unwrap();
    let scope = RotationScope {
        operation: [61; 16],
        instance: [62; 16],
        boot: [63; 16],
    };
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            scope.operation,
            &[scope.instance],
        )
        .unwrap();
    store
        .begin_rotation_process(scope.operation, scope.instance, scope.boot, "synthetic-new")
        .unwrap();
    let (receiver, sender) = MessageReceiver::pair().unwrap();
    let fd: OwnedFd = sender.into();
    let child = Command::new("/usr/bin/python3")
        .env_clear()
        .args([
            "-c",
            "import socket\ns=socket.socket(fileno=0)\ns.send(b'queued')",
        ])
        .stdin(Stdio::from(fd))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let mut supervisor = SupervisedPeer::new(child, uid, scope).unwrap();
    let mut exited = false;
    for _ in 0..200 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        if stat.rsplit_once(") ").unwrap().1.starts_with("Z ") {
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(exited, "fixture child did not exit");
    let mut payload = [0; 32];
    assert_eq!(
        supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload),
        Err(PeerIdentityError::ChildExited)
    );
    assert_eq!(
        supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload),
        Err(PeerIdentityError::ChildExited)
    );
    let mut child = supervisor.into_child();
    assert!(child.wait().unwrap().success());
}

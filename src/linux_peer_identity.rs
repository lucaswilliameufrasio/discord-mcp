//! Narrow Linux Unix-socket peer credential check; not an adoption verifier.
//!
//! The expected UID and PID must be obtained independently from a trusted
//! supervisor. Caller-supplied IDs or a database record are not a trust anchor.
//! This check does not defeat same-UID compromise, PID reuse, descriptor passing,
//! or a privileged attacker and must not be persisted as proof of liveness.
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg, sockopt,
};
use std::{io::IoSliceMut, mem::MaybeUninit, os::unix::net::UnixDatagram};
use tokio::net::UnixStream;

#[derive(Debug, Eq, PartialEq)]
pub enum PeerIdentityError {
    InvalidExpectedPid,
    CredentialsUnavailable,
    Mismatch,
    ChildExited,
    ChildStatusUnavailable,
    ScopeMismatch,
    InvalidScope,
    TrackingUnavailable,
    MessageUnavailable,
    InvalidMessage,
}

/// Experimental datagram boundary. Create before handing its sender to a child;
/// it provides no protection against privileged senders or compromised children.
pub struct MessageReceiver(UnixDatagram);

impl MessageReceiver {
    pub fn pair() -> std::io::Result<(Self, UnixDatagram)> {
        let (receiver, sender) = UnixDatagram::pair()?;
        sockopt::set_socket_passcred(&receiver, true)?;
        Ok((Self(receiver), sender))
    }
}

/// Supervisor-owned context. All IDs must be established by the supervisor,
/// never trusted from the peer. They do not themselves authenticate a workload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RotationScope {
    pub operation: [u8; 16],
    pub instance: [u8; 16],
    pub boot: [u8; 16],
}

/// Holds the actual child handle so a recycled PID cannot be accepted after it
/// has been observed exiting. No data from a peer is taken as an expected PID.
pub struct SupervisedPeer {
    child: std::process::Child,
    uid: u32,
    scope: RotationScope,
    exited: bool,
}

impl SupervisedPeer {
    /// Receive one bounded datagram, authenticating its kernel-provided PID and
    /// UID and checking the rotation snapshot. Not evidence of an OAuth login.
    /// Nonblocking; callers must arrange readiness and handle failure.
    pub fn receive_rotation_message(
        &mut self,
        receiver: &MessageReceiver,
        store: &crate::channels::ChannelStore,
        expected: RotationScope,
        payload: &mut [u8],
    ) -> Result<usize, PeerIdentityError> {
        if self.scope != expected {
            return Err(PeerIdentityError::ScopeMismatch);
        }
        if payload.is_empty() {
            return Err(PeerIdentityError::InvalidMessage);
        }
        self.require_running()?;
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let result = recvmsg(
            &receiver.0,
            &mut [IoSliceMut::new(payload)],
            &mut ancillary,
            RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
        )
        .map_err(|_| PeerIdentityError::MessageUnavailable)?;
        let mut credentials = None;
        let mut invalid = result.bytes == 0
            || result
                .flags
                .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC);
        for message in ancillary.drain() {
            match message {
                RecvAncillaryMessage::ScmCredentials(cred) if credentials.is_none() => {
                    credentials = Some(cred)
                }
                _ => invalid = true,
            }
        }
        if invalid {
            return Err(PeerIdentityError::InvalidMessage);
        }
        let cred = credentials.ok_or(PeerIdentityError::InvalidMessage)?;
        if cred.pid.as_raw_pid() != self.child.id() as i32 || cred.uid.as_raw() != self.uid {
            return Err(PeerIdentityError::Mismatch);
        }
        store
            .check_rotation_process_epoch(expected.operation, expected.instance, expected.boot)
            .map_err(|_| PeerIdentityError::TrackingUnavailable)?;
        self.require_running()?;
        Ok(result.bytes)
    }
    pub fn new(
        child: std::process::Child,
        uid: u32,
        scope: RotationScope,
    ) -> Result<Self, (PeerIdentityError, std::process::Child)> {
        if scope.operation == [0; 16] || scope.instance == [0; 16] || scope.boot == [0; 16] {
            return Err((PeerIdentityError::InvalidScope, child));
        }
        Ok(Self {
            child,
            uid,
            scope,
            exited: false,
        })
    }

    /// Return ownership to the caller, which remains responsible for reaping it.
    pub fn into_child(self) -> std::process::Child {
        self.child
    }

    /// Authenticate a connected peer as this currently supervised child for
    /// the supervisor's own expected scope. Does not verify OAuth adoption.
    pub fn verify_connection(
        &mut self,
        stream: &UnixStream,
        expected: RotationScope,
    ) -> Result<(), PeerIdentityError> {
        if self.scope != expected {
            return Err(PeerIdentityError::ScopeMismatch);
        }
        self.require_running()?;
        verify_peer(stream, self.uid, self.child.id())?;
        self.require_running()
    }

    /// Check the supervised connection and the local rotation snapshot. The
    /// result is momentary and does not authenticate a login or prove adoption.
    pub fn verify_rotation_connection(
        &mut self,
        stream: &UnixStream,
        store: &crate::channels::ChannelStore,
        expected: RotationScope,
    ) -> Result<(), PeerIdentityError> {
        self.verify_connection(stream, expected)?;
        store
            .check_rotation_process_epoch(expected.operation, expected.instance, expected.boot)
            .map_err(|_| PeerIdentityError::TrackingUnavailable)?;
        self.verify_connection(stream, expected)
    }

    fn require_running(&mut self) -> Result<(), PeerIdentityError> {
        if self.exited {
            return Err(PeerIdentityError::ChildExited);
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.exited = true;
                Err(PeerIdentityError::ChildExited)
            }
            Ok(None) => Ok(()),
            Err(_) => Err(PeerIdentityError::ChildStatusUnavailable),
        }
    }
}

/// Check credentials captured by the kernel for this connected Unix socket.
/// The result is scoped to the connection and must not be promoted to proof of
/// credential adoption, revocation status, or a durable workload identity.
pub fn verify_peer(
    stream: &UnixStream,
    expected_uid: u32,
    expected_pid: u32,
) -> Result<(), PeerIdentityError> {
    if expected_pid == 0 || expected_pid > i32::MAX as u32 {
        return Err(PeerIdentityError::InvalidExpectedPid);
    }
    let peer = stream
        .peer_cred()
        .map_err(|_| PeerIdentityError::CredentialsUnavailable)?;
    if peer.uid() != expected_uid || peer.pid() != Some(expected_pid as i32) {
        return Err(PeerIdentityError::Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod message_tests {
    use super::*;
    use crate::channels::{ChannelStore, EmbeddedSecret};
    use std::{
        os::fd::OwnedFd,
        os::unix::fs::MetadataExt,
        process::{Command, Stdio},
        time::Duration,
    };

    #[test]
    fn missing_credentials_and_truncated_ancillary_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChannelStore::open(&dir.path().join("fixture.db"), &[111; 32]).unwrap();
        let scope = RotationScope {
            operation: [41; 16],
            instance: [42; 16],
            boot: [43; 16],
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
        for missing in [true, false] {
            let (receiver, sender) = MessageReceiver::pair().unwrap();
            if missing {
                // Test-only fault injection: the production constructor enables this before publication.
                sockopt::set_socket_passcred(&receiver.0, false).unwrap();
            }
            let script = if missing {
                "import socket,time\ns=socket.socket(fileno=0)\ns.send(b'x')\ntime.sleep(2)"
            } else {
                "import socket,time,array\ns=socket.socket(fileno=0)\nf=array.array('i',[0]*8)\ns.sendmsg([b'x'],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,f)])\ns.sendmsg([b'x'],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,f)])\ntime.sleep(2)"
            };
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
            let mut payload = [0; 8];
            if !missing {
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
                let mut ancillary = RecvAncillaryBuffer::new(&mut space);
                let mut result = None;
                for _ in 0..100 {
                    match recvmsg(
                        &receiver.0,
                        &mut [IoSliceMut::new(&mut payload)],
                        &mut ancillary,
                        RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
                    ) {
                        Ok(value) => {
                            result = Some(value);
                            break;
                        }
                        Err(rustix::io::Errno::AGAIN) => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(error) => panic!("recvmsg failed: {error}"),
                    }
                }
                let result = result.expect("ancillary fixture timed out");
                assert!(result.flags.contains(ReturnFlags::CTRUNC));
                for message in ancillary.drain() {
                    drop(message);
                }
            }
            let mut result = Err(PeerIdentityError::MessageUnavailable);
            for _ in 0..100 {
                result =
                    supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload);
                if result != Err(PeerIdentityError::MessageUnavailable) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut child = supervisor.into_child();
            let _ = child.kill();
            let _ = child.wait();
            assert_eq!(
                result,
                Err(PeerIdentityError::InvalidMessage),
                "missing={missing}"
            );
        }
    }
}

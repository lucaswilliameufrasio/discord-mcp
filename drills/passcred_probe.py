#!/usr/bin/env python3
"""Linux-only synthetic probe, not an adoption authentication implementation."""
import os
import array
import errno
import socket
import struct


def receive(sock, size=64, ancillary_size=None):
    if ancillary_size is None:
        ancillary_size = socket.CMSG_SPACE(struct.calcsize("3i"))
    data, ancillary, flags, _ = sock.recvmsg(size, ancillary_size)
    if flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC):
        raise ValueError("truncated message or ancillary data")
    credentials = [
        struct.unpack("3i", value)
        for level, kind, value in ancillary
        if level == socket.SOL_SOCKET
        and kind == socket.SCM_CREDENTIALS
        and len(value) == struct.calcsize("3i")
    ]
    if len(credentials) != 1:
        raise ValueError("missing or ambiguous credentials")
    return data, credentials[0]


def main():
    no_cred_receiver, no_cred_sender = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    no_cred_receiver.settimeout(5)
    no_cred_sender.sendall(b"no credentials")
    try:
        receive(no_cred_receiver)
        raise AssertionError("missing credentials accepted")
    except ValueError as error:
        assert str(error) == "missing or ambiguous credentials"
    no_cred_receiver.close()
    no_cred_sender.close()

    receiver, sender = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    receiver.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
    receiver.settimeout(5)
    expected_pid, expected_uid = os.getpid(), os.getuid()
    sender.sendall(b"parent")
    data, (pid, uid, _) = receive(receiver)
    assert data == b"parent" and (pid, uid) == (expected_pid, expected_uid)

    child = os.fork()
    if child == 0:
        try:
            receiver.close()
            sender.sendall(b"child")
            os._exit(0)
        except BaseException:
            os._exit(1)
    try:
        data, (pid, uid, _) = receive(receiver)
        assert data == b"child" and pid == child and uid == expected_uid
        assert pid != expected_pid
        _, status = os.waitpid(child, 0)
        assert os.waitstatus_to_exitcode(status) == 0
    finally:
        # Reap on failure as well; the child sends only one short packet.
        try:
            os.waitpid(child, 0)
        except ChildProcessError:
            pass

    control_parent, control_child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    control_parent.settimeout(5)
    control_child.settimeout(5)
    child = os.fork()
    if child == 0:
        try:
            control_parent.close()
            receiver.close()
            sender.close()  # Only use the descriptor received over SCM_RIGHTS.
            _, ancillary, flags, _ = control_child.recvmsg(1, socket.CMSG_SPACE(array.array("i").itemsize))
            if flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC) or len(ancillary) != 1:
                os._exit(2)
            level, kind, payload = ancillary[0]
            fds = array.array("i")
            fds.frombytes(payload)
            if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS or len(fds) != 1:
                os._exit(3)
            passed = socket.socket(fileno=fds[0])
            try:
                passed.sendmsg([b"forged"], [(socket.SOL_SOCKET, socket.SCM_CREDENTIALS,
                                             struct.pack("3i", expected_pid, os.getuid(), os.getgid()))])
                os._exit(4)  # A privileged sender might be able to forge this PID.
            except OSError as error:
                if error.errno != errno.EPERM:
                    os._exit(5)
            passed.sendall(b"passed-fd")
            passed.close()
            os._exit(0)
        except BaseException:
            os._exit(6)
    control_child.close()
    try:
        fds = array.array("i", [sender.fileno()])
        control_parent.sendmsg([b"x"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, fds)])
        data, (pid, uid, _) = receive(receiver)
        assert data == b"passed-fd" and (pid, uid) == (child, expected_uid)
        assert pid != expected_pid
        _, status = os.waitpid(child, 0)
        assert os.waitstatus_to_exitcode(status) == 0
    finally:
        control_parent.close()
        try:
            os.waitpid(child, 0)
        except ChildProcessError:
            pass

    sender.sendall(b"long-message")
    try:
        receive(receiver, size=1)
        raise AssertionError("payload truncation accepted")
    except ValueError:
        pass
    sender.sendall(b"credentials")
    try:
        receive(receiver, ancillary_size=0)
        raise AssertionError("ancillary truncation accepted")
    except ValueError:
        pass
    sender.close()
    receiver.close()
    print("PASS: inherited and SCM_RIGHTS descriptors identify sender; missing credentials, spoofed PID and truncation rejected")


if __name__ == "__main__":
    main()

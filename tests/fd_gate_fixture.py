"""Linux-only research fixture, not an application gate or recovery protocol."""
import array
import fcntl
import os
import select
import socket
import subprocess
import sys
import tempfile
import unittest


def try_exclusive(path):
    fd = os.open(path, os.O_RDWR)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return fd
    except BlockingIOError:
        os.close(fd)
        return None


def send_lock(sock, fd):
    sock.sendmsg([b"L"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [fd]))])


def read_line_with_timeout(stream):
    ready, _, _ = select.select([stream], [], [], 5)
    if not ready:
        raise TimeoutError("fixture child did not respond")
    return stream.readline().strip()


def receiver(fd):
    sock = socket.socket(fileno=fd)
    sock.settimeout(5)
    data, ancillary, flags, _ = sock.recvmsg(1, socket.CMSG_SPACE(array.array("i").itemsize))
    if data != b"L" or flags & socket.MSG_CTRUNC:
        raise RuntimeError("invalid transfer")
    descriptors = array.array("i")
    for level, kind, payload in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            descriptors.frombytes(payload[:len(payload) - len(payload) % descriptors.itemsize])
    if len(descriptors) != 1:
        raise RuntimeError("expected exactly one descriptor")
    print("RECEIVED", flush=True)
    if sys.stdin.readline().strip() != "disconnect":
        raise RuntimeError("expected disconnect")
    sock.close()
    print("DISCONNECTED", flush=True)
    if sys.stdin.readline().strip() != "release":
        raise RuntimeError("expected release")
    os.close(descriptors[0])


class TransferTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = os.path.join(self.directory.name, "capture.lock")
        open(self.path, "wb").close()

    def test_transferred_lock_outlives_sender_and_socket_disconnect(self):
        sender, receiver_sock = socket.socketpair()
        self.addCleanup(sender.close)
        self.addCleanup(receiver_sock.close)
        child = subprocess.Popen([sys.executable, __file__, "receiver", str(receiver_sock.fileno())],
                                 pass_fds=(receiver_sock.fileno(),), stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, text=True)
        def cleanup_child():
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)
        self.addCleanup(cleanup_child)
        receiver_sock.close()
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        send_lock(sender, fd)
        os.close(fd)
        self.assertEqual(read_line_with_timeout(child.stdout), "RECEIVED")
        self.assertIsNone(try_exclusive(self.path))
        child.stdin.write("disconnect\n")
        child.stdin.flush()
        self.assertEqual(read_line_with_timeout(child.stdout), "DISCONNECTED")
        sender.close()
        self.assertIsNone(try_exclusive(self.path))
        child.stdin.write("release\n")
        child.stdin.flush()
        self.assertEqual(child.wait(timeout=5), 0)
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        os.close(fd)

    def test_disconnected_receiver_rejects_transfer(self):
        sender, receiver_sock = socket.socketpair()
        self.addCleanup(sender.close)
        receiver_sock.close()
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        try:
            with self.assertRaises((BrokenPipeError, ConnectionResetError)):
                send_lock(sender, fd)
            self.assertIsNone(try_exclusive(self.path))
        finally:
            os.close(fd)
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        os.close(fd)

    def test_receiver_death_releases_transferred_lock(self):
        sender, receiver_sock = socket.socketpair()
        self.addCleanup(sender.close)
        self.addCleanup(receiver_sock.close)
        child = subprocess.Popen([sys.executable, __file__, "receiver", str(receiver_sock.fileno())],
                                 pass_fds=(receiver_sock.fileno(),), stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, text=True)
        def cleanup_child():
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)
        self.addCleanup(cleanup_child)
        receiver_sock.close()
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        send_lock(sender, fd)
        os.close(fd)
        self.assertEqual(read_line_with_timeout(child.stdout), "RECEIVED")
        self.assertIsNone(try_exclusive(self.path))
        child.kill()
        child.wait(timeout=5)
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        os.close(fd)

    def test_drain_barrier_blocks_transfer_until_writer_releases(self):
        writer = os.open(self.path, os.O_RDWR)
        fcntl.flock(writer, fcntl.LOCK_SH)
        self.assertIsNone(try_exclusive(self.path))
        os.close(writer)
        fd = try_exclusive(self.path)
        self.assertIsNotNone(fd)
        os.close(fd)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "receiver":
        receiver(int(sys.argv[2]))
    else:
        unittest.main(verbosity=2)

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


def authority(socket_fd, directory):
    """Single-client synthetic authority; not an operational implementation."""
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    capture_path = os.path.join(directory, "capture.lock")
    activity_path = os.path.join(directory, "activity.lock")
    marker_path = os.path.join(directory, "closed-marker")
    with open(marker_path, "wb") as marker:
        marker.write(b"closed\n")
        marker.flush()
        os.fsync(marker.fileno())
    sock.sendall(b"R")
    opened = False
    while True:
        command = sock.recv(1)
        if not command:
            return
        if command == b"O":
            fd = try_exclusive(capture_path)
            if fd is None:
                sock.sendall(b"B")
            else:
                os.close(fd)
                opened = True
                sock.sendall(b"O")
        elif command == b"W":
            sock.sendall(b"Y" if opened else b"N")
        elif command == b"C":
            opened = False
            with open(marker_path, "wb") as marker:
                marker.write(b"closed\n")
                marker.flush()
                os.fsync(marker.fileno())
            activity = try_exclusive(activity_path)
            if activity is None:
                sock.sendall(b"B")
                continue
            try:
                capture = try_exclusive(capture_path)
                if capture is None:
                    sock.sendall(b"B")
                    continue
                try:
                    send_lock(sock, capture)
                finally:
                    os.close(capture)
            finally:
                os.close(activity)
            # No reopening on EOF or lost ACK; receiver may still hold the fd.
            if sock.recv(1) != b"A":
                return
            sock.sendall(b"K")
        else:
            raise RuntimeError("unexpected fixture command")


def receive_lock(sock):
    data, ancillary, flags, _ = sock.recvmsg(1, socket.CMSG_SPACE(array.array("i").itemsize))
    if data != b"L" or flags & socket.MSG_CTRUNC:
        raise RuntimeError("invalid authority transfer")
    descriptors = array.array("i")
    for level, kind, payload in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            descriptors.frombytes(payload[:len(payload) - len(payload) % descriptors.itemsize])
    if len(descriptors) != 1:
        raise RuntimeError("expected one descriptor")
    return descriptors[0]


class AuthorityTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        for name in ("capture.lock", "activity.lock", "closed-marker"):
            open(os.path.join(self.directory.name, name), "wb").close()
        self.children = []
        self.sockets = []
        self.addCleanup(self.cleanup_processes)

    def cleanup_processes(self):
        for sock in self.sockets:
            sock.close()
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)

    def start_authority(self):
        client, peer = socket.socketpair()
        client.settimeout(5)
        child = subprocess.Popen([sys.executable, __file__, "authority", str(peer.fileno()), self.directory.name],
                                 pass_fds=(peer.fileno(),), stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        peer.close()
        self.children.append(child)
        self.sockets.append(client)
        self.assertEqual(client.recv(1), b"R")
        return child, client

    def command(self, sock, command):
        sock.sendall(command)
        return sock.recv(1)

    def test_restart_keeps_admission_closed_until_transferred_lock_released(self):
        child, sock = self.start_authority()
        self.assertEqual(self.command(sock, b"O"), b"O")
        sock.sendall(b"C")
        fd = receive_lock(sock)
        self.assertEqual(self.command(sock, b"A"), b"K")
        child.kill()
        child.wait(timeout=5)
        _, restarted = self.start_authority()
        self.assertEqual(self.command(restarted, b"W"), b"N")
        self.assertEqual(self.command(restarted, b"O"), b"B")
        self.assertEqual(self.command(restarted, b"W"), b"N")
        os.close(os.dup(fd))  # unrelated duplicate closure does not release fd
        self.assertEqual(self.command(restarted, b"O"), b"B")
        os.close(fd)
        self.assertEqual(self.command(restarted, b"O"), b"O")

    def test_lost_ack_and_drain_retry(self):
        child, sock = self.start_authority()
        self.assertEqual(self.command(sock, b"O"), b"O")
        writer = os.open(os.path.join(self.directory.name, "activity.lock"), os.O_RDWR)
        fcntl.flock(writer, fcntl.LOCK_SH)
        self.assertEqual(self.command(sock, b"C"), b"B")
        self.assertEqual(self.command(sock, b"W"), b"N")
        os.close(writer)
        sock.sendall(b"C")
        fd = receive_lock(sock)
        # Client disconnects without sending ACK but keeps the descriptor.
        sock.close()
        self.assertEqual(child.wait(timeout=5), 0)
        _, restarted = self.start_authority()
        self.assertEqual(self.command(restarted, b"O"), b"B")
        self.assertEqual(self.command(restarted, b"W"), b"N")
        os.close(fd)
        self.assertEqual(self.command(restarted, b"O"), b"O")


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
    elif len(sys.argv) == 4 and sys.argv[1] == "authority":
        authority(int(sys.argv[2]), sys.argv[3])
    else:
        unittest.main(verbosity=2)

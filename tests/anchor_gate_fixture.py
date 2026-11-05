"""Linux-only descriptor-anchor experiment. Not an operational gate."""
import fcntl
import array
import os
import select
import socket
import subprocess
import sys
import tempfile
import unittest


def child(role, fd):
    if role == "writer":
        fcntl.flock(fd, fcntl.LOCK_SH)
        print("HOLDING", flush=True)
        if sys.stdin.readline().strip() != "release":
            raise RuntimeError("expected release")
    elif role == "keeper":
        print("ANCHORED", flush=True)
        sys.stdin.readline()
    elif role == "guard":
        print("OPEN" if os.read(fd, 64) == b"clean\n" else "CLOSED", flush=True)
    elif role == "restart":
        # No marker value alone is sufficient to prove a safe recovery.
        print("CLOSED", flush=True)
    else:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("BUSY", flush=True)
        else:
            print("ACQUIRED", flush=True)
    os.close(fd)


def publisher(sock_fd, directory, failure):
    sock = socket.socket(fileno=sock_fd)
    state_path = os.path.join(directory, "state")
    with open(state_path, "r+b", buffering=0) as state:
        if failure == "before_write":
            return
        state.seek(0)
        state.write(b"uncertain\n")
        state.truncate()
        if failure == "before_sync":
            return
        os.fsync(state.fileno())
        fd = os.open(os.path.join(directory, "activity.lock"), os.O_RDWR)
        try:
            sock.sendmsg([b"F"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [fd]))])
            if sys.stdin.readline().strip() != "next":
                raise RuntimeError("expected next")
            # Synthetic failure during a second persistence step: no second
            # descriptor is sent, and the first recipient remains independent.
            state.seek(0)
            state.write(b"uncertain\n")
            if failure == "during_distribution":
                return
            raise RuntimeError("unexpected mode")
        finally:
            os.close(fd)


def receive_descriptor(sock):
    data, ancillary, flags, _ = sock.recvmsg(1, socket.CMSG_SPACE(array.array("i").itemsize))
    if not data:
        return None
    if data != b"F" or flags & socket.MSG_CTRUNC:
        raise RuntimeError("invalid descriptor transfer")
    fds = array.array("i")
    for level, kind, payload in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            fds.frombytes(payload[:len(payload) - len(payload) % fds.itemsize])
    if len(fds) != 1:
        for fd in fds:
            os.close(fd)
        raise RuntimeError("expected exactly one descriptor")
    return fds[0]


def reconcile_known_children(anchor, writers, state_path, fail_before_sync=False):
    """Closed-world test only: caller owns admission, handles and anchor."""
    if any(writer.poll() is None for writer in writers):
        return False
    fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        with open(state_path, "r+b", buffering=0) as state:
            state.seek(0)
            state.write(b"clean\n")
            state.truncate()
            if fail_before_sync:
                return False
            os.fsync(state.fileno())
        return True
    finally:
        os.close(fd)


def line(stream):
    ready, _, _ = select.select([stream], [], [], 5)
    if not ready:
        raise TimeoutError("fixture child timed out")
    result = stream.readline().strip()
    if not result:
        raise RuntimeError("fixture child exited without result")
    return result


class AnchorTests(unittest.TestCase):
    def test_reconcile_tracked_writers_only_after_anchor_holder_dies(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            state_path = os.path.join(directory, "state")
            with open(path, "wb"):
                pass
            with open(state_path, "wb") as state:
                state.write(b"uncertain\n")
                state.flush()
                os.fsync(state.fileno())
            anchor = os.open(path, os.O_RDWR)
            children = []

            def cleanup():
                for process in children:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                os.close(anchor)

            self.addCleanup(cleanup)

            def spawn(role):
                fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    process = subprocess.Popen(
                        [sys.executable, __file__, role, str(fd)],
                        pass_fds=(fd,), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(fd)
                children.append(process)
                return process

            keeper = spawn("keeper")
            self.assertEqual(line(keeper.stdout), "ANCHORED")
            writers = [spawn("writer"), spawn("writer")]
            for writer in writers:
                self.assertEqual(line(writer.stdout), "HOLDING")
            replacement = os.path.join(directory, "replacement")
            with open(replacement, "wb"):
                pass
            os.replace(replacement, path)
            keeper.kill()
            keeper.wait(timeout=5)
            self.assertFalse(reconcile_known_children(anchor, writers, state_path))
            writers[0].stdin.write("release\n")
            writers[0].stdin.flush()
            self.assertEqual(writers[0].wait(timeout=5), 0)
            self.assertFalse(reconcile_known_children(anchor, writers, state_path))
            writers[1].stdin.write("release\n")
            writers[1].stdin.flush()
            self.assertEqual(writers[1].wait(timeout=5), 0)

            # A live lock not represented by the tracked children still blocks.
            extra = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            try:
                fcntl.flock(extra, fcntl.LOCK_SH)
                self.assertFalse(reconcile_known_children(anchor, writers, state_path))
            finally:
                os.close(extra)
            self.assertFalse(reconcile_known_children(anchor, writers, state_path, True))
            # A visible clean byte before sync is not permission to reopen.
            with open(state_path, "rb") as state:
                self.assertEqual(state.read(), b"clean\n")
            state_fd = os.open(state_path, os.O_RDONLY)
            try:
                restarted = subprocess.run(
                    [sys.executable, __file__, "restart", str(state_fd)],
                    pass_fds=(state_fd,), capture_output=True, text=True,
                    timeout=5, check=True,
                )
            finally:
                os.close(state_fd)
            self.assertEqual(restarted.stdout.strip(), "CLOSED")
            self.assertTrue(reconcile_known_children(anchor, writers, state_path))

    def test_persistence_failures_and_partial_distribution_restart_closed(self):
        for failure in ("before_write", "before_sync", "during_distribution"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                path = os.path.join(directory, "activity.lock")
                state_path = os.path.join(directory, "state")
                with open(path, "wb"):
                    pass
                with open(state_path, "wb") as state:
                    state.write(b"clean\n")
                    state.flush()
                    os.fsync(state.fileno())
                parent, peer = socket.socketpair()
                parent.settimeout(5)
                proc = subprocess.Popen(
                    [sys.executable, __file__, "publisher", str(peer.fileno()), directory, failure],
                    pass_fds=(peer.fileno(),), stdin=subprocess.PIPE,
                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
                )
                peer.close()
                writer = None
                try:
                    fd = receive_descriptor(parent)
                    if failure == "during_distribution":
                        self.assertIsNotNone(fd)
                        try:
                            writer = subprocess.Popen(
                                [sys.executable, __file__, "writer", str(fd)],
                                pass_fds=(fd,), stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            )
                        finally:
                            os.close(fd)
                        self.assertEqual(line(writer.stdout), "HOLDING")
                        proc.stdin.write("next\n")
                        proc.stdin.flush()
                        self.assertIsNone(receive_descriptor(parent))
                    else:
                        self.assertIsNone(fd)
                    self.assertEqual(proc.wait(timeout=5), 0)
                    with open(state_path, "rb") as state:
                        expected = b"clean\n" if failure == "before_write" else b"uncertain\n"
                        self.assertEqual(state.read(), expected)
                    state_fd = os.open(state_path, os.O_RDONLY)
                    try:
                        restart = subprocess.run(
                            [sys.executable, __file__, "restart", str(state_fd)],
                            pass_fds=(state_fd,), capture_output=True, text=True, timeout=5,
                            check=True,
                        )
                    finally:
                        os.close(state_fd)
                    self.assertEqual(restart.stdout.strip(), "CLOSED")
                    if writer is not None:
                        self.assertIsNone(writer.poll())
                        writer.stdin.write("release\n")
                        writer.stdin.flush()
                        self.assertEqual(writer.wait(timeout=5), 0)
                finally:
                    parent.close()
                    for process in (proc, writer):
                        if process is not None:
                            if process.poll() is None:
                                process.kill()
                            process.communicate(timeout=5)

    def test_anchor_death_keeps_restart_closed_with_old_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            marker = os.path.join(directory, "state")
            with open(path, "wb"):
                pass
            with open(marker, "wb") as state:
                state.write(b"clean\n")
                state.flush()
                os.fsync(state.fileno())
            children = []

            def cleanup():
                for process in children:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

            self.addCleanup(cleanup)

            def spawn(role, fd):
                process = subprocess.Popen(
                    [sys.executable, __file__, role, str(fd)],
                    pass_fds=(fd,), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                children.append(process)
                return process

            def guarded_restart():
                fd = os.open(marker, os.O_RDONLY)
                try:
                    process = spawn("guard", fd)
                finally:
                    os.close(fd)
                result = line(process.stdout)
                self.assertEqual(process.wait(timeout=5), 0)
                return result

            self.assertEqual(guarded_restart(), "OPEN")
            # The fixture assumes this state is durably written before any
            # descriptor is distributed. There is no clean transition here.
            with open(marker, "wb") as state:
                state.write(b"uncertain\n")
                state.flush()
                os.fsync(state.fileno())
            anchor = os.open(path, os.O_RDWR)
            keeper = spawn("keeper", anchor)
            self.assertEqual(line(keeper.stdout), "ANCHORED")
            writer_fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            writer = spawn("writer", writer_fd)
            os.close(writer_fd)
            self.assertEqual(line(writer.stdout), "HOLDING")
            os.close(anchor)
            replacement = os.path.join(directory, "replacement")
            with open(replacement, "wb"):
                pass
            os.replace(replacement, path)
            keeper.kill()
            keeper.wait(timeout=5)
            self.assertIsNone(writer.poll())
            # Locking the replacement is possible but must not clear the state.
            replacement_fd = os.open(path, os.O_RDWR)
            try:
                fcntl.flock(replacement_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertEqual(guarded_restart(), "CLOSED")
            finally:
                os.close(replacement_fd)
            writer.stdin.write("release\n")
            writer.stdin.flush()
            self.assertEqual(writer.wait(timeout=5), 0)
            self.assertEqual(guarded_restart(), "CLOSED")
            with open(marker, "rb") as state:
                self.assertEqual(state.read(), b"uncertain\n")

    def test_anchor_keeps_all_participants_on_original_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            with open(path, "wb"):
                pass
            anchor = os.open(path, os.O_RDWR)
            self.addCleanup(os.close, anchor)
            children = []

            def cleanup():
                for process in children:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

            self.addCleanup(cleanup)

            def start(role):
                # Reopen the pinned inode, not its replaceable pathname. A
                # separate open-file description is essential for flock.
                fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                self.assertEqual(os.fstat(fd).st_ino, os.fstat(anchor).st_ino)
                try:
                    process = subprocess.Popen(
                        [sys.executable, __file__, role, str(fd)],
                        pass_fds=(fd,), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(fd)
                children.append(process)
                return process

            first = start("writer")
            self.assertEqual(line(first.stdout), "HOLDING")
            replacement = os.path.join(directory, "replacement")
            with open(replacement, "wb"):
                pass
            os.replace(replacement, path)
            self.assertNotEqual(os.stat(path).st_ino, os.fstat(anchor).st_ino)

            # A new writer and two independently started authorities continue
            # using the anchored inode despite the pathname replacement.
            second = start("writer")
            self.assertEqual(line(second.stdout), "HOLDING")
            for _ in range(2):
                authority = start("authority")
                self.assertEqual(line(authority.stdout), "BUSY")
                self.assertEqual(authority.wait(timeout=5), 0)
            first.stdin.write("release\n")
            first.stdin.flush()
            self.assertEqual(first.wait(timeout=5), 0)
            authority = start("authority")
            self.assertEqual(line(authority.stdout), "BUSY")
            self.assertEqual(authority.wait(timeout=5), 0)
            second.stdin.write("release\n")
            second.stdin.flush()
            self.assertEqual(second.wait(timeout=5), 0)
            authority = start("authority")
            self.assertEqual(line(authority.stdout), "ACQUIRED")
            self.assertEqual(authority.wait(timeout=5), 0)


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "publisher":
        publisher(int(sys.argv[2]), sys.argv[3], sys.argv[4])
    elif len(sys.argv) == 3 and sys.argv[1] in ("writer", "authority", "keeper", "guard", "restart"):
        child(sys.argv[1], int(sys.argv[2]))
    else:
        unittest.main(verbosity=2)

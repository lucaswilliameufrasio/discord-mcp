"""Linux-only descriptor-anchor experiment. Not an operational gate."""
import fcntl
import array
import os
import select
import socket
import sqlite3
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


def transition_authority(control_fd, writer_fd, barrier_fd, state_path):
    control = socket.socket(fileno=control_fd)
    writer = socket.socket(fileno=writer_fd)
    opened = False
    held = False
    prepared = False
    while True:
        ready, _, _ = select.select([control, writer], [], [], 5)
        if not ready:
            raise TimeoutError("transition fixture timed out")
        if control in ready:
            command = control.recv(1)
            if not command:
                return
            if command == b"D":
                try:
                    fcntl.flock(barrier_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    control.sendall(b"B")
                else:
                    held = True
                    control.sendall(b"D")
            elif command == b"P" and held:
                with open(state_path, "r+b", buffering=0) as state:
                    state.seek(0)
                    state.write(b"clean\n")
                    state.truncate()
                    os.fsync(state.fileno())
                prepared = True
                control.sendall(b"P")
            elif command == b"O" and held and prepared:
                # This single event loop cannot process writer requests between
                # unlock and its in-memory admission transition.
                fcntl.flock(barrier_fd, fcntl.LOCK_UN)
                held = False
                opened = True
                control.sendall(b"O")
            else:
                control.sendall(b"N")
        if writer in ready:
            request = writer.recv(1)
            if not request:
                return
            if request != b"W":
                raise RuntimeError("unexpected writer request")
            writer.sendall(b"Y" if opened else b"N")


def transition_contender(fd):
    sock = socket.socket(fileno=fd)
    sock.settimeout(5)
    for command in sys.stdin:
        if command.strip() != "probe":
            raise RuntimeError("unexpected contender command")
        sock.sendall(b"W")
        print(sock.recv(1).decode(), flush=True)


def sqlite_writer(fd, database):
    fcntl.flock(fd, fcntl.LOCK_SH)
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    print("TRANSACTION", flush=True)
    if sys.stdin.readline().strip() != "commit":
        raise RuntimeError("expected commit")
    connection.commit()
    connection.close()
    os.close(fd)


def admitted_sqlite_writer(fd, socket_fd, database):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    fcntl.flock(fd, fcntl.LOCK_SH)
    sock.sendall(b"W")
    if sock.recv(1) != b"Y":
        raise RuntimeError("admission denied")
    print("ADMITTED", flush=True)
    if sys.stdin.readline().strip() != "begin":
        raise RuntimeError("expected begin")
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    connection.commit()
    connection.close()
    os.close(fd)
    sock.close()


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
    def test_admitted_sqlite_writer_retains_barrier_after_authority_death(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            state = os.path.join(directory, "state")
            database = os.path.join(directory, "synthetic.db")
            with open(path, "wb"), open(state, "wb") as marker:
                marker.write(b"uncertain\n")
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            anchor = os.open(path, os.O_RDWR)
            processes = []
            sockets = []

            def cleanup():
                for sock in sockets:
                    sock.close()
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                os.close(anchor)

            self.addCleanup(cleanup)

            def start_authority():
                control, control_peer = socket.socketpair()
                writer, writer_peer = socket.socketpair()
                control.settimeout(5)
                writer.settimeout(5)
                barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    process = subprocess.Popen(
                        [sys.executable, __file__, "transition", str(control_peer.fileno()),
                         str(writer_peer.fileno()), str(barrier), state],
                        pass_fds=(control_peer.fileno(), writer_peer.fileno(), barrier),
                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                    )
                finally:
                    control_peer.close()
                    writer_peer.close()
                    os.close(barrier)
                processes.append(process)
                sockets.extend((control, writer))
                return process, control, writer

            def command(sock, value):
                sock.sendall(value)
                return sock.recv(1)

            authority, control, writer_socket = start_authority()
            self.assertEqual(command(control, b"D"), b"D")
            self.assertEqual(command(control, b"P"), b"P")
            self.assertEqual(command(control, b"O"), b"O")
            writer_fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            try:
                writer = subprocess.Popen(
                    [sys.executable, __file__, "admitted_sqlite_writer", str(writer_fd),
                     str(writer_socket.fileno()), database],
                    pass_fds=(writer_fd, writer_socket.fileno()), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
            finally:
                os.close(writer_fd)
                writer_socket.close()
            processes.append(writer)
            self.assertEqual(line(writer.stdout), "ADMITTED")
            authority.kill()
            authority.wait(timeout=5)
            restarted, new_control, new_writer_socket = start_authority()
            self.assertEqual(command(new_writer_socket, b"W"), b"N")
            self.assertEqual(command(new_control, b"D"), b"B")
            with sqlite3.connect(database) as db:
                self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 0)
            # The old authority's death does not revoke the admitted client;
            # its held lock is what prevents the new authority from draining.
            writer.stdin.write("begin\n")
            writer.stdin.flush()
            self.assertEqual(writer.wait(timeout=5), 0)
            with sqlite3.connect(database) as db:
                self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
            self.assertEqual(command(new_control, b"D"), b"D")
            self.assertEqual(command(new_control, b"P"), b"P")
            self.assertEqual(command(new_control, b"O"), b"O")
            self.assertEqual(command(new_writer_socket, b"W"), b"Y")
            new_control.close()
            restarted.wait(timeout=5)

    def test_sqlite_writer_reconciles_before_transition_and_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            state = os.path.join(directory, "state")
            with open(path, "wb"):
                pass
            with open(state, "wb") as marker:
                marker.write(b"uncertain\n")
            anchor = os.open(path, os.O_RDWR)
            database = os.path.join(directory, "synthetic.db")
            with sqlite3.connect(database) as db:
                db.execute("PRAGMA journal_mode=WAL")
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            processes = []
            sockets = []

            def cleanup():
                for sock in sockets:
                    sock.close()
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                os.close(anchor)

            self.addCleanup(cleanup)

            def start_authority():
                control, control_peer = socket.socketpair()
                writer, writer_peer = socket.socketpair()
                control.settimeout(5)
                barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    authority = subprocess.Popen(
                        [sys.executable, __file__, "transition", str(control_peer.fileno()),
                         str(writer_peer.fileno()), str(barrier), state],
                        pass_fds=(control_peer.fileno(), writer_peer.fileno(), barrier),
                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                    )
                finally:
                    control_peer.close()
                    writer_peer.close()
                    os.close(barrier)
                contender = subprocess.Popen(
                    [sys.executable, __file__, "contender", str(writer.fileno())],
                    pass_fds=(writer.fileno(),), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                writer.close()
                processes.extend((authority, contender))
                sockets.append(control)
                return authority, contender, control

            def probe(contender):
                contender.stdin.write("probe\n")
                contender.stdin.flush()
                return line(contender.stdout)

            def command(control, value):
                control.sendall(value)
                return control.recv(1)

            def start_sqlite_writer():
                fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    process = subprocess.Popen(
                        [sys.executable, __file__, "sqlite_writer", str(fd), database],
                        pass_fds=(fd,), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(fd)
                processes.append(process)
                self.assertEqual(line(process.stdout), "TRANSACTION")
                return process

            old_writer = start_sqlite_writer()
            authority, contender, control = start_authority()
            self.assertEqual(probe(contender), "N")
            self.assertEqual(command(control, b"D"), b"B")
            authority.kill()
            authority.wait(timeout=5)
            contender.stdin.close()
            contender.stdin = None
            contender.wait(timeout=5)
            authority, contender, control = start_authority()
            self.assertEqual(probe(contender), "N")
            self.assertIsNone(old_writer.poll())
            self.assertEqual(command(control, b"D"), b"B")
            old_writer.stdin.write("commit\n")
            old_writer.stdin.flush()
            self.assertEqual(old_writer.wait(timeout=5), 0)
            with sqlite3.connect(database) as db:
                self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
            # The test supervisor owns the process handle; no new writer is
            # created without its admission decision.
            self.assertIsNotNone(old_writer.poll())
            self.assertEqual(command(control, b"D"), b"D")
            self.assertEqual(command(control, b"P"), b"P")
            self.assertEqual(probe(contender), "N")
            competing = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            try:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(competing, fcntl.LOCK_SH | fcntl.LOCK_NB)
            finally:
                os.close(competing)
            authority.kill()
            authority.wait(timeout=5)
            contender.stdin.close()
            contender.stdin = None
            contender.wait(timeout=5)
            restarted, later, restarted_control = start_authority()
            self.assertEqual(probe(later), "N")
            self.assertEqual(command(restarted_control, b"D"), b"D")
            self.assertEqual(command(restarted_control, b"P"), b"P")
            self.assertEqual(probe(later), "N")
            self.assertEqual(command(restarted_control, b"O"), b"O")
            self.assertEqual(probe(later), "Y")
            new_writer = start_sqlite_writer()
            new_writer.stdin.write("commit\n")
            new_writer.stdin.flush()
            self.assertEqual(new_writer.wait(timeout=5), 0)
            with sqlite3.connect(database) as db:
                self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 2)
            restarted_control.close()
            restarted.wait(timeout=5)

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
    if len(sys.argv) == 5 and sys.argv[1] == "admitted_sqlite_writer":
        admitted_sqlite_writer(int(sys.argv[2]), int(sys.argv[3]), sys.argv[4])
    elif len(sys.argv) == 4 and sys.argv[1] == "sqlite_writer":
        sqlite_writer(int(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) == 6 and sys.argv[1] == "transition":
        transition_authority(int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5])
    elif len(sys.argv) == 3 and sys.argv[1] == "contender":
        transition_contender(int(sys.argv[2]))
    elif len(sys.argv) == 5 and sys.argv[1] == "publisher":
        publisher(int(sys.argv[2]), sys.argv[3], sys.argv[4])
    elif len(sys.argv) == 3 and sys.argv[1] in ("writer", "authority", "keeper", "guard", "restart"):
        child(sys.argv[1], int(sys.argv[2]))
    else:
        unittest.main(verbosity=2)

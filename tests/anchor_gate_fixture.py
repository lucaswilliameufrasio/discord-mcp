"""Linux-only descriptor-anchor experiment. Not an operational gate."""
import fcntl
import array
import os
import select
import signal
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
    elif role == "independent_guard":
        fcntl.flock(fd, fcntl.LOCK_SH)
        print("GUARDED", flush=True)
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


def transition_authority(control_fd, writer_fd, barrier_fd, state_path, previous_pidfd=None,
                         complete=True):
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
                if not complete:
                    control.sendall(b"B")
                    continue
                if previous_pidfd is not None:
                    handles = previous_pidfd if isinstance(previous_pidfd, list) else [previous_pidfd]
                    if any(not select.select([handle], [], [], 0)[0] for handle in handles):
                        control.sendall(b"B")
                        continue
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


def spawn_descendant(database, detached=False):
    descendant = subprocess.Popen(
        [sys.executable, __file__, "descendant", database],
        stdin=sys.stdin, stdout=sys.stdout, stderr=sys.stderr,
        start_new_session=detached,
    )
    print(f"CHILD {descendant.pid}", flush=True)


def descendant_writer(database):
    if sys.stdin.readline().strip() != "begin":
        raise RuntimeError("expected begin")
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    print("DESCENDANT_WRITING", flush=True)
    if sys.stdin.readline().strip() != "commit":
        raise RuntimeError("expected commit")
    connection.commit()
    connection.close()


def inventory_writer(socket_fd, database):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    sock.sendall(b"T")
    if sock.recv(1) != b"C":
        raise RuntimeError("expected commit")
    connection.commit()
    connection.close()
    sock.close()


def inventory_launcher(socket_fd, directory, mode):
    # This fixture assumes this file was initialized previously and that this
    # launcher is the only entity permitted to start or admit writers.
    state_path = os.path.join(directory, "inventory-state")
    with open(state_path, "r+b", buffering=0) as state:
        state.seek(0)
        state.write(b"launching\n")
        state.truncate()
        if mode == "before_sync":
            print("NO_LAUNCH", flush=True)
            return
        os.fsync(state.fileno())
        print("INTENT_SYNCED", flush=True)
        if sys.stdin.readline().strip() != "spawn":
            raise RuntimeError("expected spawn")
        worker = subprocess.Popen(
            [sys.executable, __file__, "inventory_writer", str(socket_fd),
             os.path.join(directory, "synthetic.db")],
            pass_fds=(socket_fd,), stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        print(f"SPAWNED {worker.pid}", flush=True)
        # Deliberately stop before persisting a writer registration.
        sys.stdin.readline()


def registry_guard(path):
    try:
        connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=1)
        try:
            integrity = connection.execute("PRAGMA integrity_check").fetchone()
            epochs = connection.execute("SELECT state FROM epoch").fetchall()
            entries = connection.execute("SELECT token, state FROM writers").fetchall()
            valid = (integrity == ("ok",) and epochs == [("sealed",)] and bool(entries)
                     and all(token and state == "done" for token, state in entries))
        finally:
            connection.close()
    except sqlite3.Error:
        valid = False
    print("OPEN" if valid else "CLOSED", flush=True)


def registry_crash(path):
    connection = sqlite3.connect(path, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("UPDATE writers SET state='done' WHERE token='parent'")
    print("UNCOMMITTED", flush=True)
    sys.stdin.readline()
    connection.rollback()
    connection.close()


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


def grant_authority(socket_fd, barrier_fd):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    fcntl.flock(barrier_fd, fcntl.LOCK_SH)
    sock.sendmsg([b"F"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [barrier_fd]))])
    if sock.recv(1) != b"A":
        raise RuntimeError("missing receipt acknowledgment")
    # The receiver now holds a reference to the same locked description.
    os.close(barrier_fd)
    print("ACK", flush=True)
    sys.stdin.readline()


def transferred_sqlite_writer(socket_fd, database):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    fd = receive_descriptor(sock)
    if fd is None:
        raise RuntimeError("missing barrier")
    sock.sendall(b"A")
    print("ADMITTED", flush=True)
    if sys.stdin.readline().strip() != "disconnect":
        raise RuntimeError("expected disconnect")
    sock.close()
    print("DISCONNECTED", flush=True)
    if sys.stdin.readline().strip() != "commit":
        raise RuntimeError("expected commit")
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    connection.commit()
    connection.close()
    os.close(fd)


def early_release_writer(socket_fd, database, action):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    fd = receive_descriptor(sock)
    if fd is None:
        raise RuntimeError("missing barrier")
    sock.sendall(b"A")
    if sys.stdin.readline().strip() != "start":
        raise RuntimeError("expected start")
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    if action == "unlock":
        fcntl.flock(fd, fcntl.LOCK_UN)
    os.close(fd)
    print("RELEASED", flush=True)
    if sys.stdin.readline().strip() != "commit":
        raise RuntimeError("expected commit")
    connection.commit()
    connection.close()
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
    @unittest.skipUnless(hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"),
                         "Linux pidfd APIs are required")
    def test_detached_descendant_invalidates_process_group_completeness(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            state = os.path.join(directory, "state")
            database = os.path.join(directory, "synthetic.db")
            with open(path, "wb"), open(state, "wb") as marker:
                marker.write(b"uncertain\n")
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            parent = subprocess.Popen(
                [sys.executable, __file__, "spawn_detached", database],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, start_new_session=True,
            )
            child_handle = None
            parent_handle = os.pidfd_open(parent.pid)
            anchor = os.open(path, os.O_RDWR)
            control = control_peer = candidate = candidate_peer = None
            authority = None
            try:
                announcement = line(parent.stdout).split()
                self.assertEqual(announcement[0], "CHILD")
                child_pid = int(announcement[1])
                child_handle = os.pidfd_open(child_pid)
                self.assertEqual(parent.wait(timeout=5), 0)
                self.assertEqual(os.getpgid(child_pid), child_pid)
                self.assertNotEqual(child_pid, parent.pid)
                parent.stdin.write("begin\n")
                parent.stdin.flush()
                self.assertEqual(line(parent.stdout), "DESCENDANT_WRITING")
                self.assertTrue(select.select([parent_handle], [], [], 0)[0])
                self.assertFalse(select.select([child_handle], [], [], 0)[0])

                control, control_peer = socket.socketpair()
                candidate, candidate_peer = socket.socketpair()
                control.settimeout(5)
                candidate.settimeout(5)
                barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    authority = subprocess.Popen(
                        [sys.executable, __file__, "multi_transition", str(control_peer.fileno()),
                         str(candidate_peer.fileno()), str(barrier), state, "0", str(parent_handle)],
                        pass_fds=(control_peer.fileno(), candidate_peer.fileno(), barrier, parent_handle),
                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                    )
                finally:
                    control_peer.close()
                    candidate_peer.close()
                    os.close(barrier)
                candidate.sendall(b"W")
                self.assertEqual(candidate.recv(1), b"N")
                control.sendall(b"D")
                self.assertEqual(control.recv(1), b"B")
                parent.stdin.write("commit\n")
                parent.stdin.flush()
                self.assertTrue(select.select([child_handle], [], [], 5)[0])
                with sqlite3.connect(database) as db:
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
                control.sendall(b"D")
                self.assertEqual(control.recv(1), b"B")
            finally:
                if child_handle is not None:
                    if not select.select([child_handle], [], [], 0)[0]:
                        signal.pidfd_send_signal(child_handle, signal.SIGKILL)
                    os.close(child_handle)
                for sock in (control, control_peer, candidate, candidate_peer):
                    if sock is not None:
                        sock.close()
                if authority is not None:
                    if authority.poll() is None:
                        authority.kill()
                    authority.communicate(timeout=5)
                if parent.poll() is None:
                    parent.kill()
                parent.communicate(timeout=5)
                os.close(parent_handle)
                os.close(anchor)

    @unittest.skipUnless(hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"),
                         "Linux pidfd APIs are required")
    def test_registry_restart_rejects_unfinished_descendant_and_partial_records(self):
        with tempfile.TemporaryDirectory() as directory:
            registry = os.path.join(directory, "registry.db")
            database = os.path.join(directory, "synthetic.db")
            with sqlite3.connect(registry) as db:
                db.execute("PRAGMA journal_mode=WAL")
                db.execute("CREATE TABLE epoch(state TEXT NOT NULL)")
                db.execute("CREATE TABLE writers(token TEXT PRIMARY KEY, state TEXT NOT NULL)")
                db.execute("INSERT INTO epoch VALUES ('unsealed')")
                db.execute("INSERT INTO writers VALUES ('parent', 'intent')")
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            child_handle = None
            processes = []

            def guard():
                result = subprocess.run(
                    [sys.executable, __file__, "registry_guard", registry],
                    capture_output=True, text=True, timeout=5, check=True,
                )
                return result.stdout.strip()

            try:
                self.assertEqual(guard(), "CLOSED")
                parent = subprocess.Popen(
                    [sys.executable, __file__, "spawn_descendant", database],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                processes.append(parent)
                announcement = line(parent.stdout).split()
                self.assertEqual(announcement[0], "CHILD")
                child_handle = os.pidfd_open(int(announcement[1]))
                self.assertEqual(parent.wait(timeout=5), 0)
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE writers SET state='running' WHERE token='parent'")
                crasher = subprocess.Popen(
                    [sys.executable, __file__, "registry_crash", registry],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                processes.append(crasher)
                self.assertEqual(line(crasher.stdout), "UNCOMMITTED")
                crasher.kill()
                crasher.wait(timeout=5)
                with sqlite3.connect(registry) as db:
                    self.assertEqual(db.execute("SELECT state FROM writers WHERE token='parent'").fetchone(),
                                     ("running",))
                self.assertEqual(guard(), "CLOSED")

                parent.stdin.write("begin\n")
                parent.stdin.flush()
                self.assertEqual(line(parent.stdout), "DESCENDANT_WRITING")
                # Parent is gone, but the supervisor has not proved the process
                # set complete; registering the parent as done cannot seal it.
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE writers SET state='done' WHERE token='parent'")
                self.assertEqual(guard(), "CLOSED")
                # A forged or premature seal is not proof that the descendant
                # was discovered: show that this fixture would open unsafely.
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE epoch SET state='sealed'")
                self.assertEqual(guard(), "OPEN")
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE epoch SET state='unsealed'")
                    db.execute("INSERT INTO writers VALUES ('child', 'running')")
                    # Artificially seal to exercise the independent running check.
                    db.execute("UPDATE epoch SET state='sealed'")
                self.assertEqual(guard(), "CLOSED")
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE writers SET state='invalid' WHERE token='child'")
                self.assertEqual(guard(), "CLOSED")
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE writers SET state='running' WHERE token='child'")
                parent.stdin.write("commit\n")
                parent.stdin.flush()
                self.assertTrue(select.select([child_handle], [], [], 5)[0])
                with sqlite3.connect(database) as db:
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
                # Only the test supervisor asserts completion after pidfd signals.
                with sqlite3.connect(registry) as db:
                    db.execute("UPDATE writers SET state='done' WHERE token='child'")
                self.assertEqual(guard(), "OPEN")
            finally:
                if child_handle is not None:
                    if not select.select([child_handle], [], [], 0)[0]:
                        signal.pidfd_send_signal(child_handle, signal.SIGKILL)
                    os.close(child_handle)
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

    @unittest.skipUnless(hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"),
                         "Linux pidfd APIs are required")
    def test_pending_launch_refuses_incomplete_inventory_after_launcher_death(self):
        for mode in ("before_sync", "after_spawn"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                state_path = os.path.join(directory, "inventory-state")
                database = os.path.join(directory, "synthetic.db")
                with open(state_path, "wb") as state:
                    state.write(b"clean\n")
                    state.flush()
                    os.fsync(state.fileno())
                with sqlite3.connect(database) as db:
                    db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
                client, peer = socket.socketpair()
                client.settimeout(5)
                launcher = subprocess.Popen(
                    [sys.executable, __file__, "inventory_launcher", str(peer.fileno()), directory, mode],
                    pass_fds=(peer.fileno(),), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, text=True,
                )
                peer.close()
                writer_handle = None
                try:
                    if mode == "before_sync":
                        self.assertEqual(line(launcher.stdout), "NO_LAUNCH")
                        self.assertEqual(launcher.wait(timeout=5), 0)
                        self.assertEqual(client.recv(1), b"")
                    else:
                        self.assertEqual(line(launcher.stdout), "INTENT_SYNCED")
                        launcher.stdin.write("spawn\n")
                        launcher.stdin.flush()
                        announcement = line(launcher.stdout).split()
                        self.assertEqual(announcement[0], "SPAWNED")
                        writer_handle = os.pidfd_open(int(announcement[1]))
                        self.assertEqual(client.recv(1), b"T")
                        launcher.kill()
                        launcher.wait(timeout=5)
                    # No registration or safe completion was ever recorded.
                    with open(state_path, "rb") as state:
                        self.assertEqual(state.read(), b"launching\n")
                    state_fd = os.open(state_path, os.O_RDONLY)
                    try:
                        guard = subprocess.run(
                            [sys.executable, __file__, "guard", str(state_fd)],
                            pass_fds=(state_fd,), capture_output=True, text=True,
                            timeout=5, check=True,
                        )
                    finally:
                        os.close(state_fd)
                    self.assertEqual(guard.stdout.strip(), "CLOSED")
                    if writer_handle is not None:
                        self.assertFalse(select.select([writer_handle], [], [], 0)[0])
                        client.sendall(b"C")
                        self.assertTrue(select.select([writer_handle], [], [], 5)[0])
                        with sqlite3.connect(database) as db:
                            self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
                        with open(state_path, "rb") as state:
                            self.assertEqual(state.read(), b"launching\n")
                        state_fd = os.open(state_path, os.O_RDONLY)
                        try:
                            guard = subprocess.run(
                                [sys.executable, __file__, "guard", str(state_fd)],
                                pass_fds=(state_fd,), capture_output=True, text=True,
                                timeout=5, check=True,
                            )
                        finally:
                            os.close(state_fd)
                        self.assertEqual(guard.stdout.strip(), "CLOSED")
                finally:
                    client.close()
                    if launcher.poll() is None:
                        launcher.kill()
                    launcher.communicate(timeout=5)
                    if writer_handle is not None:
                        if not select.select([writer_handle], [], [], 0)[0]:
                            signal.pidfd_send_signal(writer_handle, signal.SIGKILL)
                        os.close(writer_handle)

    @unittest.skipUnless(hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"),
                         "Linux pidfd APIs are required")
    def test_multiple_writers_and_unknown_descendant_fail_closed(self):
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
            handles = []
            sockets = []
            child_handle = None
            try:
                guard_fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    guard = subprocess.Popen(
                        [sys.executable, __file__, "independent_guard", str(guard_fd)],
                        pass_fds=(guard_fd,), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(guard_fd)
                processes.append(guard)
                self.assertEqual(line(guard.stdout), "GUARDED")
                writer_fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    writer = subprocess.Popen(
                        [sys.executable, __file__, "sqlite_writer", str(writer_fd), database],
                        pass_fds=(writer_fd,), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(writer_fd)
                processes.append(writer)
                handles.append(os.pidfd_open(writer.pid))
                self.assertEqual(line(writer.stdout), "TRANSACTION")
                parent = subprocess.Popen(
                    [sys.executable, __file__, "spawn_descendant", database],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                processes.append(parent)
                handles.append(os.pidfd_open(parent.pid))
                announcement = line(parent.stdout).split()
                self.assertEqual(announcement[0], "CHILD")
                child_handle = os.pidfd_open(int(announcement[1]))
                self.assertEqual(parent.wait(timeout=5), 0)
                guard.kill()
                guard.wait(timeout=5)
                writer.stdin.write("commit\n")
                writer.stdin.flush()
                self.assertEqual(writer.wait(timeout=5), 0)
                parent.stdin.write("begin\n")
                parent.stdin.flush()
                self.assertEqual(line(parent.stdout), "DESCENDANT_WRITING")

                def start_authority(complete, pidfds):
                    control, control_peer = socket.socketpair()
                    candidate, candidate_peer = socket.socketpair()
                    control.settimeout(5)
                    candidate.settimeout(5)
                    sockets.extend((control, candidate))
                    barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                    try:
                        process = subprocess.Popen(
                            [sys.executable, __file__, "multi_transition", str(control_peer.fileno()),
                             str(candidate_peer.fileno()), str(barrier), state, str(int(complete)),
                             *[str(handle) for handle in pidfds]],
                            pass_fds=(control_peer.fileno(), candidate_peer.fileno(), barrier, *pidfds),
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                        )
                    finally:
                        control_peer.close()
                        candidate_peer.close()
                        os.close(barrier)
                    processes.append(process)
                    return process, control, candidate

                incomplete, control, candidate = start_authority(False, handles)
                candidate.sendall(b"W")
                self.assertEqual(candidate.recv(1), b"N")
                control.sendall(b"D")
                self.assertEqual(control.recv(1), b"B")
                incomplete.kill()
                incomplete.wait(timeout=5)
                complete, control, candidate = start_authority(True, [*handles, child_handle])
                control.sendall(b"D")
                self.assertEqual(control.recv(1), b"B")
                parent.stdin.write("commit\n")
                parent.stdin.flush()
                self.assertTrue(select.select([child_handle], [], [], 5)[0])
                with sqlite3.connect(database) as db:
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 2)
                for sent, expected in ((b"D", b"D"), (b"P", b"P"), (b"O", b"O")):
                    control.sendall(sent)
                    self.assertEqual(control.recv(1), expected)
                candidate.sendall(b"W")
                self.assertEqual(candidate.recv(1), b"Y")
                control.close()
                self.assertEqual(complete.wait(timeout=5), 0)
            finally:
                if child_handle is not None:
                    if not select.select([child_handle], [], [], 0)[0]:
                        signal.pidfd_send_signal(child_handle, signal.SIGKILL)
                    os.close(child_handle)
                for sock in sockets:
                    sock.close()
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                for handle in handles:
                    os.close(handle)
                os.close(anchor)

    @unittest.skipUnless(hasattr(os, "pidfd_open"), "Linux pidfd_open is required")
    def test_pidfd_reconciliation_refuses_live_writer_after_supervisor_death(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            database = os.path.join(directory, "synthetic.db")
            state_path = os.path.join(directory, "state")
            with open(path, "wb"), open(state_path, "wb") as state:
                state.write(b"uncertain\n")
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            anchor = os.open(path, os.O_RDWR)
            sender, receiver = socket.socketpair()
            sockets = [sender, receiver]
            processes = []
            pidfd = None
            try:
                def start_with_fd(role):
                    fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                    try:
                        process = subprocess.Popen(
                            [sys.executable, __file__, role, str(fd)], pass_fds=(fd,),
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                        )
                    finally:
                        os.close(fd)
                    processes.append(process)
                    return process

                guard = start_with_fd("independent_guard")
                self.assertEqual(line(guard.stdout), "GUARDED")
                fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    authority = subprocess.Popen(
                        [sys.executable, __file__, "grant_authority", str(sender.fileno()), str(fd)],
                        pass_fds=(sender.fileno(), fd), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(fd)
                processes.append(authority)
                writer = subprocess.Popen(
                    [sys.executable, __file__, "early_writer", str(receiver.fileno()), database, "unlock"],
                    pass_fds=(receiver.fileno(),), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                processes.append(writer)
                pidfd = os.pidfd_open(writer.pid)
                sender.close()
                receiver.close()
                self.assertEqual(line(authority.stdout), "ACK")
                authority.kill()
                authority.wait(timeout=5)
                writer.stdin.write("start\n")
                writer.stdin.flush()
                self.assertEqual(line(writer.stdout), "RELEASED")
                guard.kill()
                guard.wait(timeout=5)

                # Prove the lock alone is insufficient, then release the probe.
                probe = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                os.close(probe)
                control, control_peer = socket.socketpair()
                candidate, candidate_peer = socket.socketpair()
                control.settimeout(5)
                candidate.settimeout(5)
                sockets.extend((control, candidate))
                barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    restarted = subprocess.Popen(
                        [sys.executable, __file__, "pidfd_transition", str(control_peer.fileno()),
                         str(candidate_peer.fileno()), str(barrier), state_path, str(pidfd)],
                        pass_fds=(control_peer.fileno(), candidate_peer.fileno(), barrier, pidfd),
                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                    )
                finally:
                    control_peer.close()
                    candidate_peer.close()
                    os.close(barrier)
                processes.append(restarted)
                candidate.sendall(b"W")
                self.assertEqual(candidate.recv(1), b"N")
                control.sendall(b"D")
                self.assertEqual(control.recv(1), b"B")
                writer.stdin.write("commit\n")
                writer.stdin.flush()
                self.assertEqual(writer.wait(timeout=5), 0)
                with sqlite3.connect(database) as db:
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
                for sent, expected in ((b"D", b"D"), (b"P", b"P"), (b"O", b"O")):
                    control.sendall(sent)
                    self.assertEqual(control.recv(1), expected)
                candidate.sendall(b"W")
                self.assertEqual(candidate.recv(1), b"Y")
                control.close()
                self.assertEqual(restarted.wait(timeout=5), 0)
            finally:
                for sock in sockets:
                    sock.close()
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                if pidfd is not None:
                    os.close(pidfd)
                os.close(anchor)

    def test_independent_supervisor_lock_is_lost_on_supervisor_death(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            database = os.path.join(directory, "synthetic.db")
            with open(path, "wb"):
                pass
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            anchor = os.open(path, os.O_RDWR)
            sender, receiver = socket.socketpair()
            processes = []
            try:
                guard_fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    guard = subprocess.Popen(
                        [sys.executable, __file__, "independent_guard", str(guard_fd)],
                        pass_fds=(guard_fd,), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(guard_fd)
                processes.append(guard)
                self.assertEqual(line(guard.stdout), "GUARDED")
                writer_lock = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    authority = subprocess.Popen(
                        [sys.executable, __file__, "grant_authority", str(sender.fileno()), str(writer_lock)],
                        pass_fds=(sender.fileno(), writer_lock), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(writer_lock)
                processes.append(authority)
                writer = subprocess.Popen(
                    [sys.executable, __file__, "early_writer", str(receiver.fileno()), database, "unlock"],
                    pass_fds=(receiver.fileno(),), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                processes.append(writer)
                sender.close()
                receiver.close()
                self.assertEqual(line(authority.stdout), "ACK")
                authority.kill()
                authority.wait(timeout=5)
                writer.stdin.write("start\n")
                writer.stdin.flush()
                self.assertEqual(line(writer.stdout), "RELEASED")
                probe = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                try:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    guard.kill()
                    guard.wait(timeout=5)
                    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    writer.stdin.write("commit\n")
                    writer.stdin.flush()
                    self.assertEqual(writer.wait(timeout=5), 0)
                finally:
                    os.close(probe)
                with sqlite3.connect(database) as db:
                    self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
            finally:
                sender.close()
                receiver.close()
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                os.close(anchor)

    def test_early_release_and_supervisor_reference_limits(self):
        cases = ((False, "close", False), (True, "close", True), (True, "unlock", False))
        for retain, action, blocked in cases:
            with self.subTest(retain=retain, action=action), tempfile.TemporaryDirectory() as directory:
                path = os.path.join(directory, "activity.lock")
                database = os.path.join(directory, "synthetic.db")
                with open(path, "wb"):
                    pass
                with sqlite3.connect(database) as db:
                    db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
                anchor = os.open(path, os.O_RDWR)
                barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                retained = os.dup(barrier) if retain else None
                sender, receiver = socket.socketpair()
                processes = []
                try:
                    authority = subprocess.Popen(
                        [sys.executable, __file__, "grant_authority", str(sender.fileno()), str(barrier)],
                        pass_fds=(sender.fileno(), barrier), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                    processes.append(authority)
                    os.close(barrier)
                    writer = subprocess.Popen(
                        [sys.executable, __file__, "early_writer", str(receiver.fileno()), database, action],
                        pass_fds=(receiver.fileno(),), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                    processes.append(writer)
                    sender.close()
                    receiver.close()
                    self.assertEqual(line(authority.stdout), "ACK")
                    authority.kill()
                    authority.wait(timeout=5)
                    writer.stdin.write("start\n")
                    writer.stdin.flush()
                    self.assertEqual(line(writer.stdout), "RELEASED")
                    probe = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                    try:
                        if blocked:
                            with self.assertRaises(BlockingIOError):
                                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        else:
                            fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        # Commit happens while the exclusive probe is still held
                        # in the unprotected cases, proving a false drain.
                        writer.stdin.write("commit\n")
                        writer.stdin.flush()
                        self.assertEqual(writer.wait(timeout=5), 0)
                        if retained is not None:
                            os.close(retained)
                            retained = None
                        fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    finally:
                        os.close(probe)
                    with sqlite3.connect(database) as db:
                        self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)
                finally:
                    sender.close()
                    receiver.close()
                    for process in processes:
                        if process.poll() is None:
                            process.kill()
                        process.communicate(timeout=5)
                    if retained is not None:
                        os.close(retained)
                    os.close(anchor)

    def test_transferred_writer_lock_survives_disconnect_and_authority_death(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            database = os.path.join(directory, "synthetic.db")
            with open(path, "wb"):
                pass
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
            anchor = os.open(path, os.O_RDWR)
            authority_socket, writer_socket = socket.socketpair()
            processes = []

            def cleanup():
                authority_socket.close()
                writer_socket.close()
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                os.close(anchor)

            self.addCleanup(cleanup)
            barrier = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            try:
                authority = subprocess.Popen(
                    [sys.executable, __file__, "grant_authority", str(authority_socket.fileno()), str(barrier)],
                    pass_fds=(authority_socket.fileno(), barrier), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
            finally:
                os.close(barrier)
            processes.append(authority)
            writer = subprocess.Popen(
                [sys.executable, __file__, "transferred_writer", str(writer_socket.fileno()), database],
                pass_fds=(writer_socket.fileno(),), stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            processes.append(writer)
            authority_socket.close()
            writer_socket.close()
            self.assertEqual(line(writer.stdout), "ADMITTED")
            self.assertEqual(line(authority.stdout), "ACK")
            writer.stdin.write("disconnect\n")
            writer.stdin.flush()
            self.assertEqual(line(writer.stdout), "DISCONNECTED")
            authority.kill()
            authority.wait(timeout=5)
            probe = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            try:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                writer.stdin.write("commit\n")
                writer.stdin.flush()
                self.assertEqual(writer.wait(timeout=5), 0)
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                os.close(probe)
            with sqlite3.connect(database) as db:
                self.assertEqual(db.execute("SELECT COUNT(*) FROM writes").fetchone()[0], 1)

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
    if len(sys.argv) == 3 and sys.argv[1] == "registry_guard":
        registry_guard(sys.argv[2])
    elif len(sys.argv) == 3 and sys.argv[1] == "registry_crash":
        registry_crash(sys.argv[2])
    elif len(sys.argv) == 5 and sys.argv[1] == "inventory_launcher":
        inventory_launcher(int(sys.argv[2]), sys.argv[3], sys.argv[4])
    elif len(sys.argv) == 4 and sys.argv[1] == "inventory_writer":
        inventory_writer(int(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) >= 7 and sys.argv[1] == "multi_transition":
        transition_authority(int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5],
                             [int(value) for value in sys.argv[7:]], sys.argv[6] == "1")
    elif len(sys.argv) == 3 and sys.argv[1] == "spawn_descendant":
        spawn_descendant(sys.argv[2])
    elif len(sys.argv) == 3 and sys.argv[1] == "spawn_detached":
        spawn_descendant(sys.argv[2], detached=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "descendant":
        descendant_writer(sys.argv[2])
    elif len(sys.argv) == 7 and sys.argv[1] == "pidfd_transition":
        transition_authority(int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5], int(sys.argv[6]))
    elif len(sys.argv) == 5 and sys.argv[1] == "early_writer":
        early_release_writer(int(sys.argv[2]), sys.argv[3], sys.argv[4])
    elif len(sys.argv) == 4 and sys.argv[1] == "grant_authority":
        grant_authority(int(sys.argv[2]), int(sys.argv[3]))
    elif len(sys.argv) == 4 and sys.argv[1] == "transferred_writer":
        transferred_sqlite_writer(int(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) == 5 and sys.argv[1] == "admitted_sqlite_writer":
        admitted_sqlite_writer(int(sys.argv[2]), int(sys.argv[3]), sys.argv[4])
    elif len(sys.argv) == 4 and sys.argv[1] == "sqlite_writer":
        sqlite_writer(int(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) == 6 and sys.argv[1] == "transition":
        transition_authority(int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5])
    elif len(sys.argv) == 3 and sys.argv[1] == "contender":
        transition_contender(int(sys.argv[2]))
    elif len(sys.argv) == 5 and sys.argv[1] == "publisher":
        publisher(int(sys.argv[2]), sys.argv[3], sys.argv[4])
    elif len(sys.argv) == 3 and sys.argv[1] in ("writer", "authority", "keeper", "guard", "restart", "independent_guard"):
        child(sys.argv[1], int(sys.argv[2]))
    else:
        unittest.main(verbosity=2)
